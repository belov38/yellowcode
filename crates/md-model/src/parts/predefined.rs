use crate::MdObject;
use crate::model::{MdRef, TypeDescription};
use brace::{Guid, Node};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PredefinedCode {
    Str(String),
    Number(String),
}

impl Default for PredefinedCode {
    fn default() -> Self {
        PredefinedCode::Str(String::new())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountType {
    Active,
    Passive,
    ActivePassive,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AccountData {
    pub account_type: AccountType,
    pub off_balance: bool,
    pub order: String,
    pub flags: Vec<(MdRef, bool)>,
    pub ext_dimension_types: Option<Vec<ExtDimensionType>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExtDimensionType {
    pub kind: MdRef,
    pub turnover: bool,
    pub flags: Vec<(MdRef, bool)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CalcTypeData {
    pub action_period_is_base: bool,
    pub base: Vec<MdRef>,
    pub leading: Vec<MdRef>,
    pub displaced: Vec<MdRef>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PredefinedItem {
    pub id: Guid,
    pub name: String,
    pub code: PredefinedCode,
    pub description: String,
    pub is_folder: bool,
    pub type_desc: Option<TypeDescription>,
    pub account: Option<AccountData>,
    pub calc: Option<CalcTypeData>,
    pub children: Vec<PredefinedItem>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Predefined {
    pub items: Vec<PredefinedItem>,
    #[doc(hidden)]
    pub source: Option<Node>,
}

impl Predefined {
    pub fn from_items(items: Vec<PredefinedItem>) -> Predefined {
        Predefined {
            items,
            source: None,
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &PredefinedItem> {
        let mut stack: Vec<&PredefinedItem> = self.items.iter().rev().collect();
        std::iter::from_fn(move || {
            let it = stack.pop()?;
            stack.extend(it.children.iter().rev());
            Some(it)
        })
    }

    pub fn find(&self, name: &str) -> Option<&PredefinedItem> {
        self.iter().find(|i| i.name == name)
    }
}

impl MdObject {
    pub fn predefined(&self) -> Option<&Predefined> {
        self.parts.predefined.as_ref()
    }
}
