use super::{
    extension_info::{Uuid, quoted},
    metadata::Format,
};
use crate::Result;
use mdconf::{
    EntrySource, ManagedCfFile,
    extension_header::ObjectBelonging,
    extension_objects::{ExtensionClass, ExtensionClasses, ExtensionObject, ObjectReadError},
    extension_reader::{ExtensionReadError, ExtensionRoot},
};
use memory_budget::{
    BudgetError, BudgetedWriter, Category, LimitReason, MemoryBudget, MemoryLimit,
};
use serde::Serialize;
use std::{
    io::{self, Write},
    path::Path,
};

const OUTPUT_LIMIT: u64 = 8 << 20;

pub(super) struct Fragments<'a> {
    writer: &'a mut BudgetedWriter,
    bytes: [u8; 4096],
    used: usize,
    accepted: u64,
    wire_limit: u64,
    error: Option<BudgetError>,
    #[cfg(test)]
    appends: usize,
}
impl<'a> Fragments<'a> {
    pub(super) fn new(writer: &'a mut BudgetedWriter, wire_limit: u64) -> Self {
        Self {
            writer,
            bytes: [0; 4096],
            used: 0,
            accepted: 0,
            wire_limit,
            error: None,
            #[cfg(test)]
            appends: 0,
        }
    }
    fn append(&mut self) -> std::result::Result<(), BudgetError> {
        if let Some(error) = self.error {
            return Err(error);
        }
        #[cfg(test)]
        self.record_append();
        let (bytes, _) = self.bytes.split_at(self.used);
        if let Err(error) = self.writer.try_write(bytes) {
            self.error = Some(error);
            return Err(error);
        }
        self.used = 0;
        Ok(())
    }
    fn push(&mut self, mut bytes: &[u8]) -> std::result::Result<(), BudgetError> {
        if let Some(error) = self.error {
            return Err(error);
        }
        let next = self
            .accepted
            .checked_add(u64::try_from(bytes.len()).map_err(|_| BudgetError::Overflow)?)
            .ok_or(BudgetError::Overflow)?;
        if next > self.wire_limit {
            return Err(BudgetError::Limit(MemoryLimit {
                category: Category::Response,
                requested: next,
                reason: LimitReason::Category,
            }));
        }
        while !bytes.is_empty() {
            let (_, room) = self.bytes.split_at_mut(self.used);
            let count = room.len().min(bytes.len());
            let (part, rest) = bytes.split_at(count);
            let (destination, _) = room.split_at_mut(count);
            destination.copy_from_slice(part);
            self.used += count;
            bytes = rest;
            if self.used == self.bytes.len() {
                self.append()?;
            }
        }
        self.accepted = next;
        Ok(())
    }
    pub(super) fn remaining(&self) -> u64 {
        self.wire_limit - self.accepted
    }

    pub(super) fn error(&self) -> Option<BudgetError> {
        self.error
    }
    pub(super) fn finish(&mut self) -> std::result::Result<(), BudgetError> {
        if let Some(error) = self.error {
            return Err(error);
        }
        if self.used != 0 {
            self.append()?;
        }
        Ok(())
    }
}
impl Write for Fragments<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if let Err(error) = self.push(bytes) {
            self.error = Some(error);
            return Err(io::ErrorKind::OutOfMemory.into());
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error("extension source: {0}")]
    Source(#[source] mdconf::Error),
    #[error("extension root: {0}")]
    Read(#[from] ExtensionReadError),
    #[error("configinfo manifest is absent; extension objects are unavailable")]
    NoManifest,
    #[error("extension objects: {0}")]
    Objects(#[from] ObjectReadError),
    #[error("extension output: {0}")]
    Output(#[source] BudgetError),
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ClassGroup<'a> {
    part_index: usize,
    class_index: usize,
    class_id: Uuid,
    raw_class: &'a str,
    kind: Option<&'static str>,
    reference_count: usize,
}
impl<'a> ClassGroup<'a> {
    fn new(class: ExtensionClass<'a>) -> Self {
        Self {
            part_index: class.part_index,
            class_index: class.class_index,
            class_id: Uuid(class.class_id),
            raw_class: class.raw_class,
            kind: class.kind,
            reference_count: class.references().len(),
        }
    }
    fn text(&self, out: &mut dyn Write) -> Result<()> {
        write!(
            out,
            "Class group {}/{}: {} (kind: {}, references: {}, raw: ",
            self.part_index,
            self.class_index,
            self.class_id.0,
            self.kind.unwrap_or("Unknown"),
            self.reference_count
        )?;
        quoted(out, self.raw_class)?;
        out.write_all(b")\n")?;
        Ok(())
    }
}
#[derive(Serialize)]
struct Belonging<'a> {
    known: &'static str,
    raw: &'a str,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Object<'a> {
    part_index: usize,
    class_index: usize,
    reference_index: usize,
    class_id: Uuid,
    raw_class: &'a str,
    kind: Option<&'static str>,
    entry_id: Uuid,
    raw_entry: &'a str,
    header_id: Uuid,
    name: &'a str,
    belonging: Belonging<'a>,
    extended_target: Option<Uuid>,
    property_state_count: usize,
}
impl<'a> Object<'a> {
    fn new(object: &'a ExtensionObject<'_>) -> Result<Self> {
        let r = object.reference;
        let header = object.configured_header().map_err(Error::Objects)?;
        let pairs = header.property_states();
        let property_state_count = pairs.len();
        for pair in pairs {
            pair.map_err(|source| {
                Error::Objects(ObjectReadError::Header {
                    position: r.position(),
                    source,
                })
            })?;
        }
        let known = match header.belonging() {
            ObjectBelonging::Own => "Own",
            ObjectBelonging::Adopted => "Adopted",
            ObjectBelonging::Unknown => "Unknown",
        };
        Ok(Self {
            part_index: r.part_index,
            class_index: r.class_index,
            reference_index: r.reference_index,
            class_id: Uuid(r.class_id),
            raw_class: r.raw_class,
            kind: r.kind,
            entry_id: Uuid(r.entry_id),
            raw_entry: r.raw_entry,
            header_id: Uuid(header.id),
            name: header.name,
            belonging: Belonging {
                known,
                raw: header.wire_belonging.raw(),
            },
            extended_target: header.extended_object.map(Uuid),
            property_state_count,
        })
    }
    fn text(&self, out: &mut dyn Write) -> Result<()> {
        write!(
            out,
            "Object {}/{}/{}:\nClass: {} (kind: {}, raw: ",
            self.part_index,
            self.class_index,
            self.reference_index,
            self.class_id.0,
            self.kind.unwrap_or("Unknown")
        )?;
        quoted(out, self.raw_class)?;
        write!(out, ")\nEntry: {} (raw: ", self.entry_id.0)?;
        quoted(out, self.raw_entry)?;
        write!(out, ")\nHeader: {}\nName: ", self.header_id.0)?;
        quoted(out, self.name)?;
        write!(out, "\nBelonging: {} (raw: ", self.belonging.known)?;
        quoted(out, self.belonging.raw)?;
        match &self.extended_target {
            Some(target) => write!(out, ")\nExtended target: {}", target.0)?,
            None => out.write_all(b")\nExtended target: absent")?,
        }
        writeln!(out, "\nProperty state count: {}", self.property_state_count)?;
        Ok(())
    }
}

pub(super) fn run(path: &Path, format: Format, out: &mut dyn Write) -> Result<()> {
    let (source_budget, budget) = crate::service::memory::extension_info_budgets()?;
    run_with_budgets(path, format, &source_budget, &budget, OUTPUT_LIMIT, out)
}
fn run_with_budgets(
    path: &Path,
    format: Format,
    source_budget: &MemoryBudget,
    budget: &MemoryBudget,
    output_limit: u64,
    out: &mut dyn Write,
) -> Result<()> {
    let source = ManagedCfFile::open(path, source_budget).map_err(Error::Source)?;
    inspect(&source, budget, format, output_limit, out)
}
fn inspect(
    source: &dyn EntrySource,
    budget: &MemoryBudget,
    format: Format,
    output_limit: u64,
    out: &mut dyn Write,
) -> Result<()> {
    let owner = ExtensionRoot::read(source, budget)
        .map_err(Error::Read)?
        .ok_or(Error::NoManifest)?;
    let buffer = serialize(&owner, source, budget, format, output_limit)?;
    out.write_all(buffer.as_slice())?;
    Ok(())
}
fn serialize(
    owner: &ExtensionRoot,
    source: &dyn EntrySource,
    budget: &MemoryBudget,
    format: Format,
    output_limit: u64,
) -> Result<BudgetedWriter> {
    let mut objects = owner.objects(source).map_err(Error::Objects)?;
    let mut buffer = BudgetedWriter::new(budget, output_limit).map_err(Error::Output)?;
    let mut out = Fragments::new(&mut buffer, output_limit);
    let serialized = (|| -> Result<()> {
        let cfg = owner.configuration()?;
        match format {
            Format::Json => {
                out.write_all(
                    b"{\n  \"schemaVersion\": 2,\n  \"scope\": \"topLevel\",\n  \"rootEntryId\": ",
                )?;
                serde_json::to_writer(&mut out, &Uuid(cfg.root_entry_id))?;
                out.write_all(b",\n  \"classGroups\": [")?;
            }
            Format::Text => writeln!(out, "Scope: topLevel\nRoot entry: {}", cfg.root_entry_id)?,
        }
        let mut first = true;
        for class in ExtensionClasses::new(&cfg) {
            let class = ClassGroup::new(
                class
                    .map_err(ObjectReadError::from)
                    .map_err(Error::Objects)?,
            );
            match format {
                Format::Json => {
                    if !first {
                        out.write_all(b",")?;
                    }
                    out.write_all(b"\n")?;
                    serde_json::to_writer_pretty(&mut out, &class)?;
                }
                Format::Text => class.text(&mut out)?,
            }
            first = false;
        }
        if let Format::Json = format {
            out.write_all(b"\n  ],\n  \"objects\": [")?;
        }
        first = true;
        while let Some(object) = objects.next().map_err(Error::Objects)? {
            let dto = Object::new(&object)?;
            match format {
                Format::Json => {
                    if !first {
                        out.write_all(b",")?;
                    }
                    out.write_all(b"\n")?;
                    serde_json::to_writer_pretty(&mut out, &dto)?;
                }
                Format::Text => dto.text(&mut out)?,
            }
            first = false;
        }
        if let Format::Json = format {
            out.write_all(b"\n  ]\n}\n")?;
        }
        Ok(())
    })();
    let finished = if serialized.is_ok() {
        out.finish()
    } else {
        Ok(())
    };
    let error = out.error();
    if let Some(error) = error {
        return Err(Error::Output(error).into());
    }
    finished.map_err(Error::Output)?;
    if let Some(error) = buffer.error() {
        return Err(Error::Output(*error).into());
    }
    serialized?;
    Ok(buffer)
}
