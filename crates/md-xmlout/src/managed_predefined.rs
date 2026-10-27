use crate::codec::{
    parts::predefined::{column_of, columns, ext_state, xsi_type},
    predefined::iter_children,
};
use crate::{DUMP_VERSION, ManagedXmlWriter};
use brace::{Guid, Node};
use md_model::{MdKind, MdRef, parts::*};
use memory_budget::{BudgetError, BudgetedVec, Category, Charged, MemoryBudget};
type State = (Guid, usize, &'static str);
type Result<T> = std::result::Result<T, BudgetError>;
macro_rules! present {
    ($value:expr) => {
        match $value {
            Some(value) => value,
            None => return Ok(None),
        }
    };
}
fn number(node: &Node) -> Option<usize> {
    node.text()?.trim().parse().ok()
}
fn states(source: &Node, budget: &MemoryBudget) -> Result<Option<BudgetedVec<State>>> {
    let mut out = BudgetedVec::new(budget, Category::Metadata)?;
    if source.is_empty_slot() || present!(source.at(0)).text() == Some("9") {
        return Ok(Some(out));
    }
    let kind = present!(
        source
            .at(0)
            .and_then(number)
            .and_then(|n| u32::try_from(n).ok())
    );
    let body = present!(source.at(1));
    let cols = present!(body.at(1).and_then(Node::as_list));
    let n = present!(cols.first().and_then(number));
    for col in present!(cols.get(1..)).iter().take(n) {
        present!(col.at(0).and_then(Node::text));
        present!(col.at(1).and_then(Node::text));
        present!(col.at(2));
    }
    let table = present!(body.at(2).and_then(Node::as_list));
    let n = present!(table.get(1).and_then(number));
    if n > table.len().saturating_sub(2) / 2 {
        return Ok(None);
    }
    let mut positions = BudgetedVec::try_with_capacity(budget, Category::Metadata, n)?;
    positions.try_resize(n, None)?;
    for at in 0..n {
        let index = present!(table.get(2 + 2 * at).and_then(number));
        *present!(positions.get_mut(index)) =
            Some(present!(table.get(3 + 2 * at).and_then(Node::text)));
    }
    if positions.iter().any(Option::is_none) {
        return Ok(None);
    }
    let root = present!(table.get(2 + 2 * n).and_then(|row| row.at(2)));
    let cols = present!(columns(kind));
    let reference = present!(column_of(cols, "Ref"));
    let state = present!(column_of(cols, "ExtState"));
    let mut stack = BudgetedVec::new(budget, Category::Metadata)?;
    stack.try_push(present!(iter_children(root)).iter())?;
    while let Some(top) = stack.last_mut() {
        let Some(row) = top.next() else {
            stack.pop();
            continue;
        };
        let values = present!(row.as_list());
        let count = present!(values.get(2).and_then(number));
        let values = present!(values.get(3..).and_then(|tail| tail.get(..count)));
        if count > positions.len() {
            return Ok(None);
        }
        let cell = |id| {
            values
                .iter()
                .zip(positions.iter())
                .rev()
                .find_map(|(value, key)| (*key == Some(id)).then_some(value))
        };
        if let Some(value) = cell(state)
            .and_then(|v| v.at(1))
            .and_then(Node::text)
            .and_then(ext_state)
        {
            let reference = present!(cell(reference));
            if present!(reference.at(0)).text() != Some("#") {
                return Ok(None);
            }
            let id = present!(
                reference
                    .at(2)
                    .and_then(|v| v.at(1))
                    .and_then(Node::text)
                    .and_then(Guid::parse)
            );
            out.try_push((id, out.len(), value))?;
        }
        stack.try_push(present!(iter_children(row)).iter())?;
    }
    out.sort_unstable_by_key(|(id, ordinal, _)| (*id, *ordinal));
    Ok(Some(out))
}
fn boolean(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}
fn flags(writer: &mut ManagedXmlWriter, values: &[(MdRef, bool)]) -> Result<()> {
    if values.is_empty() {
        return Ok(());
    }
    writer.open("AccountingFlags", &[])?;
    for (reference, value) in values {
        writer.leaf("Flag", Some(boolean(*value)), &[("ref", &reference.name)])?;
    }
    writer.close("AccountingFlags")
}
fn prefix(writer: &mut ManagedXmlWriter, item: &PredefinedItem, states: &[State]) -> Result<bool> {
    let mut id = [0u8; 36];
    let mut text = std::io::Cursor::new(id.as_mut_slice());
    std::io::Write::write_fmt(&mut text, format_args!("{}", item.id))
        .map_err(|_| BudgetError::Overflow)?;
    let id = std::str::from_utf8(&id).map_err(|_| BudgetError::Overflow)?;
    writer.open("Item", &[("id", id)])?;
    writer.leaf("Name", Some(&item.name), &[])?;
    match &item.code {
        PredefinedCode::Str(value) => writer.leaf("Code", Some(value), &[])?,
        PredefinedCode::Number(value) => {
            writer.leaf("Code", Some(value), &[("xsi:type", "xs:decimal")])?
        }
    }
    writer.leaf("Description", Some(&item.description), &[])?;
    if let Some(calc) = &item.calc {
        writer.leaf(
            "ActionPeriodIsBase",
            Some(boolean(calc.action_period_is_base)),
            &[],
        )?;
        for (tag, values) in [
            ("Base", &calc.base),
            ("Leading", &calc.leading),
            ("Displaced", &calc.displaced),
        ] {
            if values.is_empty() {
                continue;
            }
            writer.open(tag, &[])?;
            for value in values {
                writer.leaf("CalculationType", Some(&value.name), &[])?;
            }
            writer.close(tag)?;
        }
        return Ok(false);
    }
    if let Some(value) = &item.type_desc {
        writer.predefined_type(value)?;
    }
    if let Some(account) = &item.account {
        writer.leaf(
            "AccountType",
            Some(match account.account_type {
                AccountType::Active => "Active",
                AccountType::Passive => "Passive",
                AccountType::ActivePassive => "ActivePassive",
            }),
            &[],
        )?;
        writer.leaf("OffBalance", Some(boolean(account.off_balance)), &[])?;
        writer.leaf("Order", Some(&account.order), &[])?;
        flags(writer, &account.flags)?;
        if let Some(values) = &account.ext_dimension_types {
            if values.is_empty() {
                writer.leaf("ExtDimensionTypes", None, &[])?;
            } else {
                writer.open("ExtDimensionTypes", &[])?;
                for value in values {
                    writer.open("ExtDimensionType", &[("name", &value.kind.name)])?;
                    writer.leaf("Turnover", Some(boolean(value.turnover)), &[])?;
                    flags(writer, &value.flags)?;
                    writer.close("ExtDimensionType")?;
                }
                writer.close("ExtDimensionTypes")?;
            }
        }
    } else {
        writer.leaf("IsFolder", Some(boolean(item.is_folder)), &[])?;
    }
    let at = states.partition_point(|(id, _, _)| *id < item.id);
    if let Some((_, _, value)) = states.get(at).filter(|(id, _, _)| *id == item.id) {
        writer.leaf("ExtensionState", Some(value), &[])?;
    }
    Ok(true)
}
pub fn managed_predefined_xml(
    budget: &MemoryBudget,
    kind: MdKind,
    predefined: &Predefined,
    limit: u64,
) -> Result<Option<Charged<String>>> {
    let Some(source) = predefined.source.as_ref() else {
        return Ok(None);
    };
    let Some(kind) = xsi_type(kind) else {
        return Ok(None);
    };
    let Some(states) = states(source, budget)? else {
        return Ok(None);
    };
    let mut writer = ManagedXmlWriter::part_document(
        budget,
        limit,
        "PredefinedData",
        &[
            ("xmlns", "http://v8.1c.ru/8.3/xcf/predef"),
            ("xmlns:v8", "http://v8.1c.ru/8.1/data/core"),
            ("xmlns:xr", "http://v8.1c.ru/8.3/xcf/readable"),
            ("xmlns:xs", "http://www.w3.org/2001/XMLSchema"),
            ("xmlns:xsi", "http://www.w3.org/2001/XMLSchema-instance"),
            ("xsi:type", kind),
            ("version", DUMP_VERSION),
        ],
    )?;
    let mut stack = BudgetedVec::new(budget, Category::Metadata)?;
    stack.try_push(predefined.items.iter())?;
    while let Some(top) = stack.last_mut() {
        let Some(item) = top.next() else {
            stack.pop();
            if !stack.is_empty() {
                writer.close("ChildItems")?;
                writer.close("Item")?;
            }
            continue;
        };
        if prefix(&mut writer, item, &states)? && !item.children.is_empty() {
            writer.open("ChildItems", &[])?;
            stack.try_push(item.children.iter())?;
        } else {
            writer.close("Item")?;
        }
    }
    writer.finish().map(Some)
}
