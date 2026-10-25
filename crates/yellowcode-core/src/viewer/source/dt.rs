use mdconf::ConfigurationExt;
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use dtfile::elements::{RawEvent, RawReader, Source};
use ibschema::values::ColumnValue;
use mdconf::{Configuration, EntryMeta, EntrySource, LoadOptions};

use crate::Result;

const ENTRY_BYTES: u64 = 64 * 1024 * 1024;
const CONFIG_BYTES: u64 = 512 * 1024 * 1024;
const ENTRY_COUNT: usize = 32768;
const PREFIX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const EXTRACT_TIME: Duration = Duration::from_secs(120);

struct Counted<R> {
    inner: R,
    bytes: Rc<Cell<u64>>,
}
impl<R: Read> Read for Counted<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(out)?;
        let next = self
            .bytes
            .get()
            .checked_add(n as u64)
            .ok_or_else(|| io::Error::other("DT compressed byte counter overflow"))?;
        self.bytes.set(next);
        Ok(n)
    }
}

struct Guarded<'a, R, F> {
    inner: R,
    bytes: u64,
    limit: u64,
    deadline: Instant,
    progress: &'a mut F,
    cancelled: Rc<RefCell<Option<crate::Error>>>,
}
impl<R: Read, F: FnMut(&'static str) -> Result<()>> Read for Guarded<'_, R, F> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        if Instant::now() >= self.deadline {
            return Err(io::Error::other("DT metadata extraction time limit"));
        }
        if let Err(e) = (self.progress)("dt-config") {
            *self.cancelled.borrow_mut() = Some(e);
            return Err(io::Error::other("DT metadata extraction cancelled"));
        }
        let remaining = self
            .limit
            .checked_sub(self.bytes)
            .filter(|n| *n > 0)
            .ok_or_else(|| io::Error::other("DT metadata inflated prefix limit"))?;
        let len = out
            .len()
            .min(usize::try_from(remaining).unwrap_or(usize::MAX));
        let slice = out
            .get_mut(..len)
            .ok_or_else(|| io::Error::other("DT read range"))?;
        let n = self.inner.read(slice)?;
        self.bytes += n as u64;
        Ok(n)
    }
}

struct Item {
    meta: EntryMeta,
    offset: u64,
    len: u64,
}
pub(super) struct Spool {
    file: RefCell<tempfile::NamedTempFile>,
    items: Vec<Item>,
    names: HashSet<String>,
    bytes: u64,
}
impl Spool {
    fn new() -> Result<Self> {
        Ok(Self {
            file: RefCell::new(tempfile::NamedTempFile::new()?),
            items: Vec::new(),
            names: HashSet::new(),
            bytes: 0,
        })
    }
    fn next_extent(&self, len: u64) -> Result<u64> {
        if len > ENTRY_BYTES {
            return Err("DT CONFIG entry exceeds product limit".into());
        }
        if self.items.len() >= ENTRY_COUNT {
            return Err("DT CONFIG entry count exceeds product limit".into());
        }
        self.bytes
            .checked_add(len)
            .filter(|n| *n <= CONFIG_BYTES)
            .ok_or_else(|| "DT CONFIG stored aggregate exceeds product limit".into())
    }
    fn push(&mut self, row: &[brace::binary::BinNode], offset: u64) -> Result<()> {
        let columns = dtfile::layout::folder();
        let values = dtfile::rows::decode_row(&columns, row, offset)?;
        for (column, value) in columns.iter().zip(&values) {
            column
                .check(value)
                .map_err(|e| format!("DT CONFIG {}: {e}", column.name))?;
        }
        let [
            ColumnValue::Str(name),
            ColumnValue::DateTime(created),
            ColumnValue::DateTime(modified),
            ColumnValue::Number(attrs),
            ColumnValue::Number(size),
            ColumnValue::Bytes(data),
        ] = values.as_slice()
        else {
            return Err("DT CONFIG row has missing or invalid fields".into());
        };
        if name.is_empty() || name.contains('\0') {
            return Err("DT CONFIG invalid entry name".into());
        }
        let declared = size
            .to_string()
            .parse::<u64>()
            .map_err(|_| "DT CONFIG invalid DATASIZE")?;
        let attrs = attrs
            .to_string()
            .parse::<u32>()
            .map_err(|_| "DT CONFIG invalid ATTRIBUTES")?;
        let len = u64::try_from(data.len())?;
        if declared != len {
            return Err("DT CONFIG DATASIZE differs from binary length".into());
        }
        let next = self.next_extent(len)?;
        if !self.names.insert(name.to_ascii_lowercase()) {
            return Err("DT CONFIG duplicate entry name".into());
        }
        self.file.get_mut().as_file_mut().write_all(data)?;
        self.items.push(Item {
            meta: EntryMeta {
                name: name.clone(),
                ctime: infobase::config::v8time_of(created),
                mtime: infobase::config::v8time_of(modified),
                attrs,
            },
            offset: self.bytes,
            len,
        });
        self.bytes = next;
        Ok(())
    }
}
impl EntrySource for Spool {
    fn list(&self) -> mdconf::Result<Vec<EntryMeta>> {
        Ok(self.items.iter().map(|i| i.meta.clone()).collect())
    }
    fn stored(&self, name: &str) -> mdconf::Result<Option<Vec<u8>>> {
        self.stored_bounded(name, ENTRY_BYTES as usize)
    }
    fn stored_bounded(&self, name: &str, limit: usize) -> mdconf::Result<Option<Vec<u8>>> {
        let Some(item) = self.items.iter().find(|i| i.meta.name == name) else {
            return Ok(None);
        };
        let bad = |what: &str| mdconf::Error::Format {
            entry: name.into(),
            what: what.into(),
        };
        if item.len > limit as u64 {
            return Err(bad("stored size exceeds product limit"));
        }
        let len =
            usize::try_from(item.len).map_err(|_| bad("stored size exceeds address space"))?;
        let mut out = Vec::new();
        out.try_reserve_exact(len)
            .map_err(|_| bad("stored allocation failed"))?;
        out.resize(len, 0);
        let mut file = self.file.borrow_mut();
        file.as_file_mut().seek(SeekFrom::Start(item.offset))?;
        file.as_file_mut().read_exact(&mut out)?;
        Ok(Some(out))
    }
}

pub(super) fn open(
    path: &Path,
    opts: &LoadOptions,
    progress: &mut impl FnMut(&'static str) -> Result<()>,
) -> Result<Configuration> {
    let spool = extract(path, progress)?;
    progress("metadata")?;
    Ok(Configuration::load(&spool, opts)?)
}

pub(super) fn extract(
    path: &Path,
    progress: &mut impl FnMut(&'static str) -> Result<()>,
) -> Result<Spool> {
    let file = File::open(path)?;
    let before = file.metadata()?;
    let compressed = Rc::new(Cell::new(0));
    let cancelled = Rc::new(RefCell::new(None));
    let deadline = Instant::now() + EXTRACT_TIME;
    let shared_progress = RefCell::new(progress);
    let mut compressed_progress = |stage| (shared_progress.borrow_mut())(stage);
    let mut inflated_progress = |stage| (shared_progress.borrow_mut())(stage);
    let compressed_guard = Guarded {
        inner: file,
        bytes: 0,
        limit: before.len(),
        deadline,
        progress: &mut compressed_progress,
        cancelled: cancelled.clone(),
    };
    let mut input = Counted {
        inner: compressed_guard,
        bytes: compressed.clone(),
    };
    let header = match dtfile::container::read_header(&mut input) {
        Ok(header) => header,
        Err(error) => {
            if let Some(error) = cancelled.borrow_mut().take() {
                return Err(error);
            }
            return Err(error.into());
        }
    };
    if header.ev {
        return Err("EV DT metadata opening unsupported".into());
    }
    let guarded = Guarded {
        inner: dtfile::container::inflate_reader(input)?,
        bytes: 0,
        limit: PREFIX_BYTES,
        deadline,
        progress: &mut inflated_progress,
        cancelled: cancelled.clone(),
    };
    let mut reader = RawReader::from_payload(guarded, header);
    reader.set_read_limits(brace::binary::ReadLimits {
        atom_bytes: ENTRY_BYTES,
        retained_bytes: ENTRY_BYTES + 1024 * 1024,
        nodes: 65536,
    });
    let mut spool = Spool::new()?;
    let mut selected = false;
    loop {
        if Instant::now() >= deadline {
            return Err("DT metadata extraction time limit".into());
        }
        (shared_progress.borrow_mut())("dt-config")?;
        let event = match reader.next() {
            Ok(event) => event,
            Err(error) => {
                if let Some(error) = cancelled.borrow_mut().take() {
                    return Err(error);
                }
                return Err(error.into());
            }
        };
        match event {
            Some(RawEvent::TableStart {
                source: Source::Folder(name),
                ..
            }) => {
                selected = name.eq_ignore_ascii_case("CONFIG");
            }
            Some(RawEvent::Row(row)) if selected => spool.push(&row, reader.offset())?,
            Some(RawEvent::TableEnd) if selected => break,
            None => {
                return Err(
                    "DT has no complete CONFIG folder (CONFIGSAVE is not a substitute)".into(),
                );
            }
            _ => {}
        }
    }
    drop(reader);
    let after = std::fs::metadata(path)?;
    if before.len() != after.len() || before.modified()? != after.modified()? {
        return Err("DT input changed during metadata extraction".into());
    }
    Ok(spool)
}
