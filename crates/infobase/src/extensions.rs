mod cas;
pub mod cfe;
mod compose;
pub(crate) use compose::{ComposeError, MeteredComposeInput, compose, compose_metered};
mod control;
mod logic;
mod row;
mod schema;
mod source;
mod zipped;
use brace::Guid;
pub use control::{Control, Stop, path_open_scratch};
pub use logic::{ExtensionFacts, ExtensionLogic, Intercept, scan_intercepts};
use memory_budget::{BudgetError, BudgetedText, BudgetedVec, Category, MemoryBudget};
use onecd::DateTime;
pub use row::dt_generation;
pub use schema::Projection;
use serde::{Serialize, Serializer, ser::SerializeStruct};
pub use source::ReadSource;
pub use zipped::{ZippedInfo, zipped_info};

pub const FEATURE_BYTES: u64 = 64 << 20;
pub const SCHEMA_BYTES: u64 = 4 << 20;
pub const ROW_BYTES: u64 = 1 << 20;
pub const MAX_ITEMS: usize = 256;
pub const MAX_NAMES: usize = 256 << 10;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("source changed since it was opened")]
    Changed,
    #[error("{0}")]
    Memory(#[from] BudgetError),
    #[error("{0}")]
    Stopped(#[from] Stop),
    #[error("source read failed ({0:?})")]
    Io(std::io::ErrorKind),
    #[error("invalid installed inventory: {0}")]
    Invalid(&'static str),
    #[error("unsupported installed inventory: {0}")]
    Unsupported(&'static str),
    #[error("installed inventory limit exceeded: {0}")]
    Limit(&'static str),
}
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub struct Item {
    pub id: Guid,
    pub name: BudgetedText,
    pub order: i64,
    pub purpose_code: i64,
    pub scope_code: i64,
    pub update_time: Option<DateTime>,
    pub used_in_distributed_infobase: bool,
}
struct Text<T>(T);
impl<T: std::fmt::Display> Serialize for Text<T> {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.collect_str(&self.0)
    }
}
impl Serialize for Item {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let mut out = s.serialize_struct("InstalledExtension", 7)?;
        out.serialize_field("id", &Text(self.id))?;
        out.serialize_field("name", self.name.as_str())?;
        out.serialize_field("order", &self.order)?;
        out.serialize_field("purpose_code", &self.purpose_code)?;
        out.serialize_field("scope_code", &self.scope_code)?;
        out.serialize_field("update_time", &self.update_time.map(Text))?;
        out.serialize_field(
            "used_in_distributed_infobase",
            &self.used_in_distributed_infobase,
        )?;
        out.end()
    }
}
pub struct Inventory {
    items: BudgetedVec<Item>,
    names: usize,
}
impl Inventory {
    pub fn new(budget: &MemoryBudget) -> Result<Self> {
        Ok(Self {
            items: BudgetedVec::new(budget, Category::Metadata)?,
            names: 0,
        })
    }
    pub fn len(&self) -> usize {
        self.items.len()
    }
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
    pub fn items(&self) -> &[Item] {
        &self.items
    }
    pub(crate) fn push(&mut self, item: Item) -> Result<()> {
        if self.items.len() >= MAX_ITEMS {
            return Err(Error::Limit("extension count"));
        }
        if item.id == Guid::NIL || self.items.iter().any(|x| x.id == item.id) {
            return Err(Error::Invalid("empty or duplicate extension ID"));
        }
        let names = self
            .names
            .checked_add(item.name.as_str().len())
            .ok_or(BudgetError::Overflow)?;
        if names > MAX_NAMES {
            return Err(Error::Limit("aggregate names"));
        }
        self.items.try_push(item)?;
        self.names = names;
        Ok(())
    }
    pub fn finish(&mut self) {
        self.items
            .as_mut_slice()
            .sort_unstable_by_key(|x| (x.order, x.id));
    }
}
impl Serialize for Inventory {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.collect_seq(self.items.iter())
    }
}

const GENERATION_BYTES: usize = zipped::ZIPPED_BYTES;

pub fn generation_key(bytes: &[u8], budget: &MemoryBudget) -> Result<Option<[u8; 20]>> {
    use brace::binary::{AtomValue, BinNode};
    let malformed = || Error::Invalid("malformed generation stream");
    let body = zipped::read_list(bytes, budget)?;
    let [
        BinNode::Atom(version),
        BinNode::List(locator),
        _,
        _,
        _,
        _,
        _,
        _,
        _,
        _,
    ] = body.as_slice()
    else {
        return Err(Error::Unsupported("extension generation layout"));
    };
    let [BinNode::Atom(key_version), BinNode::Atom(key)] = locator.as_slice() else {
        return Err(Error::Unsupported("extension generation locator"));
    };
    if version.as_int() != Some(2) || key_version.as_int() != Some(1) {
        return Err(Error::Unsupported("extension generation version"));
    }
    let AtomValue::Narrow(raw) = key.value() else {
        return Err(malformed());
    };
    let hash: [u8; 20] = raw.as_slice().try_into().map_err(|_| malformed())?;
    Ok((hash != [0; 20]).then_some(hash))
}
