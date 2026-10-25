use mdschema::forms::{Context, FormTables, PropEntry, TableValue, Value};
use mdschema::{StaticFormContext, StaticFormProp, StaticFormTables, StaticValue};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy)]
pub(crate) enum JsonRef<'a> {
    Native(&'a Value),
    Embedded(&'static StaticValue),
}
impl<'a> JsonRef<'a> {
    pub fn get(self, key: &str) -> Option<Self> {
        match self {
            Self::Native(value) => value.get(key).map(Self::Native),
            Self::Embedded(value) => value.get(key).map(Self::Embedded),
        }
    }
    pub fn is_null(self) -> bool {
        match self {
            Self::Native(value) => value.is_null(),
            Self::Embedded(value) => value.is_null(),
        }
    }
    pub fn is_object(self) -> bool {
        self.as_object().is_some()
    }
    pub fn as_str(self) -> Option<&'a str> {
        match self {
            Self::Native(value) => value.as_str(),
            Self::Embedded(value) => value.as_str(),
        }
    }
    pub fn as_bool(self) -> Option<bool> {
        match self {
            Self::Native(value) => value.as_bool(),
            Self::Embedded(value) => value.as_bool(),
        }
    }
    pub fn as_i64(self) -> Option<i64> {
        match self {
            Self::Native(value) => value.as_i64(),
            Self::Embedded(value) => value.as_i64(),
        }
    }
    pub fn as_f64(self) -> Option<f64> {
        match self {
            Self::Native(value) => value.as_f64(),
            Self::Embedded(value) => value.as_i64().map(|value| value as f64),
        }
    }
    pub fn as_array(self) -> Option<ArrayRef<'a>> {
        match self {
            Self::Native(value) => value.as_array().map(|v| ArrayRef::Native(v.as_slice())),
            Self::Embedded(value) => value.as_array().map(ArrayRef::Embedded),
        }
    }
    pub fn as_object(self) -> Option<ObjectRef<'a>> {
        match self {
            Self::Native(value) => value.as_object().map(|_| ObjectRef::Native(value)),
            Self::Embedded(value) => value.as_object().map(ObjectRef::Embedded),
        }
    }
}
#[derive(Debug, Clone, Copy)]
pub(crate) enum ArrayRef<'a> {
    Native(&'a [Value]),
    Embedded(&'static [StaticValue]),
}
impl<'a> ArrayRef<'a> {
    pub fn len(self) -> usize {
        match self {
            Self::Native(v) => v.len(),
            Self::Embedded(v) => v.len(),
        }
    }
    pub fn is_empty(self) -> bool {
        self.len() == 0
    }
    pub fn get(self, index: usize) -> Option<JsonRef<'a>> {
        match self {
            Self::Native(v) => v.get(index).map(JsonRef::Native),
            Self::Embedded(v) => v.get(index).map(JsonRef::Embedded),
        }
    }
    pub fn first(self) -> Option<JsonRef<'a>> {
        self.get(0)
    }
    pub fn iter(self) -> impl Iterator<Item = JsonRef<'a>> {
        let native = match self {
            Self::Native(v) => Some(v),
            _ => None,
        };
        let embedded = match self {
            Self::Embedded(v) => Some(v),
            _ => None,
        };
        native
            .into_iter()
            .flatten()
            .map(JsonRef::Native)
            .chain(embedded.into_iter().flatten().map(JsonRef::Embedded))
    }
}
#[derive(Debug, Clone, Copy)]
pub(crate) enum ObjectRef<'a> {
    Native(&'a Value),
    Embedded(&'static [(&'static str, StaticValue)]),
}
impl<'a> ObjectRef<'a> {
    pub fn len(self) -> usize {
        match self {
            Self::Native(v) => v.as_object().map_or(0, |v| v.len()),
            Self::Embedded(v) => v.len(),
        }
    }
    pub fn is_empty(self) -> bool {
        self.len() == 0
    }
    pub fn iter(self) -> impl Iterator<Item = (&'a str, JsonRef<'a>)> {
        let native = match self {
            Self::Native(v) => Some(v),
            _ => None,
        };
        let embedded = match self {
            Self::Embedded(v) => Some(v),
            _ => None,
        };
        native
            .into_iter()
            .flat_map(|v| v.as_object().into_iter().flat_map(|v| v.iter()))
            .map(|(key, value)| (key.as_str(), JsonRef::Native(value)))
            .chain(
                embedded
                    .into_iter()
                    .flatten()
                    .map(|(key, value)| (*key, JsonRef::Embedded(value))),
            )
    }
    pub fn keys(self) -> impl Iterator<Item = &'a str> {
        self.iter().map(|(key, _)| key)
    }
}
#[derive(Debug, Clone, Copy)]
pub(crate) struct TableValueRef<'a> {
    pub raw: JsonRef<'a>,
    pub parsed: Option<JsonRef<'a>>,
}
#[derive(Debug, Clone, Copy)]
pub(crate) enum TableEntriesRef<'a> {
    Native(&'a BTreeMap<String, TableValue>),
    Embedded(StaticFormProp),
}
impl<'a> TableEntriesRef<'a> {
    pub fn get(self, key: &str) -> Option<TableValueRef<'a>> {
        match self {
            Self::Native(v) => v.get(key).map(|v| TableValueRef {
                raw: JsonRef::Native(&v.raw),
                parsed: v.parsed.as_ref().map(JsonRef::Native),
            }),
            Self::Embedded(v) => v.table_value(key).map(|v| TableValueRef {
                raw: JsonRef::Embedded(v.raw()),
                parsed: v.parsed().map(JsonRef::Embedded),
            }),
        }
    }
}
#[derive(Debug, Clone, Copy)]
pub(crate) struct PropRef<'a> {
    pub path: Option<ArrayRef<'a>>,
    pub codec: &'a str,
    pub table: TableEntriesRef<'a>,
    pub default: ArrayRef<'a>,
    #[cfg(test)]
    pub moved: bool,
    pub raw: JsonRef<'a>,
}
impl<'a> PropRef<'a> {
    fn native(value: &'a PropEntry) -> Self {
        Self {
            path: value.path.as_deref().map(ArrayRef::Native),
            codec: &value.codec,
            table: TableEntriesRef::Native(&value.table),
            default: ArrayRef::Native(&value.default),
            #[cfg(test)]
            moved: value.moved,
            raw: JsonRef::Native(&value.raw),
        }
    }
    fn embedded(value: StaticFormProp) -> Self {
        Self {
            path: value.path().map(ArrayRef::Embedded),
            codec: value.codec(),
            table: TableEntriesRef::Embedded(value),
            default: ArrayRef::Embedded(value.default()),
            #[cfg(test)]
            moved: value.moved(),
            raw: JsonRef::Embedded(value.raw()),
        }
    }
}
#[derive(Debug, Clone, Copy)]
pub(crate) enum StringsRef<'a> {
    Native(&'a [String]),
    Embedded(StaticFormContext),
}
impl<'a> StringsRef<'a> {
    pub fn iter(self) -> impl Iterator<Item = &'a str> {
        let native = match self {
            Self::Native(v) => Some(v),
            _ => None,
        };
        let embedded = match self {
            Self::Embedded(v) => Some(v),
            _ => None,
        };
        native.into_iter().flatten().map(String::as_str).chain(
            embedded
                .into_iter()
                .flat_map(|v| v.order())
                .map(|value| -> &'a str { value }),
        )
    }
}
#[derive(Debug, Clone, Copy)]
pub(crate) enum PropsRef<'a> {
    Native(&'a [(String, PropEntry)]),
    Embedded(StaticFormContext),
}
impl<'a> PropsRef<'a> {
    pub fn iter(self) -> impl Iterator<Item = (&'a str, PropRef<'a>)> {
        let native = match self {
            Self::Native(v) => Some(v),
            _ => None,
        };
        let embedded = match self {
            Self::Embedded(v) => Some(v),
            _ => None,
        };
        native
            .into_iter()
            .flatten()
            .map(|(key, value)| (key.as_str(), PropRef::native(value)))
            .chain(
                embedded
                    .into_iter()
                    .flat_map(|v| v.props())
                    .map(|(key, value)| (key, PropRef::embedded(value))),
            )
    }
    pub fn get(self, index: usize) -> Option<(&'a str, PropRef<'a>)> {
        self.iter().nth(index)
    }
}
#[derive(Debug, Clone, Copy)]
pub(crate) struct ContextRef<'a> {
    pub order: StringsRef<'a>,
    pub props: PropsRef<'a>,
}
impl<'a> ContextRef<'a> {
    fn native(value: &'a Context) -> Self {
        Self {
            order: StringsRef::Native(&value.order),
            props: PropsRef::Native(&value.props),
        }
    }
    fn embedded(value: StaticFormContext) -> Self {
        Self {
            order: StringsRef::Embedded(value),
            props: PropsRef::Embedded(value),
        }
    }
}
#[derive(Debug, Clone, Copy)]
pub(crate) enum ContextsRef<'a> {
    Native(&'a BTreeMap<String, Context>),
    Embedded(StaticFormTables),
}
impl<'a> ContextsRef<'a> {
    pub fn get(self, key: &str) -> Option<ContextRef<'a>> {
        match self {
            Self::Native(v) => v.get(key).map(ContextRef::native),
            Self::Embedded(v) => v.context(key).map(ContextRef::embedded),
        }
    }
    pub fn contains_key(self, key: &str) -> bool {
        self.get(key).is_some()
    }
    pub fn iter(self) -> impl Iterator<Item = (&'a str, ContextRef<'a>)> {
        let native = match self {
            Self::Native(v) => Some(v),
            _ => None,
        };
        let embedded = match self {
            Self::Embedded(v) => Some(v),
            _ => None,
        };
        native
            .into_iter()
            .flat_map(|v| v.iter())
            .map(|(key, value)| (key.as_str(), ContextRef::native(value)))
            .chain(
                embedded
                    .into_iter()
                    .flat_map(|v| v.contexts())
                    .map(|(key, value)| (key, ContextRef::embedded(value))),
            )
    }
}
#[derive(Debug, Clone, Copy)]
pub(crate) struct TableView<'a> {
    pub learned: ContextsRef<'a>,
    pub paths: JsonRef<'a>,
    pub events: JsonRef<'a>,
    pub misc: JsonRef<'a>,
    pub types: JsonRef<'a>,
}
impl<'a> TableView<'a> {
    pub fn native(value: &'a FormTables) -> Self {
        Self {
            learned: ContextsRef::Native(&value.learned),
            paths: JsonRef::Native(&value.paths),
            events: JsonRef::Native(&value.events),
            misc: JsonRef::Native(&value.misc),
            types: JsonRef::Native(&value.types),
        }
    }
    pub fn embedded(v85: bool) -> Self {
        let value = mdschema::static_form_tables(v85);
        Self {
            learned: ContextsRef::Embedded(value),
            paths: JsonRef::Embedded(value.paths()),
            events: JsonRef::Embedded(value.events()),
            misc: JsonRef::Embedded(value.misc()),
            types: JsonRef::Embedded(value.types()),
        }
    }
}
