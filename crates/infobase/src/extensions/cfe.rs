use std::io::Write;
use std::sync::Arc;
use v8cont::{Container, DataSource, EntrySpec, Format, V8Time, WriteOptions};

pub const MAX_BYTES: u64 = 64 << 20;
pub const MAX_KEPT_BYTES: u64 = 256 << 20;

#[derive(Clone, Copy, Debug)]
pub struct CfeEntry<'a> {
    pub name: &'a str,
    pub ctime: V8Time,
    pub mtime: V8Time,
    pub attrs: u32,
    pub data: &'a [u8],
}

const PAGE: u64 = 0x200;
const PAGE_HEADER: u64 = 31;
const TOC_RECORD: u64 = 12;
const ENTRY_HEADER: u64 = 24;
const FILE_HEADER: u64 = 16;

fn observed(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_digit() || b.is_ascii_lowercase() || b == b'.' || b == b'-')
}

pub fn size(configinfo: &CfeEntry<'_>, manifest: &[CfeEntry<'_>]) -> Option<u64> {
    let mut entries = manifest.iter().chain(std::iter::once(configinfo));
    let n = u64::try_from(manifest.len()).ok()?.checked_add(1)?;
    let toc = n.checked_mul(TOC_RECORD)?.max(PAGE);
    entries.try_fold(FILE_HEADER + PAGE_HEADER + toc, |at, e| {
        let name = u64::try_from(e.name.encode_utf16().count())
            .ok()?
            .checked_mul(2)?;
        let data = u64::try_from(e.data.len()).ok()?.max(PAGE);
        at.checked_add(2 * PAGE_HEADER + ENTRY_HEADER)?
            .checked_add(name)?
            .checked_add(data)
    })
}

pub fn within(total: u64, len: u64, limit: u64) -> Option<u64> {
    total.checked_add(len).filter(|&next| next <= limit)
}

pub fn keep(
    kept: &mut u64,
    data: Result<Arc<[u8]>, String>,
    limit: u64,
) -> Result<Arc<[u8]>, String> {
    let bytes = data?;
    let next = within(*kept, bytes.len() as u64, limit)
        .ok_or_else(|| format!("the data of all extensions: more than {limit} bytes"))?;
    *kept = next;
    Ok(bytes)
}

pub fn check_names<'a>(manifest: impl IntoIterator<Item = &'a str>) -> Result<(), String> {
    let mut previous: Option<&str> = None;
    for name in manifest {
        if !observed(name) {
            return Err(format!(
                "the entry {name:?}: a name outside [0-9a-z.-], its place in the table of contents is not observed"
            ));
        }
        if name == "configinfo" {
            return Err(format!("the entry {name:?} is repeated"));
        }
        match previous {
            Some(p) if p == name => return Err(format!("the entry {name:?} is repeated")),
            Some(p) if p > name => {
                return Err(format!(
                    "the manifest is not sorted by name ({p:?} before {name:?}): the order of its pages is not observed"
                ));
            }
            _ => {}
        }
        previous = Some(name);
    }
    Ok(())
}

struct Slices<'a>(Vec<&'a [u8]>);

impl DataSource for Slices<'_> {
    fn write_data(&mut self, index: usize, out: &mut dyn Write) -> v8cont::Result<()> {
        let data = self
            .0
            .get(index)
            .ok_or_else(|| v8cont::Error::LayoutMismatch(format!("no data for entry {index}")))?;
        out.write_all(data)?;
        Ok(())
    }
}

pub fn container(
    configinfo: &CfeEntry<'_>,
    manifest: &[CfeEntry<'_>],
    limit: u64,
) -> Result<Vec<u8>, String> {
    if configinfo.name != "configinfo" {
        return Err(format!(
            "the entry {:?} in place of configinfo",
            configinfo.name
        ));
    }
    check_names(manifest.iter().map(|e| e.name))?;
    let physical: Vec<&CfeEntry<'_>> = manifest.iter().chain(std::iter::once(configinfo)).collect();
    if let Some(e) = physical.iter().find(|e| e.attrs != 0) {
        return Err(format!(
            "the entry {:?}: the attributes {} of its row are not observed",
            e.name, e.attrs
        ));
    }
    if size(configinfo, manifest).is_none_or(|size| size > limit) {
        return Err(format!("more than {limit} bytes"));
    }
    let mut order: Vec<usize> = (0..physical.len()).collect();
    let name = |i: &usize| physical.get(*i).map_or("", |e| e.name);
    order.sort_by(|a, b| name(a).cmp(name(b)));
    let specs: Vec<EntrySpec> = physical
        .iter()
        .map(|e| EntrySpec {
            name: e.name.to_string(),
            ctime: e.ctime,
            mtime: e.mtime,
            attrs: 0,
            data_len: Some(e.data.len() as u64),
        })
        .collect();
    let data: Vec<&[u8]> = physical.iter().map(|e| e.data).collect();
    let fail = |e: v8cont::Error| format!("the container: {e}");
    let mut first = Vec::new();
    v8cont::write_container(
        &mut first,
        &specs,
        &mut Slices(data.clone()),
        &WriteOptions::top_level(Format::F15),
    )
    .map_err(fail)?;
    let captured = Container::open(first.as_slice())
        .and_then(|c| c.capture_layout())
        .map_err(fail)?;
    drop(first);
    if !captured.residue.is_empty() {
        return Err("the container: the layout of the fresh save does not replay".into());
    }
    let mut layout = captured.layout;
    let mut entries = Vec::with_capacity(order.len());
    let mut sorted_specs = Vec::with_capacity(order.len());
    let mut sorted_data = Vec::with_capacity(order.len());
    for &i in &order {
        let (Some(entry), Some(spec), Some(bytes)) =
            (layout.entries.get(i), specs.get(i), data.get(i))
        else {
            return Err("the container: an entry vanished".into());
        };
        entries.push(entry.clone());
        sorted_specs.push(spec.clone());
        sorted_data.push(*bytes);
    }
    layout.entries = entries;
    let mut out = Vec::with_capacity(usize::try_from(layout.file_len).unwrap_or_default());
    v8cont::write_exact(
        &mut out,
        &layout,
        &[],
        None,
        &sorted_specs,
        &mut Slices(sorted_data),
    )
    .map_err(fail)?;
    Ok(out)
}
