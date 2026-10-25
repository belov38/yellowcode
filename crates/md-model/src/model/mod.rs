mod value;

pub use value::*;

use brace::Guid;
use mdschema::MdKind;
use std::borrow::Cow;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Configuration {
    pub xml_origin: Option<crate::xml_origin::XmlOrigin>,
    pub id: Guid,
    #[doc(hidden)]
    pub root_info: Option<std::sync::Arc<dyn crate::CodecState>>,
    pub header_id: Guid,
    pub compat: u32,
    pub props: Vec<Prop>,
    pub objects: Vec<MdObject>,
    pub modules: Vec<Module>,
    pub diagnostics: Diagnostics,
    pub parts: crate::parts::ConfigParts,
    pub settings: crate::ConfigSettings,
    #[doc(hidden)]
    pub index: crate::index::Index,
    #[doc(hidden)]
    pub predef: std::collections::HashMap<Guid, String>,
    pub profile: crate::LoadProfile,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Diagnostics {
    pub misses: BTreeMap<String, u32>,
    pub unsupported: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MdObject {
    pub kind: MdKind,
    pub id: Guid,
    pub name: String,
    pub synonym: LocalString,
    pub comment: String,
    pub generated_types: Vec<GeneratedType>,
    pub this_node: Option<Guid>,
    pub props: Vec<Prop>,
    pub children: Vec<Child>,
    pub forms: Vec<SubObject>,
    pub templates: Vec<SubObject>,
    pub recalculations: Vec<Recalculation>,
    pub subsystems: Vec<MdObject>,
    pub modules: Vec<Module>,
    pub parts: crate::parts::ObjectParts,
    pub form: Option<crate::forms::Form>,
    pub template: Option<crate::templates::Template>,
    pub flowchart: Option<crate::graph::Flowchart>,
}

impl Configuration {
    fn all_objects(&self) -> impl Iterator<Item = &MdObject> {
        let mut stack: Vec<&MdObject> = self.objects.iter().rev().collect();
        std::iter::from_fn(move || {
            let o = stack.pop()?;
            stack.extend(o.subsystems.iter().rev());
            Some(o)
        })
    }

    pub fn object(&self, id: Guid) -> Option<&MdObject> {
        self.all_objects().find(|o| o.id == id)
    }

    pub fn find(&self, kind: MdKind, name: &str) -> Option<&MdObject> {
        let hit = |o: &&MdObject| o.kind == kind && o.name == name;
        self.objects
            .iter()
            .find(hit)
            .or_else(|| self.all_objects().find(hit))
    }

    pub fn of_kind(&self, kind: MdKind) -> impl Iterator<Item = &MdObject> {
        self.all_objects().filter(move |o| o.kind == kind)
    }
}

fn find_prop<'a>(props: &'a [Prop], name: &str) -> Option<&'a Value> {
    props.iter().find(|p| p.name == name).map(|p| &p.value)
}

impl MdObject {
    pub fn prop(&self, name: &str) -> Option<&Value> {
        find_prop(&self.props, name)
    }

    fn with_tag(&self, tag: ChildTag) -> impl Iterator<Item = &Child> {
        self.children.iter().filter(move |c| c.tag == tag)
    }

    pub fn attributes(&self) -> impl Iterator<Item = &Child> {
        self.with_tag(ChildTag::Attribute)
    }
    pub fn tabular_sections(&self) -> impl Iterator<Item = &Child> {
        self.with_tag(ChildTag::TabularSection)
    }
    pub fn dimensions(&self) -> impl Iterator<Item = &Child> {
        self.with_tag(ChildTag::Dimension)
    }
    pub fn resources(&self) -> impl Iterator<Item = &Child> {
        self.with_tag(ChildTag::Resource)
    }
    pub fn enum_values(&self) -> impl Iterator<Item = &Child> {
        self.with_tag(ChildTag::EnumValue)
    }
    pub fn commands(&self) -> impl Iterator<Item = &Child> {
        self.with_tag(ChildTag::Command)
    }

    pub fn standard_attributes(&self) -> &[StandardAttribute] {
        match self.prop("StandardAttributes") {
            Some(Value::StandardAttributes(v)) => v,
            _ => &[],
        }
    }

    pub fn module(&self, kind: ModuleKind) -> Option<&Module> {
        self.modules.iter().find(|m| m.kind == kind)
    }

    pub fn recalculation(&self, name: &str) -> Option<&Recalculation> {
        self.recalculations.iter().find(|r| r.name == name)
    }

    pub fn type_id(&self, category: &str) -> Option<Guid> {
        self.generated_types
            .iter()
            .find(|g| g.category == category)
            .map(|g| g.type_id)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Child {
    pub tag: ChildTag,
    pub id: Guid,
    pub name: String,
    pub synonym: LocalString,
    pub comment: String,
    pub generated_types: Vec<GeneratedType>,
    pub props: Vec<Prop>,
    pub children: Vec<Child>,
    pub module: Option<Module>,
}

impl Child {
    pub fn prop(&self, name: &str) -> Option<&Value> {
        find_prop(&self.props, name)
    }

    pub fn type_desc(&self) -> Option<&TypeDescription> {
        match self.prop("Type") {
            Some(Value::Type(t)) => Some(t),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChildTag {
    Attribute,
    TabularSection,
    Dimension,
    Resource,
    EnumValue,
    Command,
    AddressingAttribute,
    AccountingFlag,
    ExtDimensionAccountingFlag,
    Column,
    URLTemplate,
    Method,
    Operation,
    Parameter,
    IntegrationServiceChannel,
    Function,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Recalculation {
    pub id: Guid,
    pub name: String,
    pub synonym: LocalString,
    pub comment: String,
    pub generated_types: Vec<GeneratedType>,
    pub props: Vec<Prop>,
    pub dimensions: Vec<RecalcDimension>,
    pub module: Option<Module>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RecalcDimension {
    pub id: Guid,
    pub name: String,
    pub synonym: LocalString,
    pub comment: String,
    pub register_dimension: MdRef,
    pub leading_register_data: Vec<MdRef>,
    pub props: Vec<Prop>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SubKind {
    Form,
    Template,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SubObject {
    pub kind: SubKind,
    pub id: Guid,
    pub name: String,
    pub props: Vec<Prop>,
    pub module: Option<Module>,
    pub form: Option<crate::forms::Form>,
    pub template: Option<crate::templates::Template>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedType {
    pub category: String,
    pub type_id: Guid,
    pub value_id: Guid,
}

#[derive(Debug, Clone)]
pub struct GeneratedTypeRef<'a> {
    pub object: &'a MdObject,
    pub child: Option<&'a Child>,
    pub category: &'a str,
    pub recalculation: Option<&'a Recalculation>,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Module {
    pub kind: ModuleKind,
    pub source: Vec<u8>,
}

impl Module {
    pub fn text(&self) -> Cow<'_, str> {
        let b = self
            .source
            .strip_prefix(b"\xef\xbb\xbf")
            .unwrap_or(&self.source);
        String::from_utf8_lossy(b)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModuleKind {
    Module,
    ObjectModule,
    ManagerModule,
    RecordSetModule,
    ValueManagerModule,
    CommandModule,
    FormModule,
    ManagedApplicationModule,
    OrdinaryApplicationModule,
    SessionModule,
    ExternalConnectionModule,
}
