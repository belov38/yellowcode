mod source;

use super::{
    controller::{Code, code},
    stamp::Stamp,
};
use brace::Guid;
use infobase::extensions::{Control, ReadSource};
use mdconf::{
    EntryMeta, EntrySource, StoredBytes,
    extension_header::{HeaderError, InstalledHeader, ObjectBelonging},
    extension_objects::{InventoryError, ObjectReadError, ObjectReference, ReferencePosition},
    extension_reader::{ExtensionEntry, ExtensionReadError, ExtensionRoot},
};
use memory_budget::{BudgetedVec, BudgetedWriter, Category, MemoryBudget};
use serde::{Deserialize, Serialize, Serializer};
use source::{BaseFolders, EntryMap, Key, Physical, Volume};
use std::{cell::RefCell, io, path::Path};

pub(super) const ENTRIES: usize = 2048;
pub(super) const STORED: usize = 1 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CopyKind {
    Cas,
    Saved,
}

#[derive(Clone, Copy)]
pub(super) struct Selection {
    pub id: Guid,
    pub copy: Option<CopyKind>,
}

pub(super) struct Options {
    pub stamp: Stamp,
    pub cache: usize,
    pub limit: u64,
    pub selection: Selection,
}

pub(super) fn onecd(
    path: &Path,
    control: &Control,
    budget: &MemoryBudget,
    options: Options,
    scan: &impl Serialize,
) -> Result<BudgetedVec<u8>, Code> {
    let Options {
        stamp,
        cache,
        limit,
        selection,
    } = options;
    ReadSource::open_checked(path, control.clone(), cache, |file| stamp.check(file))
        .and_then(|source| {
            source.with_extension_snapshot(selection.id, |generation, txn| {
                let folders = BaseFolders::new(txn);
                inspect(
                    Volume::OneCd(&folders),
                    generation.map_err(source_error),
                    selection,
                    control,
                    budget,
                    limit,
                    scan,
                )
            })
        })
        .map_err(source_error)?
}

pub(super) fn dt(
    path: &Path,
    control: &Control,
    budget: &MemoryBudget,
    options: Options,
    scan: &impl Serialize,
) -> Result<BudgetedVec<u8>, Code> {
    let Options {
        stamp,
        limit,
        selection,
        ..
    } = options;
    let work = super::dt::Work::new(stamp);
    let generation = super::dt::selected_generation(path, control, &work, selection.id)
        .map_err(source_error)?
        .map_err(source_error);
    let volume = Volume::Dt {
        path,
        work,
        records: RefCell::new(None),
    };
    inspect(volume, generation, selection, control, budget, limit, scan)
}

fn source_error(error: infobase::extensions::Error) -> Code {
    use infobase::extensions::Error as E;
    match error {
        E::Unsupported(_) => Code::ContentLayout,
        E::Invalid("selected extension is not installed") => Code::Invalid,
        E::Invalid(_) => Code::CorruptContent,
        other => code(other),
    }
}

fn check(control: &Control) -> Result<(), Code> {
    control
        .check()
        .map_err(|stop| code(infobase::extensions::Error::Stopped(stop)))
}

fn inspect<'a>(
    volume: Volume<'a>,
    generation: Result<Option<[u8; 20]>, Code>,
    selection: Selection,
    control: &'a Control,
    budget: &MemoryBudget,
    limit: u64,
    scan: &impl Serialize,
) -> Result<BudgetedVec<u8>, Code> {
    let cas = generation.ok().flatten().map(Key::digest);
    let saved = Key::saved(selection.id, "configinfo")?;
    let have_cas = generation != Ok(None);
    let choice = selection.copy.unwrap_or(if have_cas {
        CopyKind::Cas
    } else {
        CopyKind::Saved
    });
    let have_saved = volume.probe(control, choice, cas, saved)?;
    let copies: &[CopyKind] = match (have_cas, have_saved) {
        (true, true) => &[CopyKind::Cas, CopyKind::Saved],
        (true, false) => &[CopyKind::Cas],
        (false, true) => &[CopyKind::Saved],
        (false, false) => return Err(Code::NoContent),
    };
    let manifest = match choice {
        CopyKind::Cas => generation.and_then(|key| key.map(Key::digest).ok_or(Code::NoContent)),
        CopyKind::Saved if have_saved => Ok(saved),
        CopyKind::Saved => Err(Code::NoContent),
    };
    let result = manifest.and_then(|manifest| {
        let view = View {
            volume,
            manifest,
            control,
            choice,
            id: selection.id,
            entries: RefCell::new(None),
            fault: RefCell::new(None),
        };
        decode(&view, copies, budget, limit, scan)
    });
    match result {
        Err(
            code @ (Code::NoContent | Code::ContentLayout | Code::CorruptContent | Code::WorkLimit),
        ) => {
            #[derive(Serialize)]
            struct Failure<'a, S> {
                version: u8,
                scan: &'a S,
                state: &'static str,
                code: Code,
                copy: CopyKind,
                copies: &'a [CopyKind],
            }
            let mut output = ContentOutput::new(budget, limit)?;
            output.value(&Failure {
                version: 1,
                scan,
                state: "failed",
                code,
                copy: choice,
                copies,
            })?;
            Ok(output.writer.into_buffer())
        }
        other => other,
    }
}

fn decode(
    view: &View<'_>,
    copies: &[CopyKind],
    budget: &MemoryBudget,
    limit: u64,
    scan: &impl Serialize,
) -> Result<BudgetedVec<u8>, Code> {
    let control = view.control;
    let choice = view.choice;
    let root = ExtensionRoot::read_installed(view, budget, |manifest| {
        view.translate(view.prepare(manifest, [manifest.root_id]))
            .map_err(|source| ExtensionReadError::Source {
                entry: ExtensionEntry::Manifest,
                source,
            })
    })
    .map_err(|error| view.fault.take().unwrap_or_else(|| root_error(error)))?
    .ok_or(if choice == CopyKind::Cas {
        Code::CorruptContent
    } else {
        Code::NoContent
    })?;
    let mut requested =
        BudgetedVec::new(control.budget(), Category::Metadata).map_err(|_| Code::MemoryLimit)?;
    let mut references = 0usize;
    for (group_index, class) in root.installed_classes().enumerate() {
        check(control)?;
        let class = class.map_err(inventory_error)?;
        references = references.saturating_add(class.references().len());
        if group_index >= ENTRIES || references > ENTRIES {
            return Err(Code::WorkLimit);
        }
        for reference in class
            .references()
            .flatten()
            .filter(ObjectReference::supports_header)
        {
            check(control)?;
            requested
                .try_push(reference.entry_id)
                .map_err(|_| Code::MemoryLimit)?;
        }
    }
    view.prepare(root.manifest(), requested.as_slice().iter().copied())?;
    drop(requested);
    let mut decoder = root
        .installed_objects(view)
        .map_err(|_| Code::MemoryLimit)?;
    let mut output = ContentOutput::new(budget, limit)?;
    output.begin(scan, choice, copies)?;
    for class in root.installed_classes() {
        check(control)?;
        let class = class.map_err(inventory_error)?;
        output.group(&class)?;
        for (reference_index, reference) in class.references().enumerate() {
            check(control)?;
            let position = ReferencePosition {
                part_index: class.part_index,
                class_index: class.class_index,
                reference_index,
            };
            let reference = match reference {
                Ok(reference) => reference,
                Err(_) => {
                    output.row(Row::unavailable(position, None, "invalid_reference"))?;
                    continue;
                }
            };
            match decoder.with_header(reference, |header| {
                output.row(Row::decoded(reference, header))
            }) {
                Ok(written) => written?,
                Err(error) => {
                    let reason = match view.fault.take() {
                        Some(Code::CorruptContent) => "invalid_record",
                        Some(code) => return Err(code),
                        None => match error {
                            ObjectReadError::UnsupportedKind { .. } => "unsupported_kind",
                            ObjectReadError::MissingEntry { .. } => "missing_entry",
                            ObjectReadError::Header {
                                source: HeaderError::Version,
                                ..
                            } => "unsupported_header",
                            ObjectReadError::HeaderPath(_) | ObjectReadError::Header { .. } => {
                                "invalid_header"
                            }
                            ObjectReadError::Brace {
                                source: brace::ManagedParseError::Memory(_),
                                ..
                            }
                            | ObjectReadError::Memory(_) => return Err(Code::MemoryLimit),
                            ObjectReadError::Read { source, .. } => match root_error(source) {
                                Code::CorruptContent => "invalid_record",
                                code => return Err(code),
                            },
                            ObjectReadError::Brace { .. } => "invalid_record",
                            _ => return Err(Code::CorruptContent),
                        },
                    };
                    output.row(Row::unavailable(position, Some(reference.entry_id), reason))?;
                }
            }
        }
        output.end_group()?;
    }
    output.finish()
}

fn inventory_error(error: InventoryError) -> Code {
    match error {
        InventoryError::PartVersion { .. } => Code::ContentLayout,
        _ => Code::CorruptContent,
    }
}

fn root_error(error: ExtensionReadError) -> Code {
    use ExtensionReadError as E;
    match error {
        E::StoredTooLarge(_) | E::DepthLimit(_) => Code::WorkLimit,
        E::Memory(_)
        | E::Brace(brace::ManagedParseError::Memory(_))
        | E::Manifest(mdconf::configinfo::ConfigInfoError::Memory(_))
        | E::Inflate {
            source: zraw::ManagedInflateError::Memory(_),
            ..
        } => Code::MemoryLimit,
        E::Configuration(
            mdconf::parts::extension_configuration::ConfigurationError::UnsupportedRootVersion
            | mdconf::parts::extension_configuration::ConfigurationError::UnsupportedPartVersion
            | mdconf::parts::extension_configuration::ConfigurationError::UnsupportedBodyVersion {
                ..
            }
            | mdconf::parts::extension_configuration::ConfigurationError::UnexpectedValue { .. },
        )
        | E::Manifest(mdconf::configinfo::ConfigInfoError::UnsupportedVersion(_))
        | E::Manifest(mdconf::configinfo::ConfigInfoError::Invalid("supported root descriptor")) => {
            Code::ContentLayout
        }
        E::Source { .. } => Code::Unavailable,
        E::Inflate {
            source: zraw::ManagedInflateError::TooLarge(_),
            ..
        } => Code::WorkLimit,
        _ => Code::CorruptContent,
    }
}

struct View<'a> {
    volume: Volume<'a>,
    manifest: Key,
    control: &'a Control,
    choice: CopyKind,
    id: Guid,
    entries: RefCell<Option<EntryMap>>,
    fault: RefCell<Option<Code>>,
}
impl View<'_> {
    fn prepare(
        &self,
        manifest: &mdconf::configinfo::ConfigInfo,
        requested: impl IntoIterator<Item = Guid>,
    ) -> Result<(), Code> {
        let entries = EntryMap::new(manifest, self.id, self.choice, requested, self.control)?;
        self.volume.prepare(self.control, &entries)?;
        *self.entries.borrow_mut() = Some(entries);
        Ok(())
    }

    fn translate<T>(&self, result: Result<T, Code>) -> mdconf::Result<T> {
        result.map_err(|code| {
            *self.fault.borrow_mut() = Some(code);
            io::Error::from(io::ErrorKind::InvalidData).into()
        })
    }
    fn physical(&self, name: &str) -> Result<Option<Physical>, Code> {
        if name == "configinfo" {
            return Ok(Some(Physical {
                copy: self.choice,
                key: self.manifest,
            }));
        }
        let id = Guid::parse(name).ok_or(Code::CorruptContent)?;
        Ok(self
            .entries
            .borrow()
            .as_ref()
            .and_then(|entries| entries.get(id)))
    }
}
impl EntrySource for View<'_> {
    fn list(&self) -> mdconf::Result<Vec<EntryMeta>> {
        Err(io::Error::from(io::ErrorKind::Unsupported).into())
    }
    fn stored(&self, _: &str) -> mdconf::Result<Option<Vec<u8>>> {
        Err(io::Error::from(io::ErrorKind::Unsupported).into())
    }
    fn stored_len(&self, name: &str) -> mdconf::Result<Option<u64>> {
        self.translate((|| match self.physical(name)? {
            Some(entry) => self.volume.len(self.control, entry),
            None => Ok(None),
        })())
    }
    fn stored_owned_bounded(
        &self,
        name: &str,
        limit: usize,
    ) -> mdconf::Result<Option<StoredBytes>> {
        self.translate((|| match self.physical(name)? {
            Some(entry) => self.volume.fetch(self.control, entry, limit.min(STORED)),
            None => Ok(None),
        })())
    }
}

struct Text<T>(T);
impl<T: std::fmt::Display> Serialize for Text<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&self.0)
    }
}
#[derive(Serialize)]
struct Group {
    part: usize,
    class: usize,
    class_id: Text<Guid>,
    kind: Option<&'static str>,
    references: usize,
}
#[derive(Serialize)]
struct Row<'a> {
    reference: usize,
    entry_id: Option<Text<Guid>>,
    header_id: Option<Text<Guid>>,
    name: Option<&'a str>,
    belonging: Option<&'static str>,
    belonging_code: Option<&'a str>,
    target_id: Option<Text<Guid>>,
    comment: Option<&'a str>,
    property_count: Option<usize>,
    issue: Option<&'static str>,
}
impl<'a> Row<'a> {
    fn unavailable(position: ReferencePosition, id: Option<Guid>, issue: &'static str) -> Self {
        Self {
            reference: position.reference_index,
            entry_id: id.map(Text),
            header_id: None,
            name: None,
            belonging: None,
            belonging_code: None,
            target_id: None,
            comment: None,
            property_count: None,
            issue: Some(issue),
        }
    }
    fn decoded(reference: ObjectReference<'_>, header: InstalledHeader<'a>) -> Self {
        let mut states = header.property_states();
        let count = states.len();
        let valid_properties = states.all(|state| state.is_ok());
        Self {
            reference: reference.reference_index,
            entry_id: Some(Text(reference.entry_id)),
            header_id: Some(Text(header.id)),
            name: Some(header.name),
            belonging: Some(match header.belonging() {
                ObjectBelonging::Own => "own",
                ObjectBelonging::Adopted => "adopted",
                ObjectBelonging::Unknown => "unknown",
            }),
            belonging_code: Some(header.wire_belonging.raw()),
            target_id: header.extended_object.map(Text),
            comment: Some(header.comment),
            property_count: valid_properties.then_some(count),
            issue: (!valid_properties).then_some("invalid_properties"),
        }
    }
}

struct ContentOutput {
    writer: BudgetedWriter,
    limit: u64,
    groups: usize,
    count: usize,
    first_row: bool,
    partial: bool,
}
impl ContentOutput {
    fn new(budget: &MemoryBudget, limit: u64) -> Result<Self, Code> {
        Ok(Self {
            writer: BudgetedWriter::new(budget, limit).map_err(|_| Code::MemoryLimit)?,
            limit,
            groups: 0,
            count: 0,
            first_row: true,
            partial: false,
        })
    }
    fn bytes(&mut self, bytes: &[u8]) -> Result<(), Code> {
        if (self.writer.len() as u64).saturating_add(bytes.len() as u64) > self.limit {
            return Err(Code::WorkLimit);
        }
        self.writer.try_write(bytes).map_err(|_| Code::MemoryLimit)
    }
    fn value(&mut self, value: &impl Serialize) -> Result<(), Code> {
        serde_json::to_writer(ContentSink(self), value).map_err(|error| {
            if error.io_error_kind() == Some(io::ErrorKind::FileTooLarge) {
                Code::WorkLimit
            } else {
                Code::MemoryLimit
            }
        })
    }
    fn begin(
        &mut self,
        scan: &impl Serialize,
        copy: CopyKind,
        copies: &[CopyKind],
    ) -> Result<(), Code> {
        self.bytes(br#"{"version":1,"scan":"#)?;
        self.value(scan)?;
        self.bytes(br#","copy":"#)?;
        self.value(&copy)?;
        self.bytes(br#","copies":"#)?;
        self.value(&copies)?;
        self.bytes(br#","groups":["#)
    }
    fn group(&mut self, class: &mdconf::extension_objects::ExtensionClass<'_>) -> Result<(), Code> {
        if self.groups >= ENTRIES || self.count.saturating_add(class.references().len()) > ENTRIES {
            return Err(Code::WorkLimit);
        }
        if self.groups > 0 {
            self.bytes(b",")?;
        }
        self.groups += 1;
        self.first_row = true;
        self.bytes(br#"{"class":"#)?;
        self.value(&Group {
            part: class.part_index,
            class: class.class_index,
            class_id: Text(class.class_id),
            kind: class.kind,
            references: class.references().len(),
        })?;
        self.bytes(br#","objects":["#)
    }
    fn row(&mut self, row: Row<'_>) -> Result<(), Code> {
        if !self.first_row {
            self.bytes(b",")?;
        }
        self.first_row = false;
        self.count += 1;
        self.partial |= row.issue.is_some();
        self.value(&row)
    }
    fn end_group(&mut self) -> Result<(), Code> {
        self.bytes(b"]}")
    }
    fn finish(mut self) -> Result<BudgetedVec<u8>, Code> {
        self.bytes(br#"],"state":"#)?;
        self.value(&if self.partial { "partial" } else { "ready" })?;
        self.bytes(b"}")?;
        Ok(self.writer.into_buffer())
    }
}
struct ContentSink<'a>(&'a mut ContentOutput);
impl io::Write for ContentSink<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0
            .bytes(bytes)
            .map(|()| bytes.len())
            .map_err(|code| match code {
                Code::WorkLimit => io::ErrorKind::FileTooLarge.into(),
                _ => io::ErrorKind::OutOfMemory.into(),
            })
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
