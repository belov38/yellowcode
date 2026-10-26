use std::sync::Arc;

use crate::{CodecState, EntryData};

#[doc(hidden)]
#[derive(Debug, Clone)]
pub enum FormSource {
    Doc(Arc<EntryData>),
    Text(Arc<[u8]>),
    ManagedDoc(memory_budget::Shared<EntryData>),
    ManagedText(memory_budget::Shared<Vec<u8>>),
}

impl FormSource {
    #[doc(hidden)]
    pub fn entry(&self) -> Option<&EntryData> {
        match self {
            Self::Doc(value) => Some(value),
            Self::ManagedDoc(value) => Some(value),
            _ => None,
        }
    }
    #[doc(hidden)]
    pub fn bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Text(value) => Some(value),
            Self::ManagedText(value) => Some(value),
            _ => None,
        }
    }
}
impl PartialEq for FormSource {
    fn eq(&self, other: &Self) -> bool {
        if matches!((self, other), (Self::Doc(a), Self::Doc(b)) if Arc::ptr_eq(a, b))
            || matches!((self, other), (Self::ManagedDoc(a), Self::ManagedDoc(b)) if memory_budget::Shared::ptr_eq(a, b))
        {
            return true;
        }
        match (self.entry(), other.entry()) {
            (Some(a), Some(b)) => a == b,
            (None, None) => self.bytes() == other.bytes(),
            _ => false,
        }
    }
}

#[doc(hidden)]
#[derive(Debug, Clone)]
pub enum FormShared {
    Native(Arc<dyn CodecState>),
    Managed(memory_budget::ErasedShared),
}

impl PartialEq for FormShared {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Form {
    pub items: Vec<FormItem>,
    pub attributes: Vec<FormAttribute>,
    pub commands: Vec<FormCommand>,
    pub parameters: Vec<FormParameter>,
    pub events: Vec<(String, String)>,
    #[doc(hidden)]
    pub source: Option<FormSource>,
    #[doc(hidden)]
    pub shared: Option<FormShared>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct FormItem {
    pub id: i64,
    pub name: String,
    pub tag: String,
    pub children: Vec<FormItem>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct FormAttribute {
    pub id: i64,
    pub name: String,
    pub main: bool,
    pub types: Vec<FormAttributeType>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct FormCommand {
    pub id: i64,
    pub name: String,
    pub action: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct FormParameter {
    pub name: String,
    pub key: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormAttributeType {
    Id(brace::Guid),
    Other,
    Invalid,
}
