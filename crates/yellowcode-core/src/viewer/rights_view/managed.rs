use super::{MAX_CONDITION, MAX_LIMIT, RightsQuery};
use crate::viewer::{
    Lang,
    body::{self, ViewerBody},
    configurator::borrowed::{right_hint, right_title},
    names::ManagedNames,
    object_window::canonical_managed,
    path::{self, ManagedTarget},
    show::ManagedShowError,
};
use mdconf::{
    ManagedEntrySource, MdKind,
    parts::{Right, RightTarget, Rights},
    startup::{ManagedSelectionError, ManagedViewerOpen},
};
use memory_budget::{BudgetError, BudgetedText, BudgetedVec, Category, MemoryBudget};
use serde::{Serialize, Serializer};
use std::fmt;
type Result<T, E> = std::result::Result<T, ManagedShowError<E>>;
fn slice<T: Serialize, S: Serializer>(
    value: &BudgetedVec<T>,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    value.as_slice().serialize(serializer)
}
#[derive(Serialize)]
struct Flags {
    set_for_new_objects: bool,
    set_for_attributes_by_default: bool,
    independent_rights_of_child_objects: bool,
}
#[derive(Serialize)]
struct Table<'a> {
    defined: bool,
    flags: Flags,
    #[serde(serialize_with = "slice")]
    columns: BudgetedVec<&'static str>,
    #[serde(serialize_with = "slice")]
    titles: BudgetedVec<&'static str>,
    #[serde(serialize_with = "slice")]
    hints: BudgetedVec<&'static str>,
    #[serde(serialize_with = "slice")]
    rows: BudgetedVec<Row<'a>>,
    total: usize,
    from: usize,
    templates: &'a [(String, String)],
}
#[derive(Serialize)]
struct Row<'a> {
    #[serde(serialize_with = "text")]
    object: BudgetedText,
    kind: &'a str,
    #[serde(serialize_with = "slice")]
    values: BudgetedVec<Option<bool>>,
    #[serde(serialize_with = "slice")]
    restricted: BudgetedVec<Restriction<'a>>,
}
#[derive(Serialize)]
struct Restriction<'a> {
    right: &'static str,
    #[serde(serialize_with = "slice")]
    conditions: BudgetedVec<Condition<'a>>,
}
struct Condition<'a>(&'a str);
impl fmt::Display for Condition<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some((end, _)) = self.0.char_indices().nth(MAX_CONDITION) {
            f.write_str(self.0.get(..end).ok_or(fmt::Error)?)?;
            f.write_str("…")
        } else {
            f.write_str(self.0)
        }
    }
}
impl Serialize for Condition<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}
fn text<S: Serializer>(
    value: &BudgetedText,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    value.as_str().serialize(serializer)
}
fn vec<T>(budget: &MemoryBudget) -> std::result::Result<BudgetedVec<T>, BudgetError> {
    BudgetedVec::new(budget, Category::Metadata)
}
include!(concat!(env!("OUT_DIR"), "/lowercase_context.rs"));
fn context_property(rows: &[(u32, u32)], c: char) -> bool {
    rows.binary_search_by(|&(start, end)| {
        if end < u32::from(c) {
            std::cmp::Ordering::Less
        } else if start > u32::from(c) {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Equal
        }
    })
    .is_ok()
}
fn lowercase(budget: &MemoryBudget, text: &str) -> std::result::Result<BudgetedText, BudgetError> {
    let mut out = BudgetedText::new(budget, Category::Metadata, u64::MAX)?;
    for (at, c) in text.char_indices() {
        if c == 'Σ' {
            let before = text.get(..at).ok_or(BudgetError::Overflow)?;
            let after = text.get(at + c.len_utf8()..).ok_or(BudgetError::Overflow)?;
            let prev = before
                .chars()
                .rev()
                .find(|c| !context_property(IGNORABLE, *c));
            let next = after.chars().find(|c| !context_property(IGNORABLE, *c));
            let final_sigma = prev.is_some_and(|c| context_property(CASED, c))
                && !next.is_some_and(|c| context_property(CASED, c));
            out.try_push_str(if final_sigma { "ς" } else { "σ" })?;
        } else {
            for lower in c.to_lowercase() {
                let mut bytes = [0; 4];
                out.try_push_str(lower.encode_utf8(&mut bytes))?;
            }
        }
    }
    Ok(out)
}
fn value(rights: Option<&Rights>, target: &RightTarget, right: Right) -> bool {
    rights.is_some_and(|r| r.value(target, right))
}
struct RowContext<'a, 'b> {
    budget: &'b MemoryBudget,
    names: &'b ManagedNames<'a>,
    rights: Option<&'a Rights>,
    columns: &'b [Right],
    lang: Lang,
}
impl<'a> RowContext<'a, '_> {
    fn row(
        &self,
        path: BudgetedText,
        kind: MdKind,
        target: &RightTarget,
    ) -> std::result::Result<Row<'a>, BudgetError> {
        let Self {
            budget,
            names,
            rights,
            columns,
            lang,
        } = *self;
        let mut values = vec(budget)?;
        let mut restricted = vec(budget)?;
        for right in columns {
            if !right.is_applicable(kind) {
                values.try_push(None)?;
                continue;
            }
            values.try_push(Some(value(rights, target, *right)))?;
            let mut conditions = vec(budget)?;
            if let Some(rights) = rights {
                for restriction in rights.restrictions(target, *right) {
                    if !restriction.condition.is_empty() {
                        conditions.try_push(Condition(&restriction.condition))?;
                    }
                }
            }
            if !conditions.is_empty() {
                restricted.try_push(Restriction {
                    right: right.static_name(),
                    conditions,
                })?;
            }
        }
        Ok(Row {
            object: path,
            kind: names.kind_name(kind, lang),
            values,
            restricted,
        })
    }
}
pub fn rights_table_managed<S: ManagedEntrySource>(
    open: &ManagedViewerOpen<S>,
    names: &ManagedNames<'_>,
    role: &str,
    lang: Lang,
    query: &RightsQuery,
    wire_limit: u64,
) -> Result<Option<ViewerBody>, S::Error> {
    rights_table_borrowed(
        open,
        names,
        role,
        lang,
        &RightsQueryRef::from(query),
        wire_limit,
    )
}
pub struct RightsQueryRef<'a> {
    pub q: &'a str,
    pub only: bool,
    pub from: usize,
    pub limit: usize,
    pub templates: bool,
}
impl<'a> From<&'a RightsQuery> for RightsQueryRef<'a> {
    fn from(query: &'a RightsQuery) -> Self {
        Self {
            q: &query.q,
            only: query.only,
            from: query.from,
            limit: query.limit,
            templates: query.templates,
        }
    }
}
pub fn rights_table_borrowed<S: ManagedEntrySource>(
    open: &ManagedViewerOpen<S>,
    names: &ManagedNames<'_>,
    role: &str,
    lang: Lang,
    query: &RightsQueryRef<'_>,
    wire_limit: u64,
) -> Result<Option<ViewerBody>, S::Error> {
    let target = path::resolve_managed(open, names, role)?;
    let ManagedTarget::Object { object, .. } = &target else {
        return Ok(None);
    };
    if object.kind != MdKind::Role {
        return Ok(None);
    }
    let selected = open
        .context()
        .read_rights(object.id)
        .map_err(ManagedSelectionError::Decode)?
        .ok_or(ManagedSelectionError::MissingValidated)?;
    let rights = selected.rights.as_deref();
    let budget = open.context().index().memory_budget();
    let mut columns = vec(budget)?;
    for right in Right::all_static() {
        if right.is_applicable(MdKind::Configuration)
            || open
                .objects()
                .iter()
                .any(|o| right.is_applicable(o.object.kind))
        {
            columns.try_push(right)?;
        }
    }
    let mut table = Table {
        defined: rights.is_some(),
        flags: Flags {
            set_for_new_objects: rights.is_some_and(|r| r.set_for_new_objects),
            set_for_attributes_by_default: rights.is_some_and(|r| r.set_for_attributes_by_default),
            independent_rights_of_child_objects: rights
                .is_some_and(|r| r.independent_rights_of_child_objects),
        },
        columns: vec(budget)?,
        titles: vec(budget)?,
        hints: vec(budget)?,
        rows: vec(budget)?,
        total: 0,
        from: query.from,
        templates: rights
            .filter(|_| query.templates)
            .map_or(&[], |r| r.templates.as_slice()),
    };
    for right in columns.iter() {
        let name = right.static_name();
        table.columns.try_push(name)?;
        table
            .titles
            .try_push(right_title(name, lang).unwrap_or(name))?;
        table
            .hints
            .try_push(right_hint(name, lang).unwrap_or_default())?;
    }
    if rights.is_some() {
        let q = lowercase(budget, query.q)?;
        let limit = query.limit.min(MAX_LIMIT);
        let context = RowContext {
            budget,
            names,
            rights,
            columns: &columns,
            lang,
        };
        for ordinal in 0..=open.objects().len() {
            let (target, kind, key) = if ordinal == 0 {
                (
                    ManagedTarget::Configuration(open.identity()),
                    MdKind::Configuration,
                    RightTarget::Configuration,
                )
            } else {
                let object = &open
                    .objects()
                    .get(ordinal - 1)
                    .ok_or(BudgetError::Overflow)?
                    .object;
                (
                    ManagedTarget::Object {
                        ordinal: ordinal - 1,
                        object,
                    },
                    object.kind,
                    RightTarget::Object(object.id),
                )
            };
            if !columns.iter().any(|r| r.is_applicable(kind)) {
                continue;
            }
            let path = canonical_managed(open, &target)?;
            if !q.as_str().is_empty()
                && !lowercase(budget, path.as_str())?
                    .as_str()
                    .contains(q.as_str())
            {
                continue;
            }
            if query.only
                && !columns
                    .iter()
                    .any(|r| r.is_applicable(kind) && value(rights, &key, *r))
            {
                continue;
            }
            let at = table.total;
            table.total = table.total.checked_add(1).ok_or(BudgetError::Overflow)?;
            if at >= query.from && table.rows.len() < limit {
                table.rows.try_push(context.row(path, kind, &key)?)?;
            }
        }
    }
    Ok(Some(body::json(budget, wire_limit, &table)?))
}
