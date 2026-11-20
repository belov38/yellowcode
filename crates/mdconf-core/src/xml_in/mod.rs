mod namespaces;
mod values;
pub use namespaces::{NamespaceContext, NamespaceScope};

use crate::xml::XmlNode;
use crate::{Codec, Configuration, Diagnostics, Result, Value};

pub struct ValueReader {
    schema: &'static mdschema::Schema,
    names: values::Names,
    diagnostics: Diagnostics,
}

impl ValueReader {
    pub fn new(configuration: &Configuration) -> Result<Self> {
        Self::with_index(&configuration.index)
    }

    #[doc(hidden)]
    pub fn with_index(index: &md_model::index::Index) -> Result<Self> {
        Ok(Self {
            schema: mdschema::schema()?,
            names: values::Names::new(index),
            diagnostics: Diagnostics::default(),
        })
    }

    pub fn read(&mut self, scope: &str, codec: Codec, element: &XmlNode) -> Value {
        let mut cx = values::XCtx {
            schema: self.schema,
            names: &self.names,
            diag: &mut self.diagnostics,
            scope: scope.to_string(),
        };
        values::value(&mut cx, codec, element)
    }

    pub fn read_scoped(
        &mut self,
        scope: &str,
        codec: Codec,
        element: &XmlNode,
        namespaces: &dyn NamespaceScope,
    ) -> Value {
        let mut cx = values::XCtx {
            schema: self.schema,
            names: &self.names,
            diag: &mut self.diagnostics,
            scope: scope.to_string(),
        };
        values::property_scoped(&mut cx, codec, None, element, None, namespaces)
    }

    pub fn read_property(
        &mut self,
        scope: &str,
        spec: &mdschema::PropSpec,
        element: &XmlNode,
        depth: usize,
    ) -> Value {
        self.read_property_scoped(scope, spec, element, depth, &NamespaceContext::EMPTY)
    }

    pub fn read_property_scoped(
        &mut self,
        scope: &str,
        spec: &mdschema::PropSpec,
        element: &XmlNode,
        indent_depth: usize,
        namespaces: &dyn NamespaceScope,
    ) -> Value {
        self.read_property_named_scoped(
            scope,
            spec,
            (element, &values::qname(element)),
            indent_depth,
            namespaces,
        )
    }

    fn read_property_named_scoped(
        &mut self,
        scope: &str,
        spec: &mdschema::PropSpec,
        property: (&XmlNode, &str),
        indent_depth: usize,
        namespaces: &dyn NamespaceScope,
    ) -> Value {
        let (element, output_tag) = property;
        let mut cx = values::XCtx {
            schema: self.schema,
            names: &self.names,
            diag: &mut self.diagnostics,
            scope: scope.to_string(),
        };
        values::property_scoped_as(
            &mut cx,
            spec.codec,
            Some(&spec.map),
            element,
            Some(indent_depth),
            namespaces,
            output_tag,
        )
    }

    pub fn diagnostics(&self) -> &Diagnostics {
        &self.diagnostics
    }
    pub(crate) fn resolved_uuid_reference(&mut self, scope: &str, tag: &str) {
        self.qualified_miss("unresolved", scope, tag);
    }
    pub(crate) fn qualified_external_empty_collection(&mut self, scope: &str, prop: &crate::Prop) {
        if matches!(
            prop.codec,
            Codec::CpLinks | Codec::CParams | Codec::LinkByType
        ) && let Value::Xml(value) = &prop.value
            && value.children.is_empty()
            && value.attrs.is_empty()
            && shell_xml::xml_whitespace(value.text.as_deref().unwrap_or_default())
        {
            self.qualified_miss("raw", scope, &prop.name);
        }
    }
    fn qualified_miss(&mut self, what: &str, scope: &str, tag: &str) {
        let key = format!("{what}:{scope}.{tag}");
        if let Some(count) = self.diagnostics.misses.get_mut(&key) {
            if *count > 1 {
                *count -= 1;
            } else {
                self.diagnostics.misses.remove(&key);
            }
        }
    }

    pub fn into_diagnostics(self) -> Diagnostics {
        self.diagnostics
    }
}

#[derive(Debug, Clone)]
pub struct ShellOptions {
    pub max_directory_depth: usize,
    pub max_entries: usize,
    pub max_inventory_path_bytes: usize,
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
    pub max_metadata_depth: usize,
    pub max_identities: usize,
    pub max_full_name_bytes: usize,
    pub max_total_name_bytes: usize,
    pub max_xml_nodes: usize,
    pub max_xml_attributes: usize,
    pub max_xml_namespaces: usize,
    pub max_xml_depth: usize,
    pub max_xml_memory: u64,
    pub max_xml_text_work: u64,
}
impl Default for ShellOptions {
    fn default() -> Self {
        Self {
            max_directory_depth: 256,
            max_entries: 1_000_000,
            max_inventory_path_bytes: 128 << 20,
            max_file_bytes: 64 << 20,
            max_total_bytes: 512 << 20,
            max_metadata_depth: 64,
            max_identities: 1_000_000,
            max_full_name_bytes: 4096,
            max_total_name_bytes: 64 << 20,
            max_xml_nodes: 100_000,
            max_xml_attributes: 64,
            max_xml_namespaces: 32,
            max_xml_depth: 256,
            max_xml_memory: 128 << 20,
            max_xml_text_work: 4 << 30,
        }
    }
}
pub use md_model::xml_origin::XmlFormat;
#[derive(Debug, Default)]
pub struct ShellReport {
    pub descriptions: usize,
    pub identities: usize,
    pub property_codecs: std::collections::BTreeMap<String, (Codec, usize)>,
    pub unknown_properties: std::collections::BTreeMap<String, usize>,
    pub known_without_codec: std::collections::BTreeMap<String, usize>,
    pub configuration_properties: usize,
    pub unread_files: Vec<std::path::PathBuf>,
    pub unreferenced_descriptions: Vec<std::path::PathBuf>,
    pub unsupported: Vec<UnsupportedShell>,
}
#[derive(Debug)]
pub struct UnsupportedShell {
    pub id: brace::Guid,
    pub name: String,
    pub path: std::path::PathBuf,
}
impl ShellReport {
    pub fn summary(&self) -> String {
        format!(
            "{} descriptions, {} identities, {} schema/header properties, {} configuration properties, {} known without codec, {} unknown properties, {} unsupported identities",
            self.descriptions,
            self.identities,
            self.property_codecs.values().map(|(_, n)| n).sum::<usize>(),
            self.configuration_properties,
            self.known_without_codec.values().sum::<usize>(),
            self.unknown_properties.values().sum::<usize>(),
            self.unsupported.len()
        )
    }
}
#[derive(Debug)]
pub struct ShellDump {
    pub configuration: Configuration,
    pub format: XmlFormat,
    pub files: std::collections::BTreeMap<std::path::PathBuf, std::path::PathBuf>,
    pub report: ShellReport,
}
pub fn load_shells(dir: &std::path::Path, options: &ShellOptions) -> Result<ShellDump> {
    shells::load(dir, options)
}

pub(crate) mod schema;
mod shell_index;
pub(crate) mod shell_xml;
mod shells;
pub(crate) mod walk;

pub(crate) mod objects;
pub use objects::{ObjectDump, ObjectOptions, ObjectReport, load_objects};
mod modules;

mod part_misc;
mod part_predefined;
mod part_rights;
mod parts;
pub use parts::{PartsDump, PartsOptions, load_parts};

#[doc(hidden)]
pub fn parse_part<'a>(
    bytes: &'a [u8],
    path: &std::path::Path,
    options: &ShellOptions,
    budget: &memory_budget::MemoryBudget,
    text_work: &mut u64,
) -> Result<crate::xml::view::XmlArena<'a>> {
    shell_xml::parse(bytes, path, options, budget, text_work)
}

#[doc(hidden)]
#[derive(Debug)]
pub enum CheckedPartError {
    Part(crate::Error),
    Memory(memory_budget::BudgetError),
    Cancelled,
}
#[doc(hidden)]
pub fn parse_part_checked<'a>(
    bytes: &'a [u8],
    path: &std::path::Path,
    options: &ShellOptions,
    budget: &memory_budget::MemoryBudget,
    text_work: &mut u64,
    cancelled: &dyn Fn() -> bool,
) -> std::result::Result<crate::xml::view::XmlArena<'a>, CheckedPartError> {
    shell_xml::parse_checked(bytes, path, options, budget, text_work, cancelled)
}

mod part_paths;

mod part_xml;

mod fmt220;
pub use fmt220::translate_2_20;

mod fmt220_versions;
pub use fmt220_versions::read_named_versions;
