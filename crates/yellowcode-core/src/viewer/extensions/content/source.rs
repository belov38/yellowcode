use super::super::dt;
use super::{Code, CopyKind, STORED, check, code};
use brace::{Guid, binary::BinNode};
use infobase::{config::ManagedConfigTxnSource, extensions::Control};
use mdconf::{EntryBacking, StoredBytes, configinfo::ConfigInfo};
use memory_budget::{BudgetedVec, Category};
use sha1::{Digest, Sha1};
use std::{
    cell::RefCell,
    io::{self, Write},
    path::Path,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Key {
    bytes: [u8; 74],
    len: usize,
    digest: Option<[u8; 20]>,
}
impl Key {
    pub(super) fn digest(hash: [u8; 20]) -> Self {
        let mut bytes = [0; 74];
        let hex = |digit| {
            if digit < 10 {
                b'0' + digit
            } else {
                b'a' + (digit - 10)
            }
        };
        for (chunk, byte) in bytes.as_chunks_mut::<2>().0.iter_mut().zip(hash) {
            *chunk = [hex(byte >> 4), hex(byte & 15)];
        }
        Self {
            bytes,
            len: 40,
            digest: Some(hash),
        }
    }
    pub(super) fn saved(id: Guid, name: impl std::fmt::Display) -> Result<Self, Code> {
        let mut bytes = [0; 74];
        let mut output = io::Cursor::new(&mut bytes[..]);
        write!(output, "{id}__{name}").map_err(|_| Code::CorruptContent)?;
        let len = output.position() as usize;
        bytes
            .get_mut(..len)
            .ok_or(Code::CorruptContent)?
            .make_ascii_lowercase();
        Ok(Self {
            bytes,
            len,
            digest: None,
        })
    }
    fn matches(self, name: &str) -> bool {
        self.bytes
            .get(..self.len)
            .is_some_and(|bytes| bytes.eq_ignore_ascii_case(name.as_bytes()))
    }
    fn verify(self, bytes: &[u8]) -> Result<(), Code> {
        if let Some(expected) = self.digest
            && <[u8; 20]>::from(Sha1::digest(bytes)) != expected
        {
            return Err(Code::CorruptContent);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Physical {
    pub(super) copy: CopyKind,
    pub(super) key: Key,
}
struct Mapping {
    id: Guid,
    source: Option<Physical>,
}

pub(super) struct EntryMap(BudgetedVec<Mapping>);
impl EntryMap {
    pub(super) fn new(
        manifest: &ConfigInfo,
        extension: Guid,
        copy: CopyKind,
        requested: impl IntoIterator<Item = Guid>,
        control: &Control,
    ) -> Result<Self, Code> {
        let mut map = BudgetedVec::<Mapping>::new(control.budget(), Category::Metadata)
            .map_err(|_| Code::MemoryLimit)?;
        for id in requested {
            check(control)?;
            if map.as_slice().iter().any(|entry| entry.id == id) {
                continue;
            }
            let source = if copy == CopyKind::Saved {
                Some(Physical {
                    copy,
                    key: Key::saved(extension, id)?,
                })
            } else {
                None
            };
            map.try_push(Mapping { id, source })
                .map_err(|_| Code::MemoryLimit)?;
        }
        map.as_mut_slice().sort_unstable_by_key(|entry| entry.id);
        if copy == CopyKind::Cas {
            for entry in &manifest.entries {
                check(control)?;
                let Some(id) = Guid::parse(&entry.name) else {
                    continue;
                };
                let Ok(index) = map
                    .as_slice()
                    .binary_search_by_key(&id, |mapping| mapping.id)
                else {
                    continue;
                };
                let mapping = map
                    .as_mut_slice()
                    .get_mut(index)
                    .ok_or(Code::CorruptContent)?;
                if mapping
                    .source
                    .replace(Physical {
                        copy,
                        key: Key::digest(entry.stored_sha1),
                    })
                    .is_some()
                {
                    return Err(Code::CorruptContent);
                }
            }
        }
        Ok(Self(map))
    }

    pub(super) fn get(&self, id: Guid) -> Option<Physical> {
        let at = self
            .0
            .as_slice()
            .binary_search_by_key(&id, |entry| entry.id)
            .ok()?;
        self.0.as_slice().get(at)?.source
    }
}

pub(super) struct Record {
    source: Physical,
    capture: bool,
    contents: Option<Found>,
}
enum Found {
    Present,
    Bytes(StoredBytes),
    Invalid,
    Oversize(u64),
}

pub(super) struct BaseFolders<'a> {
    txn: &'a onecd::ReadTxn,
    cas: RefCell<Option<ManagedConfigTxnSource<'a>>>,
    saved: RefCell<Option<ManagedConfigTxnSource<'a>>>,
}
impl<'a> BaseFolders<'a> {
    pub(super) fn new(txn: &'a onecd::ReadTxn) -> Self {
        Self {
            txn,
            cas: RefCell::new(None),
            saved: RefCell::new(None),
        }
    }
    fn folder<T>(
        &self,
        kind: CopyKind,
        control: &Control,
        read: impl FnOnce(&ManagedConfigTxnSource<'a>) -> Result<T, Code>,
    ) -> Result<T, Code> {
        let (slot, name) = match kind {
            CopyKind::Cas => (&self.cas, "CONFIGCAS"),
            CopyKind::Saved => (&self.saved, "CONFIGCASSAVE"),
        };
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            let checkpoint = || {
                control
                    .slot()
                    .map_err(|_| infobase::Error::UnsupportedStatic("extension read stopped"))
            };
            let budget = self.txn.memory_budget().ok_or(Code::MemoryLimit)?;
            let index =
                ManagedConfigTxnSource::read_in_checked(self.txn, name, budget, &checkpoint);
            check(control)?;
            *slot = Some(index.map_err(onecd_error)?);
        }
        read(slot.as_ref().ok_or(Code::Unavailable)?)
    }
}

pub(super) enum Volume<'a> {
    OneCd(&'a BaseFolders<'a>),
    Dt {
        path: &'a Path,
        work: dt::Work,
        records: RefCell<Option<BudgetedVec<Record>>>,
    },
}
impl Volume<'_> {
    pub(super) fn probe(
        &self,
        control: &Control,
        choice: CopyKind,
        generation: Option<Key>,
        saved_key: Key,
    ) -> Result<bool, Code> {
        match self {
            Self::OneCd(base) => match base.folder(CopyKind::Saved, control, |saved| {
                Ok(find_name(saved, saved_key, control)?.is_some())
            }) {
                Err(Code::CorruptContent) if choice == CopyKind::Cas => Ok(false),
                other => other,
            },
            Self::Dt {
                path,
                work,
                records,
            } => {
                let mut requests =
                    BudgetedVec::try_with_capacity(control.budget(), Category::Metadata, 2)
                        .map_err(|_| Code::MemoryLimit)?;
                requests
                    .try_push(Record {
                        source: Physical {
                            copy: CopyKind::Saved,
                            key: saved_key,
                        },
                        capture: choice == CopyKind::Saved,
                        contents: None,
                    })
                    .map_err(|_| Code::MemoryLimit)?;
                if choice == CopyKind::Cas
                    && let Some(key) = generation
                {
                    requests
                        .try_push(Record {
                            source: Physical {
                                copy: CopyKind::Cas,
                                key,
                            },
                            capture: true,
                            contents: None,
                        })
                        .map_err(|_| Code::MemoryLimit)?;
                }
                read_dt(path, work, control, requests.as_mut_slice())?;
                let have_saved = requests
                    .as_slice()
                    .first()
                    .is_some_and(|r| r.contents.is_some());
                *records.borrow_mut() = Some(requests);
                Ok(have_saved)
            }
        }
    }

    pub(super) fn prepare(&self, control: &Control, map: &EntryMap) -> Result<(), Code> {
        if let Self::Dt {
            path,
            work,
            records,
        } = self
        {
            records.take();
            let mut requests = BudgetedVec::<Record>::try_with_capacity(
                control.budget(),
                Category::Metadata,
                map.0.len(),
            )
            .map_err(|_| Code::MemoryLimit)?;
            for source in map.0.as_slice().iter().filter_map(|entry| entry.source) {
                check(control)?;
                if !requests.as_slice().iter().any(|r| r.source == source) {
                    requests
                        .try_push(Record {
                            source,
                            capture: true,
                            contents: None,
                        })
                        .map_err(|_| Code::MemoryLimit)?;
                }
            }
            if !requests.is_empty() {
                read_dt(path, work, control, requests.as_mut_slice())?;
            }
            *records.borrow_mut() = Some(requests);
        }
        Ok(())
    }

    pub(super) fn len(&self, control: &Control, entry: Physical) -> Result<Option<u64>, Code> {
        check(control)?;
        match self {
            Self::OneCd(base) => base.folder(entry.copy, control, |source| {
                let Some(name) = find_name(source, entry.key, control)? else {
                    return Ok(None);
                };
                source
                    .indexed_len(name)
                    .map(|size| size.map(|size| size as u64))
                    .map_err(onecd_error)
            }),
            Self::Dt { records, .. } => {
                Ok(match Self::find_record(records.borrow().as_ref(), entry) {
                    Some(Found::Bytes(bytes)) => Some(bytes.len() as u64),
                    Some(Found::Oversize(length)) => Some(*length),
                    Some(Found::Invalid) => return Err(Code::CorruptContent),
                    Some(Found::Present) => return Err(Code::Unavailable),
                    None => None,
                })
            }
        }
    }

    fn find_record(records: Option<&BudgetedVec<Record>>, entry: Physical) -> Option<&Found> {
        records?
            .as_slice()
            .iter()
            .find(|record| record.source == entry)?
            .contents
            .as_ref()
    }

    pub(super) fn fetch(
        &self,
        control: &Control,
        entry: Physical,
        limit: usize,
    ) -> Result<Option<StoredBytes>, Code> {
        check(control)?;
        let data = match self {
            Self::OneCd(base) => base.folder(entry.copy, control, |source| {
                let Some(name) = find_name(source, entry.key, control)? else {
                    return Ok(None);
                };
                let checkpoint = || {
                    control
                        .check()
                        .map_err(|_| infobase::Error::UnsupportedStatic("extension read stopped"))
                };
                let bytes = source.stored_bounded_checked(name, limit, &checkpoint);
                check(control)?;
                bytes
                    .map_err(onecd_error)?
                    .map(EntryBacking::from_charged)
                    .transpose()
                    .map_err(|_| Code::MemoryLimit)
            })?,
            Self::Dt { records, .. } => match Self::find_record(records.borrow().as_ref(), entry) {
                Some(Found::Bytes(bytes)) if bytes.capacity() <= limit => Some(bytes.clone()),
                Some(_) => return Err(Code::WorkLimit),
                None => None,
            },
        };
        if let Some(bytes) = &data {
            entry.key.verify(bytes)?;
        }
        Ok(data)
    }
}

fn onecd_error(error: infobase::Error) -> Code {
    match error {
        infobase::Error::Memory(_) | infobase::Error::Engine(onecd::Error::Memory(_)) => {
            Code::MemoryLimit
        }
        infobase::Error::Engine(onecd::Error::Io(_)) => Code::Unavailable,
        _ => Code::CorruptContent,
    }
}

fn find_name<'a>(
    folder: &'a ManagedConfigTxnSource<'_>,
    key: Key,
    control: &Control,
) -> Result<Option<&'a str>, Code> {
    let mut result = None;
    for entry in folder.list() {
        check(control)?;
        if key.matches(&entry.name) && result.replace(entry.name.as_str()).is_some() {
            return Err(Code::CorruptContent);
        }
    }
    Ok(result)
}

fn read_dt(
    path: &Path,
    work: &dt::Work,
    control: &Control,
    requests: &mut [Record],
) -> Result<(), Code> {
    if !read_dt_pass(path, work, control, requests, false)? {
        for record in requests.iter_mut() {
            record.contents = None;
        }
        read_dt_pass(path, work, control, requests, true)?;
    }
    Ok(())
}

fn read_dt_pass(
    path: &Path,
    work: &dt::Work,
    control: &Control,
    requests: &mut [Record],
    required_only: bool,
) -> Result<bool, Code> {
    let mut memory_error = false;
    let mut optional_row_error = false;
    let read = dt::folders(path, control, work, &mut |folder, reader| {
        let kind = if folder.eq_ignore_ascii_case("ConfigCAS") {
            CopyKind::Cas
        } else {
            CopyKind::Saved
        };
        let required = requests.iter().any(|r| r.source.copy == kind && r.capture);
        if !required && (required_only || !requests.iter().any(|r| r.source.copy == kind)) {
            reader.skip_table_rows(dt::DISCARD_NODES, &mut || {
                control
                    .check()
                    .map_err(|_| dtfile::Error::Io(io::ErrorKind::Interrupted.into()))
            })?;
            return Ok(());
        }
        let result = read_dt_rows(reader, kind, control, requests, &mut memory_error);
        optional_row_error = !required
            && result.as_ref().is_err_and(|error| {
                matches!(
                    error,
                    dtfile::Error::Dt { .. } | dtfile::Error::Limit { .. }
                ) || matches!(error, dtfile::Error::Io(e) if e.kind() == io::ErrorKind::InvalidData)
            });
        result
    });
    if memory_error {
        return Err(Code::MemoryLimit);
    }
    match read {
        Ok(()) => Ok(true),
        Err(
            infobase::extensions::Error::Invalid(_)
            | infobase::extensions::Error::Io(io::ErrorKind::InvalidData),
        ) => {
            if optional_row_error {
                Ok(false)
            } else {
                Err(Code::CorruptContent)
            }
        }
        Err(infobase::extensions::Error::Limit(_)) if optional_row_error => Ok(false),
        Err(other) => Err(code(other)),
    }
}

fn read_dt_rows(
    reader: &mut dt::Payload<'_>,
    kind: CopyKind,
    control: &Control,
    requests: &mut [Record],
    memory_error: &mut bool,
) -> dtfile::Result<()> {
    while let Some(fields) = reader.folder_header()? {
        let invalid = || io::Error::from(io::ErrorKind::InvalidData);
        let [_, BinNode::Atom(name), _, _, _, _, _, _, _, size] = fields.as_slice() else {
            return Err(invalid().into());
        };
        let name = name.as_str().ok_or_else(invalid)?;
        let Some(record) = requests
            .iter_mut()
            .find(|record| record.source.copy == kind && record.source.key.matches(&name))
        else {
            reader.copy_folder_binary_with_work_limit(&mut io::sink(), u64::MAX, 128)?;
            continue;
        };
        if !record.capture {
            reader.copy_folder_binary_with_work_limit(&mut io::sink(), u64::MAX, 128)?;
            record.contents = Some(Found::Present);
            continue;
        }
        let declared = match size {
            BinNode::Atom(size) => size
                .as_int()
                .and_then(|value| u64::try_from(value).ok())
                .or_else(|| {
                    size.as_numeric_text()
                        .and_then(|value| value.parse::<u64>().ok())
                }),
            _ => None,
        };
        if record.contents.is_some() || declared.is_none() {
            reader.copy_folder_binary_with_work_limit(&mut io::sink(), u64::MAX, 128)?;
            record.contents = Some(Found::Invalid);
            continue;
        }
        let declared = declared.ok_or_else(invalid)?;
        if declared > STORED as u64 {
            let copied =
                reader.copy_folder_binary_with_work_limit(&mut io::sink(), u64::MAX, 128)?;
            record.contents = Some(if copied == declared {
                Found::Oversize(declared)
            } else {
                Found::Invalid
            });
            continue;
        }
        let out_of_memory = || io::Error::from(io::ErrorKind::OutOfMemory);
        let mut bytes =
            BudgetedVec::try_with_capacity(control.budget(), Category::Metadata, declared as usize)
                .map_err(|_| {
                    *memory_error = true;
                    out_of_memory()
                })?;
        let mut sink = StoredSink {
            bytes: &mut bytes,
            overflow: false,
        };
        let copied = reader.copy_folder_binary_with_work_limit(&mut sink, u64::MAX, 128);
        if copied.as_ref().is_err_and(
            |error| matches!(error, dtfile::Error::Io(e) if e.kind() == io::ErrorKind::OutOfMemory),
        ) {
            *memory_error = true;
        }
        record.contents = Some(if copied? == declared {
            Found::Bytes(
                EntryBacking::from_charged(bytes.into_charged_vec()).map_err(|_| {
                    *memory_error = true;
                    out_of_memory()
                })?,
            )
        } else {
            Found::Invalid
        });
    }
    Ok(())
}

struct StoredSink<'a> {
    bytes: &'a mut BudgetedVec<u8>,
    overflow: bool,
}

impl Write for StoredSink<'_> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.overflow |= self.bytes.len().saturating_add(data.len()) > self.bytes.capacity();
        if !self.overflow {
            self.bytes
                .try_extend_from_slice(data)
                .map_err(|_| io::Error::from(io::ErrorKind::OutOfMemory))?;
        }
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
