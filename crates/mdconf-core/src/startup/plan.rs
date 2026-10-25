use super::{observed_order, order_count};
use mdschema::{StaticPropSpec, StaticValue, static_schema};
use memory_budget::{BudgetError, BudgetedVec, Category, MemoryBudget};
const CATEGORY: Category = Category::Metadata;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedHead {
    Name,
    Synonym,
    Comment,
}
#[derive(Debug, Clone, Copy)]
pub enum ManagedPropSource {
    Head(ManagedHead),
    Spec(StaticPropSpec),
    Fallback {
        fd: Option<StaticPropSpec>,
        hp: Option<StaticPropSpec>,
    },
}
#[derive(Debug)]
pub struct ManagedPropertyPlan {
    items: BudgetedVec<(&'static str, ManagedPropSource)>,
}
impl ManagedPropertyPlan {
    pub fn build(
        budget: &MemoryBudget,
        props_type: &str,
        tag: &str,
        context: Option<&str>,
        spec: &'static StaticValue,
    ) -> Result<Self, BudgetError> {
        let rows = static_schema()
            .table("xdto_md.json")
            .and_then(|table| table.get(props_type))
            .and_then(StaticValue::as_array)
            .unwrap_or(&[]);
        let xprops = || {
            rows.iter()
                .filter_map(|row| row.at(0).and_then(StaticValue::as_str))
        };
        let context = context.filter(|c| order_count(c) >= 10).map(observed_order);
        let tag_order = observed_order(tag);
        let mut names = BudgetedVec::try_with_capacity(budget, CATEGORY, rows.len())?;
        if let Some(context) = context {
            if let Some(context) = context {
                for name in context.iter() {
                    if xprops().any(|p| p == name) {
                        names.try_push(name)?;
                    }
                }
            }
        } else if let Some(observed) = tag_order.filter(|o| o.iter().next().is_some()) {
            for name in observed.iter() {
                if xprops().any(|p| p == name) {
                    names.try_push(name)?;
                }
            }
            for (i, name) in xprops().enumerate() {
                if names.contains(&name) {
                    continue;
                }
                let prev = xprops().take(i).filter(|p| names.contains(p)).last();
                let at = prev
                    .and_then(|p| names.iter().position(|n| *n == p))
                    .map_or(0, |i| i + 1);
                names.try_push(name)?;
                names
                    .get_mut(at..)
                    .ok_or(BudgetError::Overflow)?
                    .rotate_right(1);
            }
        } else {
            for name in xprops() {
                names.try_push(name)?;
            }
        }
        let manual = static_schema()
            .table("tag_props.json")
            .and_then(|table| table.get(tag));
        let mut items = BudgetedVec::try_with_capacity(budget, CATEGORY, names.len())?;
        for name in names.iter().copied() {
            let source = match name {
                "Name" => ManagedPropSource::Head(ManagedHead::Name),
                "Synonym" => ManagedPropSource::Head(ManagedHead::Synonym),
                "Comment" => ManagedPropSource::Head(ManagedHead::Comment),
                _ => {
                    let value = manual.and_then(|m| m.get(name)).or_else(|| spec.get(name));
                    if let Some(value) = value.and_then(StaticPropSpec::from_static) {
                        ManagedPropSource::Spec(value)
                    } else {
                        let fd = static_schema()
                            .table("fd_template.json")
                            .and_then(|t| t.get(name))
                            .and_then(StaticPropSpec::from_static);
                        let hp = static_schema()
                            .table("hp_templates.json")
                            .and_then(|t| t.get(tag))
                            .and_then(|t| t.get(name))
                            .and_then(StaticPropSpec::from_static);
                        ManagedPropSource::Fallback { fd, hp }
                    }
                }
            };
            items.try_push((name, source))?;
        }
        Ok(Self { items })
    }
    pub(super) fn explicit(
        budget: &MemoryBudget,
        specs: &'static StaticValue,
    ) -> Result<Self, BudgetError> {
        let specs = specs.as_object().unwrap_or_default();
        let mut items = BudgetedVec::try_with_capacity(
            budget,
            CATEGORY,
            specs.len().checked_add(3).ok_or(BudgetError::Overflow)?,
        )?;
        for (name, head) in [
            ("Name", ManagedHead::Name),
            ("Synonym", ManagedHead::Synonym),
            ("Comment", ManagedHead::Comment),
        ] {
            items.try_push((name, ManagedPropSource::Head(head)))?;
        }
        for (name, value) in specs {
            items.try_push((
                *name,
                ManagedPropSource::Spec(
                    StaticPropSpec::from_static(value).ok_or(BudgetError::Overflow)?,
                ),
            ))?;
        }
        Ok(Self { items })
    }
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, ManagedPropSource)> + '_ {
        self.items.iter().copied()
    }
    pub fn len(&self) -> usize {
        self.items.len()
    }
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}
