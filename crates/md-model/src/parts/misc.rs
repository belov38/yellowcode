use crate::{EntryData, MdObject, MdRef};
use brace::Node;

#[doc(hidden)]
#[derive(Clone)]
pub struct MiscSource(#[doc(hidden)] pub SourceKind);
#[doc(hidden)]
#[derive(Clone)]
pub enum SourceKind {
    Native(Node),
    Managed {
        entry: memory_budget::Shared<EntryData>,
        child: Option<usize>,
    },
}
impl MiscSource {
    #[doc(hidden)]
    pub fn native(node: Node) -> Self {
        Self(SourceKind::Native(node))
    }
    #[doc(hidden)]
    pub fn managed(entry: memory_budget::Shared<EntryData>, child: Option<usize>) -> Option<Self> {
        let source = Self(SourceKind::Managed { entry, child });
        source.node()?.as_list()?;
        Some(source)
    }
    #[doc(hidden)]
    pub fn node(&self) -> Option<&Node> {
        match &self.0 {
            SourceKind::Native(node) => Some(node),
            SourceKind::Managed { entry, child } => {
                let EntryData::Brace(doc) = &**entry else {
                    return None;
                };
                match child {
                    None => Some(&doc.root),
                    Some(at) => doc.root.as_list()?.get(*at),
                }
            }
        }
    }
}
impl std::fmt::Debug for MiscSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.node() {
            Some(node) => node.fmt(f),
            None => f.write_str("<invalid source>"),
        }
    }
}
impl PartialEq for MiscSource {
    fn eq(&self, other: &Self) -> bool {
        matches!((self.node(), other.node()), (Some(a), Some(b)) if a == b)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Schedule {
    pub begin_date: String,
    pub end_date: String,
    pub begin_time: String,
    pub end_time: String,
    pub completion_time: String,
    pub completion_interval: i64,
    pub repeat_period_in_day: i64,
    pub repeat_pause: i64,
    pub week_days: Vec<i64>,
    pub week_day_in_month: i64,
    pub day_in_month: i64,
    pub months: Vec<i64>,
    pub weeks_period: i64,
    pub days_repeat_period: i64,
    pub detailed: Vec<Schedule>,
    #[doc(hidden)]
    pub source: MiscSource,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ContentItem {
    pub object: MdRef,
    pub auto_record: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Style {
    #[doc(hidden)]
    pub source: MiscSource,
}

impl MdObject {
    pub fn exchange_content(&self) -> Option<&[ContentItem]> {
        self.parts.exchange_content.as_deref()
    }
}
