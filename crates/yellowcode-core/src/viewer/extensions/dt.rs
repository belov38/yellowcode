use super::stamp::Stamp;
use dtfile::{
    container::{DtVersion, inflate_reader, read_header},
    elements::{RawEvent, RawReader, Source},
};
use infobase::extensions::{Control, Error, Inventory, Projection, Result};
use memory_budget::{BudgetError, Category};
use std::{
    cell::Cell,
    fs::File,
    io::{self, Read},
    path::Path,
    rc::Rc,
};

const COMPRESSED: u64 = 8 << 30;
const INFLATED: u64 = 32 << 30;
pub(super) const DISCARD_NODES: u64 = 68_719_476_736;
const SCHEMA_RETAINED: u64 = 4 << 20;
const SCHEMA_NODES: u64 = 65_536;

pub(super) struct Capped<'a, R> {
    inner: R,
    control: Control,
    read: u64,
    limit: u64,
    reason: &'static str,
    stop: Rc<Cell<Option<&'static str>>>,
    total: Option<&'a Cell<u64>>,
}
impl<R: Read> Read for Capped<'_, R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.control
            .check()
            .map_err(|_| io::Error::from(io::ErrorKind::Other))?;
        if out.is_empty() {
            return Ok(0);
        }
        let remaining = self.limit.saturating_sub(self.read);
        if remaining == 0 {
            let mut probe = [0];
            let result = self.inner.read(&mut probe);
            self.control
                .check()
                .map_err(|_| io::Error::from(io::ErrorKind::Other))?;
            return match result? {
                0 => Ok(0),
                _ => {
                    self.stop.set(Some(self.reason));
                    Err(io::ErrorKind::Other.into())
                }
            };
        }
        let take = out
            .len()
            .min(64 << 10)
            .min(usize::try_from(remaining).unwrap_or(usize::MAX));
        let result = self
            .inner
            .read(out.get_mut(..take).ok_or(io::ErrorKind::InvalidInput)?);
        self.control
            .check()
            .map_err(|_| io::Error::from(io::ErrorKind::Other))?;
        let count = result?;
        self.read = self
            .read
            .checked_add(count as u64)
            .ok_or(io::ErrorKind::Other)?;
        if let Some(total) = &self.total {
            total.set(self.read);
        }
        Ok(count)
    }
}
fn translate(control: &Control, stop: &Cell<Option<&'static str>>, error: dtfile::Error) -> Error {
    if let Err(e) = control.check() {
        return Error::Stopped(e);
    }
    if let Some(reason) = stop.get() {
        return Error::Limit(reason);
    }
    match error {
        dtfile::Error::Limit { what, .. } => Error::Limit(what),
        dtfile::Error::Io(e) => Error::Io(e.kind()),
        _ => Error::Invalid("DT framing"),
    }
}
fn working_bytes() -> Result<u64> {
    let node = std::mem::size_of::<brace::binary::BinNode>() as u64;
    let atom = std::mem::size_of::<brace::binary::Atom>() as u64;
    let vector = std::mem::size_of::<Vec<brace::binary::BinNode>>() as u64;
    SCHEMA_RETAINED
        .checked_mul(3)
        .and_then(|n| {
            n.checked_add(SCHEMA_NODES.checked_mul(node.checked_mul(6)?.checked_add(atom)?)?)
        })
        .and_then(|n| n.checked_add(6u64.checked_mul(128)?.checked_mul(vector)?))
        .and_then(|n| n.checked_add((64 << 10) + (1 << 20)))
        .ok_or(BudgetError::Overflow.into())
}
pub fn scan_dt(path: &Path, control: &Control) -> Result<Inventory> {
    scan(path, control, COMPRESSED, INFLATED, None)
}
pub(super) fn scan_bound(path: &Path, control: &Control, stamp: Stamp) -> Result<Inventory> {
    scan(path, control, COMPRESSED, INFLATED, Some(&Work::new(stamp)))
}
fn scan(
    path: &Path,
    control: &Control,
    compressed: u64,
    inflated: u64,
    work: Option<&Work>,
) -> Result<Inventory> {
    walk(path, control, compressed, inflated, work, None, None)?
        .ok_or(Error::Invalid("missing inventory"))
}

pub(super) type Payload<'a> = RawReader<Capped<'a, zraw::InflateReader<Capped<'a, File>>>>;
type FolderVisitor<'a> = dyn FnMut(&str, &mut Payload<'_>) -> dtfile::Result<()> + 'a;

#[derive(Default)]
pub(super) struct Work {
    stamp: Option<Stamp>,
    compressed: Cell<u64>,
    inflated: Cell<u64>,
}

impl Work {
    pub(super) fn new(stamp: Stamp) -> Self {
        Self {
            stamp: Some(stamp),
            ..Self::default()
        }
    }
}

struct Locator {
    id: brace::Guid,
    generation: Option<Result<Option<[u8; 20]>>>,
}

pub(super) fn selected_generation(
    path: &Path,
    control: &Control,
    work: &Work,
    id: brace::Guid,
) -> Result<Result<Option<[u8; 20]>>> {
    let mut selected = Locator {
        id,
        generation: None,
    };
    walk(
        path,
        control,
        COMPRESSED,
        INFLATED,
        Some(work),
        Some(&mut selected),
        None,
    )?;
    selected
        .generation
        .ok_or(Error::Invalid("selected extension is not installed"))
}

pub(super) fn folders(
    path: &Path,
    control: &Control,
    work: &Work,
    visitor: &mut FolderVisitor<'_>,
) -> Result<()> {
    walk(
        path,
        control,
        COMPRESSED,
        INFLATED,
        Some(work),
        None,
        Some(visitor),
    )
    .map(|_| ())
}

fn walk(
    path: &Path,
    control: &Control,
    compressed: u64,
    inflated: u64,
    work: Option<&Work>,
    mut generation: Option<&mut Locator>,
    mut folders: Option<&mut FolderVisitor<'_>>,
) -> Result<Option<Inventory>> {
    control.check()?;
    let _working = control
        .budget()
        .try_reserve(Category::ReaderWorking, working_bytes()?)
        .map_err(BudgetError::from)?;
    let file = {
        let _path = infobase::extensions::path_open_scratch(control.budget(), path)?;
        File::open(path).map_err(|e| Error::Io(e.kind()))?
    };
    control.check()?;
    let stamp = Stamp::file(&file)?;
    if work
        .and_then(|work| work.stamp)
        .is_some_and(|expected| stamp != expected)
    {
        return Err(Error::Changed);
    }
    if file.metadata().map_err(|e| Error::Io(e.kind()))?.len() > compressed {
        return Err(Error::Limit("compressed DT bytes"));
    }
    let stop = Rc::new(Cell::new(None));
    let mut input = Capped {
        inner: file,
        control: control.clone(),
        read: work.map_or(0, |w| w.compressed.get()),
        total: work.map(|w| &w.compressed),
        limit: compressed,
        reason: "compressed DT bytes",
        stop: stop.clone(),
    };
    let header = read_header(&mut input).map_err(|e| translate(control, &stop, e))?;
    if header.version != DtVersion::F3 || header.ev {
        return Err(Error::Unsupported("F3 inventory required"));
    }
    let inflater = inflate_reader(input).map_err(|e| translate(control, &stop, e))?;
    let input = Capped {
        inner: inflater,
        control: control.clone(),
        read: work.map_or(0, |w| w.inflated.get()),
        total: work.map(|w| &w.inflated),
        limit: inflated,
        reason: "inflated DT bytes",
        stop: stop.clone(),
    };
    let mut reader = RawReader::from_payload(input, header);
    reader
        .set_depth_limit(128)
        .map_err(|e| translate(control, &stop, e))?;
    let mut result = Inventory::new(control.budget())?;
    let mut projection: Option<Projection> = None;
    let mut ordinal = 0usize;
    let mut main = false;
    let mut complete = false;
    let mut selected: Option<bool> = None;
    let mut seen = [false; 2];
    loop {
        control.check()?;
        reader.set_read_limits(if selected.is_some() {
            brace::binary::ReadLimits {
                atom_bytes: 1 << 20,
                retained_bytes: 1 << 20,
                nodes: 4096,
            }
        } else {
            brace::binary::ReadLimits {
                atom_bytes: SCHEMA_RETAINED,
                retained_bytes: SCHEMA_RETAINED,
                nodes: SCHEMA_NODES,
            }
        });
        let Some(event) = reader.next().map_err(|e| translate(control, &stop, e))? else {
            break;
        };
        match event {
            RawEvent::DataBaseStart(root) => {
                if folders.is_some() {
                    if Stamp::capture(path, control.budget())? != stamp {
                        return Err(Error::Changed);
                    }
                    return Ok(None);
                }
                if projection.is_some() {
                    return Err(Error::Invalid("duplicate main database"));
                }
                projection = Some(Projection::from_binary(&root, control)?);
                main = true;
            }
            RawEvent::TableStart {
                source,
                ordinal: found,
            } => {
                selected = None;
                if (generation.is_some() || folders.is_some())
                    && let Source::Folder(name) = &source
                    && let Some(index) = ["ConfigCAS", "ConfigCASSave"]
                        .iter()
                        .position(|kind| name.eq_ignore_ascii_case(kind))
                {
                    let seen = seen
                        .get_mut(index)
                        .ok_or(Error::Invalid("extension folder index"))?;
                    if *seen || main || complete {
                        return Err(Error::Unsupported(
                            "extension folders outside the supported DT order",
                        ));
                    }
                    *seen = true;
                    if let Some(visit) = &mut folders {
                        reader.set_read_limits(brace::binary::ReadLimits {
                            atom_bytes: 1024,
                            retained_bytes: 4096,
                            nodes: 32,
                        });
                        visit(name, &mut reader).map_err(|e| translate(control, &stop, e))?;
                        continue;
                    }
                }
                if source == Source::DataBase {
                    let p = projection.ok_or(Error::Invalid("missing main schema"))?;
                    if !main || found != ordinal || ordinal >= p.tables {
                        return Err(Error::Invalid("main table order/count"));
                    }
                    ordinal = ordinal.checked_add(1).ok_or(BudgetError::Overflow)?;
                    if found == p.info_ordinal {
                        selected = Some(false);
                    } else if found == p.ngs_ordinal {
                        selected = Some(true);
                    }
                }
                if selected.is_none() {
                    reader
                        .skip_table_rows(DISCARD_NODES, &mut || {
                            control
                                .check()
                                .map_err(|_| dtfile::Error::Io(io::ErrorKind::Other.into()))
                        })
                        .map_err(|e| translate(control, &stop, e))?;
                }
            }
            RawEvent::Row(row) => match selected {
                Some(false) => {
                    result.push_dt_row(&row)?;
                    if let Some(locator) = generation.as_deref_mut() {
                        match infobase::extensions::dt_generation(
                            &row,
                            locator.id,
                            control.budget(),
                        ) {
                            Ok(None) => {}
                            Ok(Some(key)) => locator.generation = Some(Ok(key)),
                            Err(error) => locator.generation = Some(Err(error)),
                        }
                    }
                }
                Some(true) => return Err(Error::Unsupported("pending next-generation inventory")),
                None => return Err(Error::Invalid("unselected retained row")),
            },
            RawEvent::TableEnd => selected = None,
            RawEvent::ExtensionsStart => {
                if projection.is_none_or(|p| ordinal != p.tables) {
                    return Err(Error::Invalid("missing main data lists"));
                }
            }
            RawEvent::DataBaseEnd => {
                if !main || projection.is_none_or(|p| ordinal != p.tables) {
                    return Err(Error::Invalid("incomplete main database"));
                }
                main = false;
                complete = true;
            }
            _ => {}
        }
    }
    if !complete || main {
        return Err(Error::Invalid("no complete main database"));
    }
    control.check()?;
    if Stamp::capture(path, control.budget())? != stamp {
        return Err(Error::Changed);
    }
    control.check()?;
    result.finish();
    control.check()?;
    Ok(Some(result))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use memory_budget::{Limits, MemoryBudget};
    use std::io::Write;
    fn control() -> Control {
        Control::new(&MemoryBudget::new(Limits::new(64 << 20))).unwrap()
    }
    #[test]
    fn byte_caps_are_aggregate_and_exact_eof_is_admitted() {
        for (bytes, limit, ok) in [
            (b"abc".as_slice(), 3, true),
            (b"abcd".as_slice(), 3, false),
            (b"".as_slice(), 0, true),
        ] {
            let stop = Rc::new(Cell::new(None));
            let mut r = Capped {
                inner: bytes,
                control: control(),
                read: 0,
                total: None,
                limit,
                reason: "bytes",
                stop: stop.clone(),
            };
            let mut out = Vec::new();
            let mut byte = [0];
            let result = (|| -> io::Result<()> {
                loop {
                    let n = r.read(&mut byte)?;
                    if n == 0 {
                        break;
                    }
                    out.extend_from_slice(&byte[..n]);
                }
                Ok(())
            })();
            assert_eq!(result.is_ok(), ok);
            assert_eq!(out, &bytes[..usize::try_from(limit).unwrap()]);
            assert_eq!(stop.get(), (!ok).then_some("bytes"));
        }
        struct ChunkWitness;
        impl Read for ChunkWitness {
            fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                assert!(out.len() <= 65536);
                out.fill(7);
                Ok(out.len())
            }
        }
        let mut r = Capped {
            inner: ChunkWitness,
            control: control(),
            read: 0,
            total: None,
            limit: 65537,
            reason: "bytes",
            stop: Rc::new(Cell::new(None)),
        };
        assert_eq!(r.read(&mut [0; 65538]).unwrap(), 65536);
        assert_eq!(r.read(&mut [0; 2]).unwrap(), 1);
        assert!(r.read(&mut [0; 1]).is_err());
    }
    #[test]
    fn cancelled_reader_never_dispatches_another_read() {
        struct NoRead;
        impl Read for NoRead {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                panic!("read after cancellation")
            }
        }
        let c = control();
        c.cancel();
        let mut r = Capped {
            inner: NoRead,
            control: c,
            read: 0,
            total: None,
            limit: 10,
            reason: "bytes",
            stop: Rc::new(Cell::new(None)),
        };
        assert!(r.read(&mut [0]).is_err());
    }
    fn empty_dt() -> (tempfile::NamedTempFile, u64) {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base");
        infobase::create::create_empty(&base, &Default::default()).unwrap();
        let text = infobase::raw::RawIb::open(&base.join("1Cv8.1CD"))
            .unwrap()
            .dbschema_text()
            .unwrap();
        let mut schema = ibschema::logical::LogicalSchema::from_text(&text).unwrap();
        schema
            .tables
            .retain(|t| matches!(t.name.as_str(), "ExtensionsInfo" | "ExtensionsInfoNGS"));
        let mut w = dtfile::DtWriter::new(
            Vec::new(),
            dtfile::container::DtHeader {
                version: DtVersion::F3,
                ev: false,
            },
        )
        .unwrap();
        w.begin_database(&schema).unwrap();
        for t in dtfile::TableHead::database(&schema).unwrap() {
            w.begin_table(&t).unwrap();
            w.end_table().unwrap();
        }
        w.end_database().unwrap();
        let bytes = w.finish().unwrap();
        let mut input = bytes.as_slice();
        read_header(&mut input).unwrap();
        let inflated = io::copy(&mut inflate_reader(input).unwrap(), &mut io::sink()).unwrap();
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&bytes).unwrap();
        (file, inflated)
    }
    #[test]
    fn an_extension_folder_after_the_main_database_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base");
        infobase::create::create_empty(&base, &Default::default()).unwrap();
        let text = infobase::raw::RawIb::open(&base.join("1Cv8.1CD"))
            .unwrap()
            .dbschema_text()
            .unwrap();
        let mut schema = ibschema::logical::LogicalSchema::from_text(&text).unwrap();
        schema
            .tables
            .retain(|t| matches!(t.name.as_str(), "ExtensionsInfo" | "ExtensionsInfoNGS"));
        let mut w = dtfile::DtWriter::new(
            Vec::new(),
            dtfile::container::DtHeader {
                version: DtVersion::F3,
                ev: false,
            },
        )
        .unwrap();
        w.begin_database(&schema).unwrap();
        for t in dtfile::TableHead::database(&schema).unwrap() {
            w.begin_table(&t).unwrap();
            w.end_table().unwrap();
        }
        w.end_database().unwrap();
        let folder = dtfile::elements::Source::Folder("ConfigCAS".into());
        w.begin_table(&dtfile::TableHead::system(&folder).unwrap())
            .unwrap();
        w.end_table().unwrap();
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&w.finish().unwrap()).unwrap();
        let control = control();
        let work = Work::new(Stamp::file(file.as_file()).unwrap());
        assert!(matches!(
            selected_generation(file.path(), &control, &work, brace::Guid::NIL),
            Err(Error::Unsupported(
                "extension folders outside the supported DT order"
            )),
        ));
    }

    #[test]
    fn a_content_pass_checks_the_actual_opened_file_against_the_snapshot() {
        let (source, _) = empty_dt();
        let (other, _) = empty_dt();
        let expected = Stamp::file(source.as_file()).unwrap();
        let work = Work::new(expected);
        let control = control();
        assert!(matches!(
            folders(other.path(), &control, &work, &mut |_, _| unreachable!()),
            Err(Error::Changed),
        ));
        assert_eq!(work.compressed.get(), 0);
        assert_eq!(work.inflated.get(), 0);
    }
    #[test]
    fn content_passes_share_one_byte_allowance() {
        let (file, inflated) = empty_dt();
        let compressed = file.as_file().metadata().unwrap().len();
        let work = Work::default();
        let control = control();
        walk(
            file.path(),
            &control,
            compressed * 2 - 1,
            inflated * 2,
            Some(&work),
            None,
            None,
        )
        .unwrap();
        assert_eq!(work.compressed.get(), compressed);
        assert_eq!(work.inflated.get(), inflated);
        assert!(matches!(
            walk(
                file.path(),
                &control,
                compressed * 2 - 1,
                inflated * 2,
                Some(&work),
                None,
                None
            ),
            Err(Error::Limit("compressed DT bytes"))
        ));
        assert_eq!(work.compressed.get(), compressed * 2 - 1);
    }
    #[test]
    fn full_scan_byte_caps_include_all_framing_and_flags_at_eof() {
        let (file, bytes) = empty_dt();
        let compressed = file.as_file().metadata().unwrap().len();
        assert!(
            scan(file.path(), &control(), compressed, bytes, None)
                .unwrap()
                .is_empty()
        );
        assert!(matches!(
            scan(file.path(), &control(), compressed - 1, bytes, None),
            Err(Error::Limit("compressed DT bytes"))
        ));
        assert!(matches!(
            scan(file.path(), &control(), compressed, bytes - 1, None),
            Err(Error::Limit("inflated DT bytes"))
        ));
        let raw = std::fs::read(file.path()).unwrap();
        let mut input = raw.as_slice();
        let h = read_header(&mut input).unwrap();
        let mut payload = Vec::new();
        inflate_reader(input)
            .unwrap()
            .read_to_end(&mut payload)
            .unwrap();
        payload.extend(std::iter::repeat_n(0x80, 70_000));
        let mut raw = Vec::new();
        dtfile::container::write_header(&mut raw, h).unwrap();
        let mut w = dtfile::container::deflate_writer(raw).unwrap();
        w.write_all(&payload).unwrap();
        std::fs::write(file.path(), w.finish().unwrap()).unwrap();
        assert!(
            scan(file.path(), &control(), COMPRESSED, bytes + 70_000, None)
                .unwrap()
                .is_empty()
        );
        assert!(matches!(
            scan(file.path(), &control(), COMPRESSED, bytes + 69_999, None),
            Err(Error::Limit("inflated DT bytes"))
        ));
    }
}
