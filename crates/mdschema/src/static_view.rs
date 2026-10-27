use crate::Codec;

#[path = "static_data.rs"]
mod data;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaticValue {
    Null,
    Bool(bool),
    Integer(i64),
    String(&'static str),
    Array(&'static [StaticValue]),
    Object(&'static [(&'static str, StaticValue)]),
    IndexedObject(&'static [(&'static str, StaticValue)], &'static [usize]),
}
impl StaticValue {
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }
    pub fn as_bool(&self) -> Option<bool> {
        if let Self::Bool(value) = self {
            Some(*value)
        } else {
            None
        }
    }
    pub fn as_i64(&self) -> Option<i64> {
        if let Self::Integer(value) = self {
            Some(*value)
        } else {
            None
        }
    }
    pub fn as_u64(&self) -> Option<u64> {
        u64::try_from(self.as_i64()?).ok()
    }
    pub fn as_str(&self) -> Option<&'static str> {
        if let Self::String(value) = self {
            Some(value)
        } else {
            None
        }
    }
    pub fn as_array(&self) -> Option<&'static [StaticValue]> {
        if let Self::Array(value) = self {
            Some(value)
        } else {
            None
        }
    }
    pub fn as_object(&self) -> Option<&'static [(&'static str, StaticValue)]> {
        if let Self::Object(value) | Self::IndexedObject(value, _) = self {
            Some(value)
        } else {
            None
        }
    }
    pub fn get(&self, name: &str) -> Option<&'static StaticValue> {
        if let Self::IndexedObject(pairs, indices) = self {
            let at = indices
                .binary_search_by(|index| pairs.get(*index).map(|(key, _)| *key).cmp(&Some(name)))
                .ok()?;
            return pairs.get(*indices.get(at)?).map(|(_, value)| value);
        }
        self.as_object()?
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value)
    }
    pub fn sorted_object(&self) -> Option<StaticObjectIter> {
        let pairs = self.as_object()?;
        let order = match self {
            Self::IndexedObject(_, indices) => ObjectOrder::Indexed(indices.iter()),
            _ => ObjectOrder::Scan(None),
        };
        Some(StaticObjectIter { pairs, order })
    }
    pub fn at(&self, index: usize) -> Option<&'static StaticValue> {
        self.as_array()?.get(index)
    }
}

#[derive(Debug, Clone)]
enum ObjectOrder {
    Indexed(std::slice::Iter<'static, usize>),
    Scan(Option<&'static str>),
}

#[derive(Debug, Clone)]
pub struct StaticObjectIter {
    pairs: &'static [(&'static str, StaticValue)],
    order: ObjectOrder,
}
impl StaticObjectIter {
    fn empty() -> Self {
        Self {
            pairs: &[],
            order: ObjectOrder::Indexed([].iter()),
        }
    }
}
impl Iterator for StaticObjectIter {
    type Item = (&'static str, &'static StaticValue);
    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.order {
            ObjectOrder::Indexed(indices) => {
                for index in indices.by_ref() {
                    if let Some((key, value)) = self.pairs.get(*index) {
                        return Some((*key, value));
                    }
                }
                None
            }
            ObjectOrder::Scan(after) => {
                let (key, value) = self
                    .pairs
                    .iter()
                    .filter(|(key, _)| after.is_none_or(|after| after < *key))
                    .min_by_key(|(key, _)| *key)?;
                *after = Some(key);
                Some((*key, value))
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StaticPath(&'static [StaticValue]);
impl StaticPath {
    fn new(value: &StaticValue) -> Option<Self> {
        let values = value.as_array()?;
        values
            .iter()
            .all(|value| {
                value
                    .as_u64()
                    .and_then(|n| usize::try_from(n).ok())
                    .is_some()
            })
            .then_some(Self(values))
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn get(&self, index: usize) -> Option<usize> {
        usize::try_from(self.0.get(index)?.as_u64()?).ok()
    }
    pub fn iter(&self) -> impl Iterator<Item = usize> {
        self.0
            .iter()
            .filter_map(|value| value.as_u64().and_then(|n| usize::try_from(n).ok()))
    }
}

fn pairs(value: Option<&StaticValue>) -> &'static [(&'static str, StaticValue)] {
    value.and_then(StaticValue::as_object).unwrap_or(&[])
}
fn array(value: Option<&StaticValue>) -> &'static [StaticValue] {
    value.and_then(StaticValue::as_array).unwrap_or(&[])
}
fn text(value: Option<&StaticValue>) -> Option<&'static str> {
    value.and_then(StaticValue::as_str)
}
fn path(value: Option<&StaticValue>) -> Option<StaticPath> {
    value.and_then(StaticPath::new)
}

fn codec(name: Option<&str>) -> Codec {
    match name {
        Some("bool") => Codec::Bool,
        Some("int") => Codec::Int,
        Some("str") => Codec::Str,
        Some("enum") => Codec::Enum,
        Some("lstr") => Codec::LStr,
        Some("ref") => Codec::Ref,
        Some("type") => Codec::Type,
        Some("tvalue") => Codec::TValue,
        Some("mdlist") => Codec::MdList,
        Some("fieldlist") => Codec::FieldList,
        Some("cplinks") => Codec::CpLinks,
        Some("linkbytype") => Codec::LinkByType,
        Some("cparams") => Codec::CParams,
        Some("event") => Codec::Event,
        Some("method") => Codec::Method,
        Some("focontent") => Codec::FoContent,
        Some("cmdgroup") => Codec::CmdGroup,
        Some("picture") => Codec::Picture,
        Some("shortcut") => Codec::Shortcut,
        Some("color") => Codec::Color,
        Some("stylevalue") => Codec::StyleValue,
        Some("xdtotype") => Codec::XdtoType,
        Some("xdtopackages") => Codec::XdtoPackages,
        Some("characteristics") => Codec::Characteristics,
        Some("stdattrs") => Codec::StdAttrs,
        Some("purposes") => Codec::Purposes,
        Some("cacontent") => Codec::CaContent,
        _ => Codec::Unknown,
    }
}

#[derive(Debug, Clone, Copy)]
pub struct StaticPropSpec(&'static StaticValue);
impl StaticPropSpec {
    pub fn from_static(value: &'static StaticValue) -> Option<Self> {
        value.as_object()?;
        Some(Self(value))
    }
    pub fn field(&self, name: &str) -> Option<&'static StaticValue> {
        self.0.get(name)
    }
    pub fn path(&self) -> Option<StaticPath> {
        path(self.field("path"))
    }
    pub fn codec(&self) -> Codec {
        codec(text(self.field("codec")))
    }
}

#[derive(Debug, Clone, Copy)]
pub struct StaticKindSpec(&'static StaticValue);
impl StaticKindSpec {
    pub fn from_static(value: &'static StaticValue) -> Option<Self> {
        value.get("kind")?.as_str()?;
        Some(Self(value))
    }
    pub fn raw(&self) -> &'static StaticValue {
        self.0
    }
    pub fn field(&self, name: &str) -> Option<&'static StaticValue> {
        self.0.get(name)
    }
    pub fn name(&self) -> &'static str {
        text(self.field("kind")).unwrap_or("")
    }
    pub fn class(&self) -> Option<&'static str> {
        text(self.field("class"))
    }
    pub fn folder(&self) -> Option<&'static str> {
        text(self.field("folder"))
    }
    pub fn header(&self) -> Option<StaticPath> {
        path(self.field("header"))
    }
    pub fn uuid(&self) -> Option<StaticPath> {
        path(self.field("uuid"))
    }
    pub fn this_node(&self) -> Option<StaticPath> {
        path(self.field("this_node"))
    }
    pub fn find_header(&self) -> bool {
        self.field("find_header")
            .and_then(StaticValue::as_bool)
            .unwrap_or(false)
    }
    pub fn props(&self) -> impl Iterator<Item = (&'static str, StaticPropSpec)> {
        pairs(self.field("props"))
            .iter()
            .map(|(name, value)| (*name, StaticPropSpec(value)))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaticConfigPath {
    Body(StaticPath),
    Part2,
}

#[derive(Debug, Clone, Copy)]
pub struct StaticConfigProp(&'static StaticValue);
impl StaticConfigProp {
    pub fn name(&self) -> &'static str {
        text(self.0.at(0)).unwrap_or("")
    }
    pub fn path(&self) -> Option<StaticConfigPath> {
        let value = self.0.at(1)?;
        if value.as_str() == Some("part2") {
            Some(StaticConfigPath::Part2)
        } else {
            StaticPath::new(value).map(StaticConfigPath::Body)
        }
    }
    pub fn codec(&self) -> &'static str {
        text(self.0.at(2)).unwrap_or("")
    }
    pub fn extra(&self) -> &'static StaticValue {
        static NULL: StaticValue = StaticValue::Null;
        self.0.at(3).unwrap_or(&NULL)
    }
}

#[derive(Debug)]
pub struct StaticSchema;
static SCHEMA: StaticSchema = StaticSchema;

pub fn static_schema() -> &'static StaticSchema {
    &SCHEMA
}
impl StaticSchema {
    pub fn tables(&self) -> &'static [(&'static str, StaticValue)] {
        data::TABLES
    }
    pub fn table(&self, name: &str) -> Option<&'static StaticValue> {
        data::TABLES
            .binary_search_by(|(key, _)| key.cmp(&name))
            .ok()
            .and_then(|index| data::TABLES.get(index))
            .map(|(_, value)| value)
    }
    pub fn kind(&self, name: &str) -> Option<StaticKindSpec> {
        self.table("kinds.json")?.get(name).map(StaticKindSpec)
    }
    pub fn classes_top(&self) -> impl Iterator<Item = (&'static str, &'static str)> {
        pairs(
            self.table("classes.json")
                .and_then(|value| value.get("top")),
        )
        .iter()
        .filter_map(|(class, value)| value.as_str().map(|kind| (*class, kind)))
    }
    pub fn class_kind(&self, class: &str) -> Option<&'static str> {
        self.classes_top()
            .find(|(key, _)| *key == class)
            .map(|(_, kind)| kind)
    }
    pub fn config_props(&self) -> impl Iterator<Item = StaticConfigProp> {
        array(
            self.table("configuration.json")
                .and_then(|value| value.get("props")),
        )
        .iter()
        .map(StaticConfigProp)
    }
    pub fn config_prop(&self, name: &str) -> Option<StaticConfigProp> {
        self.config_props().find(|prop| prop.name() == name)
    }
}

#[path = "static_tables.rs"]
mod table_views;
pub use table_views::{
    StaticFormContext, StaticFormProp, StaticFormTableValue, StaticFormTables, StaticRightsTables,
    StaticStrings, StaticTemplateTables, static_form_tables, static_form_tables_mode,
    static_rights_tables, static_template_tables,
};
