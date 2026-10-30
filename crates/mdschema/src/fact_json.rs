use crate::StaticValue;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Number, Value};
#[derive(Debug, Clone)]
pub enum FactJson {
    Embedded(&'static StaticValue),
    Legacy(Value),
}
#[derive(Debug, Clone, Copy)]
pub enum FactValue<'a> {
    Embedded(&'a StaticValue),
    Legacy(&'a Value),
}
#[derive(Debug, Clone, Copy)]
pub enum FactObject<'a> {
    Embedded(&'a [(&'static str, StaticValue)]),
    Legacy(&'a serde_json::Map<String, Value>),
}
#[derive(Debug, Clone, Copy)]
pub enum FactArray<'a> {
    Embedded(&'a [StaticValue]),
    Legacy(&'a [Value]),
}
impl FactJson {
    pub fn view(&self) -> FactValue<'_> {
        match self {
            Self::Embedded(v) => FactValue::Embedded(v),
            Self::Legacy(v) => FactValue::Legacy(v),
        }
    }
    pub fn get(&self, key: &str) -> Option<FactValue<'_>> {
        self.view().get(key)
    }
    pub fn is_object(&self) -> bool {
        self.view().object().is_some()
    }
}
impl Default for FactJson {
    fn default() -> Self {
        Self::Embedded(&StaticValue::Null)
    }
}
impl From<Value> for FactJson {
    fn from(v: Value) -> Self {
        Self::Legacy(v)
    }
}
impl<'de> Deserialize<'de> for FactJson {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Value::deserialize(d).map(Self::Legacy)
    }
}
impl<'a> FactValue<'a> {
    pub fn is_null(self) -> bool {
        match self {
            Self::Embedded(v) => v.is_null(),
            Self::Legacy(v) => v.is_null(),
        }
    }
    pub fn as_str(self) -> Option<&'a str> {
        match self {
            Self::Embedded(v) => v.as_str(),
            Self::Legacy(v) => v.as_str(),
        }
    }
    pub fn as_bool(self) -> Option<bool> {
        match self {
            Self::Embedded(v) => v.as_bool(),
            Self::Legacy(v) => v.as_bool(),
        }
    }
    pub fn as_i64(self) -> Option<i64> {
        match self {
            Self::Embedded(v) => v.as_i64(),
            Self::Legacy(v) => v.as_i64(),
        }
    }
    pub fn as_u64(self) -> Option<u64> {
        match self {
            Self::Embedded(v) => v.as_u64(),
            Self::Legacy(v) => v.as_u64(),
        }
    }
    pub fn number(self) -> Option<Number> {
        match self {
            Self::Embedded(StaticValue::Integer(n)) => Some((*n).into()),
            Self::Legacy(Value::Number(n)) => Some(n.clone()),
            _ => None,
        }
    }
    pub fn object(self) -> Option<FactObject<'a>> {
        match self {
            Self::Embedded(v) => v.as_object().map(FactObject::Embedded),
            Self::Legacy(v) => v.as_object().map(FactObject::Legacy),
        }
    }
    pub fn as_object(self) -> Option<FactObject<'a>> {
        self.object()
    }
    pub fn array(self) -> Option<FactArray<'a>> {
        match self {
            Self::Embedded(v) => v.as_array().map(FactArray::Embedded),
            Self::Legacy(v) => v.as_array().map(|v| FactArray::Legacy(v)),
        }
    }
    pub fn as_array(self) -> Option<FactArray<'a>> {
        self.array()
    }
    pub fn get(self, key: &str) -> Option<Self> {
        match self {
            Self::Embedded(v) => v.get(key).map(Self::Embedded),
            Self::Legacy(v) => v.get(key).map(Self::Legacy),
        }
    }
    pub fn at(self, i: usize) -> Option<Self> {
        self.array()?.get(i)
    }
}
impl<'a> FactObject<'a> {
    pub fn len(self) -> usize {
        match self {
            Self::Embedded(v) => v.len(),
            Self::Legacy(v) => v.len(),
        }
    }
    pub fn is_empty(self) -> bool {
        self.len() == 0
    }
    pub fn iter(self) -> impl Iterator<Item = (&'a str, FactValue<'a>)> {
        let n = self.len();
        (0..n).filter_map(move |i| match self {
            Self::Embedded(v) => v.get(i).map(|(k, v)| (*k, FactValue::Embedded(v))),
            Self::Legacy(v) => v
                .iter()
                .nth(i)
                .map(|(k, v)| (k.as_str(), FactValue::Legacy(v))),
        })
    }
    pub fn get(self, key: &str) -> Option<FactValue<'a>> {
        match self {
            Self::Embedded(v) => v
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| FactValue::Embedded(v)),
            Self::Legacy(v) => v.get(key).map(FactValue::Legacy),
        }
    }
}
impl<'a> IntoIterator for FactObject<'a> {
    type Item = (&'a str, FactValue<'a>);
    type IntoIter = FactObjectIter<'a>;
    fn into_iter(self) -> Self::IntoIter {
        FactObjectIter {
            value: self,
            index: 0,
        }
    }
}
pub struct FactObjectIter<'a> {
    value: FactObject<'a>,
    index: usize,
}
impl<'a> Iterator for FactObjectIter<'a> {
    type Item = (&'a str, FactValue<'a>);
    fn next(&mut self) -> Option<Self::Item> {
        let i = self.index;
        self.index += 1;
        match self.value {
            FactObject::Embedded(v) => v.get(i).map(|(k, v)| (*k, FactValue::Embedded(v))),
            FactObject::Legacy(v) => v
                .iter()
                .nth(i)
                .map(|(k, v)| (k.as_str(), FactValue::Legacy(v))),
        }
    }
}
impl<'a> FactArray<'a> {
    pub fn len(self) -> usize {
        match self {
            Self::Embedded(v) => v.len(),
            Self::Legacy(v) => v.len(),
        }
    }
    pub fn is_empty(self) -> bool {
        self.len() == 0
    }
    pub fn get(self, i: usize) -> Option<FactValue<'a>> {
        match self {
            Self::Embedded(v) => v.get(i).map(FactValue::Embedded),
            Self::Legacy(v) => v.get(i).map(FactValue::Legacy),
        }
    }
    pub fn iter(self) -> impl Iterator<Item = FactValue<'a>> {
        (0..self.len()).filter_map(move |i| self.get(i))
    }
}
impl PartialEq for FactValue<'_> {
    fn eq(&self, other: &Self) -> bool {
        if let Some(a) = self.object() {
            return other.object().is_some_and(|b| {
                a.len() == b.len() && a.iter().all(|(k, v)| b.get(k).is_some_and(|w| v == w))
            });
        }
        if let Some(a) = self.array() {
            return other.array().is_some_and(|b| {
                a.len() == b.len() && a.iter().zip(b.iter()).all(|(v, w)| v == w)
            });
        }
        if let Some(a) = self.number() {
            return other.number().is_some_and(|b| a == b);
        }
        if let Some(a) = self.as_str() {
            return other.as_str() == Some(a);
        }
        if let Some(a) = self.as_bool() {
            return other.as_bool() == Some(a);
        }
        self.is_null() && other.is_null()
    }
}
impl PartialEq for FactJson {
    fn eq(&self, b: &Self) -> bool {
        self.view() == b.view()
    }
}
impl PartialEq<Value> for FactJson {
    fn eq(&self, b: &Value) -> bool {
        self.view() == FactValue::Legacy(b)
    }
}
impl PartialEq<FactJson> for Value {
    fn eq(&self, b: &FactJson) -> bool {
        b == self
    }
}
impl Serialize for FactJson {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.view().serialize(s)
    }
}
impl Serialize for FactValue<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if let Self::Legacy(v) = self {
            return v.serialize(s);
        }
        if let Some(v) = self.object() {
            return s.collect_map(v.iter());
        }
        if let Some(v) = self.array() {
            return s.collect_seq(v.iter());
        }
        if let Some(v) = self.number() {
            return v.serialize(s);
        }
        if let Some(v) = self.as_str() {
            return s.serialize_str(v);
        }
        if let Some(v) = self.as_bool() {
            return s.serialize_bool(v);
        }
        s.serialize_none()
    }
}
