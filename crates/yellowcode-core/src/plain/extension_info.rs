use super::metadata::Format;
use crate::Result;
use brace::Guid;
use mdconf::{
    EntrySource, ManagedCfFile,
    extension_reader::{ExtensionReadError, ExtensionRoot},
    parts::extension_configuration::ConfigurationExtensionPurpose,
};
use memory_budget::{BudgetError, BudgetedWriter, MemoryBudget};
use serde::{Serialize, Serializer};
use std::{
    io::{self, Write},
    path::Path,
};

const OUTPUT_LIMIT: u64 = 8 << 20;

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error("extension source: {0}")]
    Source(#[source] mdconf::Error),
    #[error("extension properties: {0}")]
    Read(#[from] ExtensionReadError),
    #[error("configinfo manifest is absent; extension properties are unavailable")]
    NoManifest,
    #[error("extension output: {0}")]
    Output(#[source] BudgetError),
}

pub(super) struct Uuid(pub(super) Guid);
impl Serialize for Uuid {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.collect_str(&self.0)
    }
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    format_version: u32,
    compatibility: u32,
    root_id: Uuid,
    record_count: usize,
}
#[derive(Serialize)]
struct Purpose<'a> {
    raw: &'a str,
    known: Option<&'static str>,
}
#[derive(Serialize)]
struct Mapping<'a> {
    raw: Option<&'a str>,
    value: Option<bool>,
}
#[derive(Serialize)]
struct Body<'a> {
    version: &'a str,
    values: usize,
    parts: usize,
    observed: bool,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Compatibility<'a> {
    slot26: &'a str,
    slot43: &'a str,
    spellings_match: bool,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Configuration<'a> {
    root_entry_id: Uuid,
    header_id: Uuid,
    name: &'a str,
    name_prefix: &'a str,
    purpose: Purpose<'a>,
    keep_mapping_by_ids: Mapping<'a>,
    compatibility: Compatibility<'a>,
    body: Body<'a>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Properties<'a> {
    schema_version: u8,
    manifest: Manifest,
    configuration: Configuration<'a>,
}
impl<'a> Properties<'a> {
    fn new(owner: &'a ExtensionRoot) -> Result<Self> {
        let manifest = owner.manifest();
        let c = owner.configuration()?;
        let known = match c.purpose() {
            ConfigurationExtensionPurpose::Patch => Some("Patch"),
            ConfigurationExtensionPurpose::Customization => Some("Customization"),
            ConfigurationExtensionPurpose::AddOn => Some("AddOn"),
            ConfigurationExtensionPurpose::Unknown => None,
        };
        Ok(Self {
            schema_version: 2,
            manifest: Manifest {
                format_version: manifest.format_version,
                compatibility: manifest.compatibility,
                root_id: Uuid(manifest.root_id),
                record_count: manifest.entries.len(),
            },
            configuration: Configuration {
                root_entry_id: Uuid(c.root_entry_id),
                header_id: Uuid(c.header.id),
                name: c.header.name,
                name_prefix: c.name_prefix,
                purpose: Purpose {
                    raw: c.wire_purpose.raw(),
                    known,
                },
                keep_mapping_by_ids: Mapping {
                    raw: c.wire_keep_mapping_by_ids.map(|wire| wire.raw()),
                    value: c.keep_mapping_by_ids(),
                },
                compatibility: Compatibility {
                    slot26: c.compatibility_at_26.raw(),
                    slot43: c.compatibility_at_43.raw(),
                    spellings_match: c.compatibility_spellings_match(),
                },
                body: Body {
                    version: c.wire_body_version.raw(),
                    values: c.body_values,
                    parts: c.parts,
                    observed: c.observed(),
                },
            },
        })
    }

    fn text(&self, out: &mut dyn Write) -> Result<()> {
        let m = &self.manifest;
        let c = &self.configuration;
        writeln!(
            out,
            "Manifest root: {}\nRoot entry: {}\nHeader: {}",
            m.root_id.0, c.root_entry_id.0, c.header_id.0
        )?;
        out.write_all(b"Name: ")?;
        quoted(out, c.name)?;
        out.write_all(b"\nName prefix: ")?;
        quoted(out, c.name_prefix)?;
        write!(
            out,
            "\nPurpose: {} (raw: ",
            c.purpose.known.unwrap_or("Unknown")
        )?;
        quoted(out, c.purpose.raw)?;
        let mapping = match c.keep_mapping_by_ids.value {
            Some(true) => "true",
            Some(false) => "false",
            None => "unknown",
        };
        if let Some(raw) = c.keep_mapping_by_ids.raw {
            write!(out, ")\nKeep mapping by IDs: {mapping} (raw: ")?;
            quoted(out, raw)?;
            out.write_all(b")")?;
        } else {
            out.write_all(b")\nKeep mapping by IDs: absent")?;
        }
        out.write_all(b"\nCompatibility slot 26: ")?;
        quoted(out, c.compatibility.slot26)?;
        out.write_all(b"\nCompatibility slot 43: ")?;
        quoted(out, c.compatibility.slot43)?;
        writeln!(
            out,
            "\nCompatibility spellings match: {}\nManifest format version: {}\nManifest compatibility: {}\nManifest records: {}",
            c.compatibility.spellings_match, m.format_version, m.compatibility, m.record_count
        )?;
        let b = &c.body;
        out.write_all(b"Body version: ")?;
        quoted(out, b.version)?;
        writeln!(out, " ({} values, {} parts)", b.values, b.parts)?;
        if !b.observed {
            out.write_all(
                b"Warning: this body version and size were not observed; properties are read at the positions of the observed bodies\n",
            )?;
        }
        Ok(())
    }
}

struct TextFormatter;
impl serde_json::ser::Formatter for TextFormatter {
    fn write_string_fragment<W: ?Sized + Write>(
        &mut self,
        out: &mut W,
        fragment: &str,
    ) -> io::Result<()> {
        for c in fragment.chars() {
            if c.is_control() {
                write!(out, "\\u{:04x}", u32::from(c))?;
            } else {
                out.write_all(c.encode_utf8(&mut [0; 4]).as_bytes())?;
            }
        }
        Ok(())
    }
}
pub(super) fn quoted(out: &mut dyn Write, value: &str) -> Result<()> {
    value.serialize(&mut serde_json::Serializer::with_formatter(
        out,
        TextFormatter,
    ))?;
    Ok(())
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
    let buffer = serialize(&owner, budget, format, output_limit)?;
    out.write_all(buffer.as_slice())?;
    Ok(())
}
fn serialize(
    owner: &ExtensionRoot,
    budget: &MemoryBudget,
    format: Format,
    output_limit: u64,
) -> Result<BudgetedWriter> {
    let properties = Properties::new(owner)?;
    let mut buffer = BudgetedWriter::new(budget, output_limit).map_err(Error::Output)?;
    let serialized = match format {
        Format::Json => {
            let mut sink = buffer.serialization_sink();
            serde_json::to_writer_pretty(&mut sink, &properties)
                .map_err(Into::into)
                .and_then(|()| sink.write_all(b"\n").map_err(Into::into))
        }
        Format::Text => properties.text(&mut buffer.serialization_sink()),
    };
    if let Some(error) = buffer.error() {
        return Err(Error::Output(*error).into());
    }
    serialized?;
    Ok(buffer)
}
