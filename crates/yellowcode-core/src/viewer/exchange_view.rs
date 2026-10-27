use crate::viewer::{
    body::{self, ViewerBody},
    names::ManagedNames,
    object_window::canonical_managed,
    path::{self, ManagedResolveError, ManagedTarget},
    show::ManagedShowError,
};
use mdconf::{
    ManagedEntrySource, MdKind,
    parts::ContentItem,
    startup::{ManagedSelectionError, ManagedViewerOpen},
};
use memory_budget::{BudgetError, BudgetedText, BudgetedVec, Category};
use serde::{Serialize, Serializer, ser::SerializeMap};
pub const MAX_PLANS: usize = 10_000;
struct Plan<'a> {
    path: BudgetedText,
    name: &'a str,
    registered: Option<bool>,
    auto_record: Option<bool>,
}
impl Serialize for Plan<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(4))?;
        map.serialize_entry("path", self.path.as_str())?;
        map.serialize_entry("name", self.name)?;
        map.serialize_entry("registered", &self.registered)?;
        map.serialize_entry("auto_record", &self.auto_record)?;
        map.end()
    }
}
struct View<'a> {
    plans: BudgetedVec<Plan<'a>>,
    truncated: bool,
}
impl Serialize for View<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(2))?;
        map.serialize_entry("plans", self.plans.as_slice())?;
        map.serialize_entry("truncated", &self.truncated)?;
        map.end()
    }
}
fn matches<S: ManagedEntrySource>(
    open: &ManagedViewerOpen<S>,
    names: &ManagedNames<'_>,
    ordinal: usize,
    item: &ContentItem,
) -> std::result::Result<bool, ManagedShowError<S::Error>> {
    let target = open.objects().get(ordinal).ok_or(BudgetError::Overflow)?;
    if let Some(id) = item.object.id {
        return Ok(id == target.object.id);
    }
    match path::resolve_managed(open, names, &item.object.name) {
        Ok(ManagedTarget::Object { ordinal: found, .. }) => Ok(found == ordinal),
        Ok(_) | Err(ManagedResolveError::Path(_)) => Ok(false),
        Err(error) => Err(error.into()),
    }
}
pub fn membership_managed<S: ManagedEntrySource>(
    open: &ManagedViewerOpen<S>,
    names: &ManagedNames<'_>,
    path: &str,
    wire_limit: u64,
) -> std::result::Result<Option<ViewerBody>, ManagedShowError<S::Error>> {
    let target = path::resolve_managed(open, names, path)?;
    let ManagedTarget::Object { ordinal, .. } = target else {
        return Ok(None);
    };
    if open
        .objects()
        .get(ordinal)
        .ok_or(BudgetError::Overflow)?
        .parent
        .is_some()
    {
        return Ok(None);
    }
    let budget = open.context().index().memory_budget();
    let mut view = View {
        plans: BudgetedVec::new(budget, Category::Metadata)?,
        truncated: false,
    };
    for (at, row) in open
        .objects()
        .iter()
        .enumerate()
        .filter(|(_, r)| r.parent.is_none() && r.object.kind == MdKind::ExchangePlan)
    {
        if view.plans.len() == MAX_PLANS {
            view.truncated = true;
            break;
        }
        let selected = open
            .context()
            .read_exchange_content(row.object.id)
            .map_err(ManagedSelectionError::Decode)?
            .ok_or(ManagedSelectionError::MissingValidated)?;
        let content = selected.value.as_deref();
        let mut member = None;
        if let Some(items) = content {
            for item in items {
                if matches(open, names, ordinal, item)? {
                    member = Some(item.auto_record);
                    break;
                }
            }
        }
        let path = canonical_managed(
            open,
            &ManagedTarget::Object {
                ordinal: at,
                object: &row.object,
            },
        )?;
        view.plans.try_push(Plan {
            path,
            name: &row.object.name,
            registered: content.map(|_| member.is_some()),
            auto_record: member,
        })?;
    }
    Ok(Some(body::json(budget, wire_limit, &view)?))
}
