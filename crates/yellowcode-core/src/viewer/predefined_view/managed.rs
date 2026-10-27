use super::{KINDS, MAX_DEPTH, MAX_ITEMS, code_text};
use crate::viewer::{
    Lang,
    body::{self, ViewerBody},
    names::ManagedNames,
    path::{self, ManagedTarget},
    show::ManagedShowError,
};
use brace::Guid;
use mdconf::{
    ChildTag, ManagedEntrySource, MdKind, MdRef, TypeDescription,
    parts::{AccountData, AccountType, PredefinedItem},
    startup::{ChildFacts, ManagedSelectionError, ManagedViewerOpen},
};
use memory_budget::{BudgetError, BudgetedText, BudgetedVec, Category, MemoryBudget, TextCount};
use serde::{
    Serialize, Serializer,
    ser::{SerializeMap, SerializeSeq},
};
type Result<T> = std::result::Result<T, BudgetError>;
struct Flags<'a>(&'a [ChildFacts]);
impl Flags<'_> {
    fn iter(&self) -> impl Iterator<Item = &ChildFacts> {
        self.0.iter().filter(|c| c.tag == ChildTag::AccountingFlag)
    }
}
impl Serialize for Flags<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(self.iter().count()))?;
        for flag in self.iter() {
            seq.serialize_element(&flag.name)?;
        }
        seq.end()
    }
}
struct FlagValues<'a> {
    columns: &'a [ChildFacts],
    values: &'a [(MdRef, bool)],
}
impl Serialize for FlagValues<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let flags = Flags(self.columns);
        let mut seq = s.serialize_seq(Some(flags.iter().count()))?;
        for flag in flags.iter() {
            let value = self
                .values
                .iter()
                .find(|(r, _)| match r.id {
                    Some(id) if id != Guid::NIL && flag.id != Guid::NIL => id == flag.id,
                    _ => r
                        .name
                        .strip_suffix(&flag.name)
                        .is_some_and(|prefix| prefix.ends_with(".AccountingFlag.")),
                })
                .map(|(_, on)| *on);
            seq.serialize_element(&value)?;
        }
        seq.end()
    }
}
struct Account<'a> {
    value: &'a AccountData,
    flags: &'a [ChildFacts],
}
impl Serialize for Account<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(4))?;
        map.serialize_entry(
            "type",
            match self.value.account_type {
                AccountType::Active => "Active",
                AccountType::Passive => "Passive",
                AccountType::ActivePassive => "ActivePassive",
            },
        )?;
        map.serialize_entry("off_balance", &self.value.off_balance)?;
        map.serialize_entry("order", &self.value.order)?;
        map.serialize_entry(
            "flags",
            &FlagValues {
                columns: self.flags,
                values: &self.value.flags,
            },
        )?;
        map.end()
    }
}
struct Item<'a> {
    value: &'a PredefinedItem,
    type_text: Option<BudgetedText>,
    flags: &'a [ChildFacts],
    children: BudgetedVec<Item<'a>>,
}
impl Serialize for Item<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let value = self.value;
        let len = 3
            + usize::from(value.is_folder)
            + usize::from(self.type_text.is_some())
            + usize::from(value.account.is_some())
            + usize::from(!self.children.is_empty());
        let mut map = s.serialize_map(Some(len))?;
        map.serialize_entry("name", &value.name)?;
        map.serialize_entry("code", code_text(&value.code))?;
        map.serialize_entry("description", &value.description)?;
        if value.is_folder {
            map.serialize_entry("folder", &true)?;
        }
        if let Some(text) = &self.type_text {
            map.serialize_entry("type", text.as_str())?;
        }
        if let Some(account) = &value.account {
            map.serialize_entry(
                "account",
                &Account {
                    value: account,
                    flags: self.flags,
                },
            )?;
        }
        if !self.children.is_empty() {
            map.serialize_entry("children", self.children.as_slice())?;
        }
        map.end()
    }
}
struct View<'a> {
    kind: MdKind,
    available: bool,
    truncated: bool,
    flags: &'a [ChildFacts],
    items: BudgetedVec<Item<'a>>,
}
impl Serialize for View<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(6))?;
        map.serialize_entry("kind", self.kind.as_str())?;
        map.serialize_entry("tree", &(self.kind != MdKind::ChartOfCalculationTypes))?;
        map.serialize_entry("available", &self.available)?;
        map.serialize_entry("truncated", &self.truncated)?;
        map.serialize_entry("flags", &Flags(self.flags))?;
        map.serialize_entry("items", self.items.as_slice())?;
        map.end()
    }
}
struct Walk<'a, 'b> {
    budget: &'b MemoryBudget,
    names: &'b ManagedNames<'a>,
    lang: Lang,
    flags: &'a [ChildFacts],
    left: usize,
    max_depth: usize,
    truncated: bool,
}
impl<'a> Walk<'a, '_> {
    fn type_text(&self, value: Option<&TypeDescription>) -> Result<Option<BudgetedText>> {
        let Some(value) = value else {
            return Ok(None);
        };
        let mut count = TextCount::new(Category::Metadata, u64::MAX);
        self.names.write_type_text(value, self.lang, &mut count)?;
        let mut out = BudgetedText::try_with_capacity(
            self.budget,
            Category::Metadata,
            count.len(),
            usize::try_from(count.len()).map_err(|_| BudgetError::Overflow)?,
        )?;
        self.names.write_type_text(value, self.lang, &mut out)?;
        Ok(Some(out))
    }
    fn items(
        &mut self,
        items: &'a [PredefinedItem],
        depth: usize,
    ) -> Result<BudgetedVec<Item<'a>>> {
        let mut out = BudgetedVec::new(self.budget, Category::Metadata)?;
        for item in items {
            if self.left == 0 {
                self.truncated = true;
                break;
            }
            self.left -= 1;
            let type_text = self.type_text(item.type_desc.as_ref())?;
            let mut children = BudgetedVec::new(self.budget, Category::Metadata)?;
            if !item.children.is_empty() {
                if depth >= self.max_depth {
                    self.truncated = true;
                } else {
                    children = self.items(&item.children, depth + 1)?;
                }
            }
            out.try_push(Item {
                value: item,
                type_text,
                flags: self.flags,
                children,
            })?;
        }
        Ok(out)
    }
}
pub fn predefined_managed<S: ManagedEntrySource>(
    open: &ManagedViewerOpen<S>,
    names: &ManagedNames<'_>,
    path: &str,
    lang: Lang,
    wire_limit: u64,
) -> std::result::Result<Option<ViewerBody>, ManagedShowError<S::Error>> {
    let target = path::resolve_managed(open, names, path)?;
    let ManagedTarget::Object { object, .. } = &target else {
        return Ok(None);
    };
    if !KINDS.contains(&object.kind) {
        return Ok(None);
    }
    let selected = open
        .predefined()
        .read(object.id)
        .map_err(ManagedSelectionError::Decode)?
        .ok_or(ManagedSelectionError::MissingValidated)?;
    let predefined = selected.predefined.as_deref();
    let budget = open.context().index().memory_budget();
    let flags = if object.kind == MdKind::ChartOfAccounts {
        object.children.as_slice()
    } else {
        &[]
    };
    let mut walk = Walk {
        budget,
        names,
        lang,
        flags,
        left: MAX_ITEMS,
        max_depth: MAX_DEPTH,
        truncated: false,
    };
    let items = walk.items(predefined.map_or(&[], |p| p.items.as_slice()), 1)?;
    let view = View {
        kind: object.kind,
        available: predefined.is_some(),
        truncated: walk.truncated,
        flags,
        items,
    };
    Ok(Some(body::json(budget, wire_limit, &view)?))
}
