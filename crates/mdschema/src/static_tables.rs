use super::{StaticObjectIter, StaticValue, array, data, pairs, static_schema, text};
use crate::forms::FormCompat;
use std::cmp::Ordering;
use std::iter::Peekable;

static NULL: StaticValue = StaticValue::Null;
fn table(name: &str) -> &'static StaticValue {
    static_schema().table(name).unwrap_or(&NULL)
}
fn sorted(value: Option<&StaticValue>) -> StaticObjectIter {
    value
        .and_then(StaticValue::sorted_object)
        .unwrap_or_else(StaticObjectIter::empty)
}
fn truthy(value: Option<&StaticValue>) -> bool {
    match value {
        None | Some(StaticValue::Null) => false,
        Some(StaticValue::Bool(value)) => *value,
        Some(StaticValue::Integer(value)) => *value != 0,
        Some(StaticValue::String(value)) => !value.is_empty(),
        Some(StaticValue::Array(value)) => !value.is_empty(),
        Some(StaticValue::Object(value) | StaticValue::IndexedObject(value, _)) => {
            !value.is_empty()
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct StaticStrings(&'static [StaticValue]);
impl StaticStrings {
    pub fn iter(&self) -> impl Iterator<Item = &'static str> {
        self.0.iter().filter_map(StaticValue::as_str)
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

struct MergeObjects {
    base: Peekable<StaticObjectIter>,
    overlay: Peekable<StaticObjectIter>,
}
impl Iterator for MergeObjects {
    type Item = (
        &'static str,
        Option<&'static StaticValue>,
        Option<&'static StaticValue>,
    );
    fn next(&mut self) -> Option<Self::Item> {
        match (
            self.base.peek().map(|(key, _)| *key),
            self.overlay.peek().map(|(key, _)| *key),
        ) {
            (None, None) => None,
            (Some(_), None) => self
                .base
                .next()
                .map(|(key, value)| (key, Some(value), None)),
            (None, Some(_)) => self
                .overlay
                .next()
                .map(|(key, value)| (key, None, Some(value))),
            (Some(first), Some(second)) => match first.cmp(second) {
                Ordering::Less => self
                    .base
                    .next()
                    .map(|(key, value)| (key, Some(value), None)),
                Ordering::Greater => self
                    .overlay
                    .next()
                    .map(|(key, value)| (key, None, Some(value))),
                Ordering::Equal => {
                    let (key, base) = self.base.next()?;
                    let (_, overlay) = self.overlay.next()?;
                    Some((key, Some(base), Some(overlay)))
                }
            },
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct StaticFormTableValue(&'static StaticValue);
impl StaticFormTableValue {
    pub fn raw(&self) -> &'static StaticValue {
        self.0
    }
    pub fn parsed(&self) -> Option<&'static StaticValue> {
        let raw = self.0.as_str()?;
        let index = data::PARSED_FORM_VALUES
            .binary_search_by(|(key, _)| key.cmp(&raw))
            .ok()?;
        data::PARSED_FORM_VALUES.get(index).map(|(_, value)| value)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct StaticFormProp(&'static StaticValue);
impl StaticFormProp {
    pub fn raw(&self) -> &'static StaticValue {
        self.0
    }
    pub fn path(&self) -> Option<&'static [StaticValue]> {
        self.0.get("path").and_then(StaticValue::as_array)
    }
    pub fn codec(&self) -> &'static str {
        text(self.0.get("codec")).unwrap_or("")
    }
    pub fn default(&self) -> &'static [StaticValue] {
        array(self.0.get("default"))
    }
    pub fn moved(&self) -> bool {
        truthy(self.0.get("moved"))
    }
    pub fn table(&self) -> impl Iterator<Item = (&'static str, StaticFormTableValue)> + use<> {
        sorted(self.0.get("table")).map(|(key, value)| (key, StaticFormTableValue(value)))
    }
    pub fn table_value(&self, key: &str) -> Option<StaticFormTableValue> {
        self.0.get("table")?.get(key).map(StaticFormTableValue)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct StaticFormContext {
    base: Option<&'static StaticValue>,
    overlay: Option<&'static StaticValue>,
    compatibility: Option<&'static StaticValue>,
    v85: bool,
}
impl StaticFormContext {
    pub fn order(&self) -> impl Iterator<Item = &'static str> + use<> {
        let overlay = self
            .overlay
            .and_then(|value| value.get("order"))
            .filter(|value| value.as_array().is_some_and(|items| !items.is_empty()));
        array(overlay.or_else(|| self.base.and_then(|value| value.get("order"))))
            .iter()
            .filter_map(StaticValue::as_str)
    }
    pub fn props(&self) -> impl Iterator<Item = (&'static str, StaticFormProp)> + use<> {
        let base = self.base.and_then(|value| value.get("props"));
        let overlay = self.overlay.and_then(|value| value.get("props"));
        let v85 = self.v85;
        let compatibility = self.compatibility;
        pairs(base)
            .iter()
            .map(move |(key, value)| {
                let replacement = overlay
                    .and_then(|overlay| overlay.get(key))
                    .filter(|value| v85 || !truthy(value.get("moved")));
                (
                    *key,
                    StaticFormProp(
                        compatibility
                            .and_then(|v| v.get(key))
                            .or(replacement)
                            .unwrap_or(value),
                    ),
                )
            })
            .chain(
                pairs(overlay)
                    .iter()
                    .filter(move |(key, value)| {
                        base.and_then(|base| base.get(key)).is_none()
                            && (v85 || !truthy(value.get("moved")))
                    })
                    .map(move |(key, value)| {
                        (
                            *key,
                            StaticFormProp(compatibility.and_then(|v| v.get(key)).unwrap_or(value)),
                        )
                    }),
            )
    }
}

#[derive(Debug, Clone, Copy)]
pub struct StaticFormTables {
    v85: bool,
    compat: FormCompat,
}
pub fn static_form_tables(v85: bool) -> StaticFormTables {
    static_form_tables_mode(v85, FormCompat::Base)
}
pub fn static_form_tables_mode(v85: bool, compat: FormCompat) -> StaticFormTables {
    StaticFormTables { v85, compat }
}
impl StaticFormTables {
    fn compatibility(&self) -> Option<&'static StaticValue> {
        (self.compat == FormCompat::V8_3_12)
            .then(|| {
                data::COMPAT_FORM_PROPERTIES
                    .iter()
                    .find(|(v85, _)| *v85 == self.v85)
                    .map(|(_, value)| value)
            })
            .flatten()
    }
    pub fn contexts(&self) -> impl Iterator<Item = (&'static str, StaticFormContext)> + use<> {
        let v85 = self.v85;
        let compatibility = self.compatibility();
        MergeObjects {
            base: sorted(Some(table("forms/learned.json"))).peekable(),
            overlay: sorted(Some(table("forms/learned85.json"))).peekable(),
        }
        .map(move |(key, base, overlay)| {
            (
                key,
                StaticFormContext {
                    base,
                    overlay,
                    v85,
                    compatibility: compatibility.and_then(|value| value.get(key)),
                },
            )
        })
    }
    pub fn context(&self, name: &str) -> Option<StaticFormContext> {
        let base = table("forms/learned.json").get(name);
        let overlay = table("forms/learned85.json").get(name);
        (base.is_some() || overlay.is_some()).then_some(StaticFormContext {
            base,
            overlay,
            v85: self.v85,
            compatibility: self.compatibility().and_then(|value| value.get(name)),
        })
    }
    pub fn paths(&self) -> &'static StaticValue {
        table("forms/paths.json")
    }
    pub fn events(&self) -> &'static StaticValue {
        table("forms/events.json")
    }
    pub fn misc(&self) -> &'static StaticValue {
        table("forms/misc.json")
    }
    pub fn types(&self) -> &'static StaticValue {
        if self.compat == FormCompat::V8_3_12 {
            &data::COMPAT_FORM_TYPES
        } else {
            table("forms/types.json")
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct StaticRightsTables;
pub fn static_rights_tables() -> StaticRightsTables {
    StaticRightsTables
}
impl StaticRightsTables {
    pub fn rights(&self) -> impl Iterator<Item = (&'static str, &'static str)> {
        pairs(table("rights/rights.json").get("rights"))
            .iter()
            .filter_map(|(key, value)| value.as_str().map(|value| (*key, value)))
    }
    pub fn rights_by_kind(&self) -> impl Iterator<Item = (&'static str, StaticStrings)> {
        pairs(Some(table("rights/rights_by_kind.json")))
            .iter()
            .filter(|(key, _)| *key != "_doc")
            .filter_map(|(key, value)| value.as_array().map(|items| (*key, StaticStrings(items))))
    }
    pub fn cmdi(&self) -> &'static StaticValue {
        table("rights/cmdi.json")
    }
    pub fn configdump(&self) -> &'static StaticValue {
        table("rights/configdump.json")
    }
}

#[derive(Debug, Clone, Copy)]
pub struct StaticTemplateTables;
pub fn static_template_tables() -> StaticTemplateTables {
    StaticTemplateTables
}
impl StaticTemplateTables {
    pub fn mxl(&self) -> &'static StaticValue {
        table("templates/tables.json")
    }
    pub fn chart(&self) -> &'static StaticValue {
        table("templates/chart_tables.json")
    }
    pub fn type_guid_expanded(
        &self,
        name: crate::ExpandedTypeName<'_>,
        set: bool,
    ) -> Option<&'static str> {
        crate::type_names::unique_type_guid(
            crate::static_platform_types(table("types.json"))?,
            name,
            set,
        )
    }
    pub fn type_guid(&self, name: &str) -> Option<&'static str> {
        pairs(Some(table("types.json")))
            .iter()
            .rev()
            .find(|(_, value)| {
                value.at(2).is_none_or(StaticValue::is_null) && text(value.at(1)) == Some(name)
            })
            .map(|(guid, _)| *guid)
    }
}

impl super::StaticSchema {
    pub fn build_event(&self, name: &str) -> Option<&'static str> {
        text(table("build/events.json").get(name))
    }
}

impl super::StaticSchema {
    pub fn platform_type(&self, guid: &str) -> Option<crate::BorrowedPlatformType<'static>> {
        crate::static_platform_types(self.table("types.json")?)?
            .find(|(key, _)| *key == guid)
            .map(|(_, ty)| ty)
    }
}
