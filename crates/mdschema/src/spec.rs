use serde::Deserialize;
use serde::de::{Deserializer, MapAccess, Visitor};
use std::collections::BTreeMap;
use std::fmt;
use std::marker::PhantomData;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Codec {
    Bool,
    Int,
    Str,
    Enum,
    LStr,
    Ref,
    Type,
    TValue,
    MdList,
    FieldList,
    CpLinks,
    LinkByType,
    CParams,
    Event,
    Method,
    FoContent,
    CmdGroup,
    Picture,
    Shortcut,
    Color,
    StyleValue,
    XdtoType,
    XdtoPackages,
    Characteristics,
    StdAttrs,
    Purposes,
    CaContent,
    #[serde(other)]
    #[default]
    Unknown,
}

pub(crate) struct Ordered<T>(pub Vec<(String, T)>);

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Ordered<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<T>(PhantomData<T>);
        impl<'de, T: Deserialize<'de>> Visitor<'de> for V<T> {
            type Value = Ordered<T>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a map")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<Self::Value, A::Error> {
                let mut out = Vec::new();
                while let Some(kv) = m.next_entry::<String, T>()? {
                    out.push(kv);
                }
                Ok(Ordered(out))
            }
        }
        d.deserialize_map(V(PhantomData))
    }
}

fn ordered<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Vec<(String, T)>, D::Error> {
    Ok(Ordered::<T>::deserialize(d)?.0)
}

#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct PropSpec {
    pub path: Option<Vec<usize>>,
    pub hp: Option<Vec<usize>>,
    pub hpp: Option<Vec<usize>>,
    pub rel_header: Option<Vec<usize>>,
    pub fd: Option<Vec<usize>>,
    #[serde(default)]
    pub codec: Codec,
    #[serde(default)]
    pub map: BTreeMap<String, String>,
    pub xml_type: Option<String>,
    pub path2: Option<Vec<usize>>,
    pub false_if: Option<(Vec<usize>, String)>,
    pub std: Option<String>,
}

#[cfg(any(test, feature = "testing"))]
impl PropSpec {
    pub fn for_tests(codec: &str, map: &[(&str, &str)]) -> PropSpec {
        let codec =
            serde_json::from_value(serde_json::Value::String(codec.into())).unwrap_or_default();
        PropSpec {
            codec,
            map: map
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct InternalInfoSpec {
    pub category: String,
    pub type_id: Vec<usize>,
    pub value_id: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ChildSpec {
    pub tag: String,
    pub uuid: Option<Vec<usize>>,
    pub header: Option<Vec<usize>>,
    #[serde(default, deserialize_with = "ordered")]
    pub props: Vec<(String, PropSpec)>,
    #[serde(default)]
    pub internal_info: Vec<InternalInfoSpec>,
    #[serde(default, deserialize_with = "ordered")]
    pub children: Vec<(String, ChildSpec)>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct KindSpec {
    pub kind: String,
    pub class: Option<String>,
    pub folder: Option<String>,
    pub uuid: Option<Vec<usize>>,
    pub header: Option<Vec<usize>>,
    #[serde(default)]
    pub internal_info: Vec<InternalInfoSpec>,
    #[serde(default, deserialize_with = "ordered")]
    pub props: Vec<(String, PropSpec)>,
    #[serde(default, deserialize_with = "ordered")]
    pub children: Vec<(String, ChildSpec)>,
    #[serde(default, deserialize_with = "ordered")]
    pub subobjects: Vec<(String, String)>,
    pub this_node: Option<Vec<usize>>,
    #[serde(default)]
    pub find_header: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XdtoProp {
    pub name: String,
    pub type_ns: Option<String>,
    pub type_name: Option<String>,
    pub lower: i32,
    pub upper: i32,
    pub form: String,
}

#[derive(Deserialize)]
pub(crate) struct RawXdtoProp(String, Option<String>, Option<String>, i32, i32, String);

impl From<RawXdtoProp> for XdtoProp {
    fn from(r: RawXdtoProp) -> Self {
        XdtoProp {
            name: r.0,
            type_ns: r.1,
            type_name: r.2,
            lower: r.3,
            upper: r.4,
            form: r.5,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformType {
    pub set: bool,
    pub namespace_uri: String,
    pub local: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct BagProp {
    pub xml: String,
    #[serde(default)]
    pub codec: Codec,
    #[serde(default)]
    pub map: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct Ui {
    #[serde(default)]
    pub command_groups: BTreeMap<String, String>,
    #[serde(default)]
    pub std_pictures: BTreeMap<String, String>,
    #[serde(default)]
    pub web_colors: BTreeMap<String, String>,
    #[serde(default)]
    pub win_colors: BTreeMap<String, String>,
    #[serde(default)]
    pub style_colors: BTreeMap<String, String>,
    #[serde(default)]
    pub keys: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct UiTables {
    #[serde(default)]
    pub style_fonts: BTreeMap<String, String>,
    #[serde(default)]
    pub windows_fonts: BTreeMap<String, String>,
    #[serde(default)]
    pub style_colors: BTreeMap<String, String>,
    #[serde(default)]
    pub win_colors: BTreeMap<String, String>,
    #[serde(default)]
    pub style_borders: BTreeMap<String, String>,
    #[serde(default)]
    pub border_styles: BTreeMap<String, String>,
    #[serde(default)]
    pub style_order: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigPath {
    Body(Vec<usize>),
    Part2,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConfigPropSpec {
    pub name: String,
    pub path: Option<ConfigPath>,
    pub codec: String,
    pub extra: serde_json::Value,
}

impl PlatformType {
    pub fn name(&self) -> crate::ExpandedTypeName<'_> {
        crate::ExpandedTypeName {
            namespace_uri: &self.namespace_uri,
            local: &self.local,
        }
    }
    pub fn legacy_qname(&self) -> Option<String> {
        self.name().legacy_qname()
    }
}
