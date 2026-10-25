use crate::error::{Error, Result};
use crate::format::{FileHeader, Format, PageHeader, entry_header_bytes, parse_entry_header};
use crate::time::V8Time;
use std::io::Write;

pub const FORMAT16_PLACEHOLDER: &[u8] = include_bytes!("../data/format16_placeholder.bin");

const MIN_DATA_PAGE: u64 = 0x200;
const F15_MAX: u64 = 0x7FFF_FFFE;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntrySpec {
    pub name: String,
    pub ctime: V8Time,
    pub mtime: V8Time,
    pub attrs: u32,
    pub data_len: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageVer {
    EntryCount,
    Fixed(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteOptions {
    pub format: Format,
    pub toc_page_min: u64,
    pub storage_ver: StorageVer,
}

impl WriteOptions {
    pub fn top_level(format: Format) -> Self {
        WriteOptions {
            format,
            toc_page_min: format.default_toc_page(),
            storage_ver: StorageVer::EntryCount,
        }
    }
    pub fn nested() -> Self {
        WriteOptions {
            format: Format::F15,
            toc_page_min: 0,
            storage_ver: StorageVer::Fixed(1),
        }
    }
}

pub trait DataSource {
    fn write_data(&mut self, index: usize, out: &mut dyn Write) -> Result<()>;
}

pub struct VecData<'a>(pub &'a [Vec<u8>]);

impl DataSource for VecData<'_> {
    fn write_data(&mut self, index: usize, out: &mut dyn Write) -> Result<()> {
        let d = self
            .0
            .get(index)
            .ok_or_else(|| Error::LayoutMismatch(format!("no data for entry {index}")))?;
        out.write_all(d)?;
        Ok(())
    }
}

struct Counter<'a> {
    out: &'a mut dyn Write,
    n: u64,
}

impl Write for Counter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let k = self.out.write(buf)?;
        self.n = self.n.saturating_add(k as u64);
        Ok(k)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.out.flush()
    }
}

fn zeros(out: &mut dyn Write, mut n: u64) -> std::io::Result<()> {
    const Z: [u8; 4096] = [0; 4096];
    while n > 0 {
        let k = usize::try_from(n).unwrap_or(usize::MAX).min(Z.len());
        out.write_all(Z.get(..k).unwrap_or(&Z))?;
        n -= k as u64;
    }
    Ok(())
}

fn overflow() -> Error {
    Error::LayoutMismatch("layout size overflows u64".into())
}

fn add(a: u64, b: u64) -> Result<u64> {
    a.checked_add(b).ok_or_else(overflow)
}

fn check_f15(f: Format, v: u64) -> Result<u64> {
    if f == Format::F15 && v > F15_MAX {
        return Err(Error::TooLarge {
            what: "Format15 address",
            size: v,
            limit: F15_MAX,
        });
    }
    Ok(v)
}

fn page_header(f: Format, data_size: u64, page_size: u64) -> Result<Vec<u8>> {
    PageHeader {
        data_size,
        page_size,
        next_page: f.undefined(),
    }
    .encode(f)
    .ok_or_else(|| Error::TooLarge {
        what: "Format15 address",
        size: data_size.max(page_size),
        limit: F15_MAX,
    })
}

struct Planned {
    header: Vec<u8>,
    header_addr: u64,
    data_addr: Option<u64>,
}

pub fn write_container(
    out: &mut dyn Write,
    specs: &[EntrySpec],
    data: &mut dyn DataSource,
    opts: &WriteOptions,
) -> Result<u64> {
    let f = opts.format;
    let n = u64::try_from(specs.len()).map_err(|_| overflow())?;
    let ph = f.page_header_size();
    let toc_size = n.checked_mul(f.toc_record_size()).ok_or_else(overflow)?;
    let toc_page = toc_size.max(opts.toc_page_min);

    let toc_addr = f.file_header_size();
    let mut pos = if n == 0 {
        toc_addr
    } else {
        add(add(toc_addr, ph)?, toc_page)?
    };
    check_f15(f, toc_size)?;
    check_f15(f, toc_page)?;
    let mut plan = Vec::with_capacity(specs.len());
    for (i, s) in specs.iter().enumerate() {
        let header = entry_header_bytes(s.ctime, s.mtime, s.attrs, &s.name);
        if parse_entry_header(&header).is_none_or(|(_, _, _, name)| name != s.name) {
            return Err(Error::LayoutMismatch(format!(
                "entry {i} ({:?}): the name does not read back",
                s.name
            )));
        }
        let hlen = u64::try_from(header.len()).map_err(|_| overflow())?;
        let header_addr = check_f15(f, pos)?;
        pos = add(add(pos, ph)?, hlen)?;
        let data_addr = match s.data_len {
            Some(len) => {
                let a = check_f15(f, pos)?;
                let page = len.max(MIN_DATA_PAGE);
                check_f15(f, page)?;
                pos = add(add(pos, ph)?, page)?;
                Some(a)
            }
            None => None,
        };
        plan.push(Planned {
            header,
            header_addr,
            data_addr,
        });
    }
    check_f15(f, pos.saturating_sub(1))?;

    let mut w = Counter { out, n: 0 };
    if f == Format::F16 {
        w.write_all(FORMAT16_PLACEHOLDER)?;
    }
    let storage_ver = match opts.storage_ver {
        StorageVer::EntryCount => u32::try_from(n).map_err(|_| overflow())?,
        StorageVer::Fixed(v) => v,
    };
    let fh = FileHeader {
        next_free_page: f.undefined(),
        page_size: MIN_DATA_PAGE as u32,
        storage_ver,
        reserved: 0,
    };
    w.write_all(&fh.encode(f))?;
    if n == 0 {
        return Ok(w.n);
    }

    w.write_all(&page_header(f, toc_size, toc_page)?)?;
    let a = usize::try_from(f.addr_size()).map_err(|_| overflow())?;
    let mut toc = Vec::new();
    for p in &plan {
        for v in [
            p.header_addr,
            p.data_addr.unwrap_or(f.undefined()),
            f.undefined(),
        ] {
            toc.extend_from_slice(v.to_le_bytes().get(..a).unwrap_or_default());
        }
    }
    w.write_all(&toc)?;
    zeros(&mut w, toc_page - toc_size)?;

    for (i, (s, p)) in specs.iter().zip(&plan).enumerate() {
        let hlen = p.header.len() as u64;
        w.write_all(&page_header(f, hlen, hlen)?)?;
        w.write_all(&p.header)?;
        if let Some(len) = s.data_len {
            let page = len.max(MIN_DATA_PAGE);
            w.write_all(&page_header(f, len, page)?)?;
            let before = w.n;
            data.write_data(i, &mut w)?;
            let written = w.n - before;
            if written != len {
                return Err(Error::LayoutMismatch(format!(
                    "entry {} ({}): data source wrote {written} bytes, declared {len}",
                    i, s.name
                )));
            }
            zeros(&mut w, page - len)?;
        }
    }
    Ok(w.n)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewEntry {
    pub name: String,
    pub ctime: V8Time,
    pub mtime: V8Time,
    pub attrs: u32,
    pub data: Option<Vec<u8>>,
}

pub fn build_container(entries: &[NewEntry], opts: &WriteOptions) -> Result<Vec<u8>> {
    let specs: Vec<EntrySpec> = entries
        .iter()
        .map(|e| EntrySpec {
            name: e.name.clone(),
            ctime: e.ctime,
            mtime: e.mtime,
            attrs: e.attrs,
            data_len: e.data.as_ref().map(|d| d.len() as u64),
        })
        .collect();
    let datas: Vec<Vec<u8>> = entries
        .iter()
        .map(|e| e.data.clone().unwrap_or_default())
        .collect();
    let mut out = Vec::new();
    write_container(&mut out, &specs, &mut VecData(&datas), opts)?;
    Ok(out)
}
