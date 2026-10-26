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
        Ok(Self {
            schema: mdschema::schema()?,
            names: values::Names::new(&configuration.index),
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

mod schema;
mod shell_index;
mod shell_xml;
mod shells;
mod walk;

mod objects;
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

mod part_paths;

mod part_xml;

mod fmt220;
pub use fmt220::translate_2_20;

mod fmt220_versions;
pub use fmt220_versions::read_named_versions;
