use crate::codec::parts::{
    R, count,
    misc::{SCHED_ATTRS, at, atext},
};
use crate::{DUMP_VERSION, ManagedXmlWriter};
use brace::Node;
use md_model::parts::{ContentItem, Schedule};
use memory_budget::{BudgetError, BudgetedText, Category, Charged, MemoryBudget};
type Result<T> = std::result::Result<T, BudgetError>;

pub fn managed_content_xml(
    budget: &MemoryBudget,
    items: &[ContentItem],
    limit: u64,
) -> Result<Charged<String>> {
    let mut writer = ManagedXmlWriter::part_document(
        budget,
        limit,
        "ExchangePlanContent",
        &[
            ("xmlns", "http://v8.1c.ru/8.3/xcf/extrnprops"),
            ("xmlns:xr", "http://v8.1c.ru/8.3/xcf/readable"),
            ("xmlns:xs", "http://www.w3.org/2001/XMLSchema"),
            ("xmlns:xsi", "http://www.w3.org/2001/XMLSchema-instance"),
            ("version", DUMP_VERSION),
        ],
    )?;
    for item in items {
        writer.open("Item", &[])?;
        writer.leaf("Metadata", Some(&item.object.name), &[])?;
        writer.leaf(
            "AutoRecord",
            Some(if item.auto_record { "Allow" } else { "Deny" }),
            &[],
        )?;
        writer.close("Item")?;
    }
    writer.finish()
}
struct Fields<'a> {
    date_time: [&'a str; 5],
    numbers: [&'a str; 7],
    week_days: &'a [Node],
    months: &'a [Node],
    detailed: &'a [Node],
}
fn add(a: usize, b: usize) -> R<usize> {
    a.checked_add(b).ok_or("number")
}
fn list(nodes: &[Node], from: usize, n: usize) -> R<&[Node]> {
    let end = add(from, n)?;
    let values = nodes.get(from..end.min(nodes.len())).unwrap_or_default();
    for value in values {
        value.text().ok_or("shape")?;
    }
    Ok(values)
}
fn fields(nodes: &[Node], in_detail: bool) -> R<Fields<'_>> {
    let date_time = [
        atext(nodes, 0)?,
        atext(nodes, 1)?,
        atext(nodes, 2)?,
        atext(nodes, 3)?,
        atext(nodes, 4)?,
    ];
    let (ci, rp, rs) = (atext(nodes, 5)?, atext(nodes, 6)?, atext(nodes, 7)?);
    let n = count(at(nodes, 8)?)?;
    let week_days = list(nodes, 9, n)?;
    let mut i = add(9, n)?;
    let (wdm, dim) = (atext(nodes, i)?, atext(nodes, add(i, 1)?)?);
    i = add(i, 2)?;
    let m = count(at(nodes, i)?)?;
    let months = list(nodes, add(i, 1)?, m)?;
    i = add(add(i, 1)?, m)?;
    let tail = |i: usize| nodes.get(i).map_or(Ok("0"), |n| n.text().ok_or("shape"));
    let (wp, drp) = (tail(i)?, tail(add(i, 1)?)?);
    i = add(i, 2)?;
    let detailed = if let Some(k) = nodes.get(i) {
        let count = count(k)?;
        let tail = nodes.get(add(i, 1)?..).unwrap_or_default();
        let items = tail.get(..count.min(tail.len())).unwrap_or_default();
        if in_detail && !items.is_empty() {
            return Err("nested");
        }
        items
    } else {
        &[]
    };
    Ok(Fields {
        date_time,
        numbers: [ci, rp, rs, wdm, dim, wp, drp],
        week_days,
        months,
        detailed,
    })
}
fn datetime(budget: &MemoryBudget, value: &str, date: bool) -> Result<BudgetedText> {
    let (offset, widths, separator) = if date {
        (0, [4, 2, 2], '-')
    } else {
        (8, [2, 2, 2], ':')
    };
    let mut chars = value.chars().skip(offset);
    let mut out = BudgetedText::new(budget, Category::Metadata, u64::MAX)?;
    for (at, width) in widths.into_iter().enumerate() {
        if at > 0 {
            out.try_push(separator)?;
        }
        for ch in chars.by_ref().take(width) {
            out.try_push(ch)?;
        }
    }
    Ok(out)
}
fn joined(budget: &MemoryBudget, values: &[Node]) -> Result<BudgetedText> {
    let mut out = BudgetedText::new(budget, Category::Metadata, u64::MAX)?;
    for (at, value) in values.iter().enumerate() {
        if at > 0 {
            out.try_push(' ')?;
        }
        out.try_push_str(value.text().ok_or(BudgetError::Overflow)?)?;
    }
    Ok(out)
}
fn write_schedule(
    writer: &mut ManagedXmlWriter,
    budget: &MemoryBudget,
    tag: &str,
    fields: &Fields<'_>,
) -> Result<()> {
    let [bd, ed, bt, et, ct] = fields.date_time;
    let dates = [
        datetime(budget, bd, true)?,
        datetime(budget, ed, true)?,
        datetime(budget, bt, false)?,
        datetime(budget, et, false)?,
        datetime(budget, ct, false)?,
    ];
    let [bd, ed, bt, et, ct] = &dates;
    let [ci, rp, rs, wdm, dim, wp, drp] = fields.numbers;
    let values = [
        bd.as_str(),
        ed.as_str(),
        bt.as_str(),
        et.as_str(),
        ct.as_str(),
        ci,
        rp,
        rs,
        wdm,
        dim,
        wp,
        drp,
    ];
    let mut attrs = [("", ""); 12];
    for (slot, (tag, value)) in attrs.iter_mut().zip(SCHED_ATTRS.into_iter().zip(values)) {
        *slot = (tag, value);
    }
    writer.open(tag, &attrs)?;
    writer.leaf(
        "ent:WeekDays",
        Some(joined(budget, fields.week_days)?.as_str()),
        &[],
    )?;
    writer.leaf(
        "ent:Months",
        Some(joined(budget, fields.months)?.as_str()),
        &[],
    )
}
pub fn managed_schedule_xml(
    budget: &MemoryBudget,
    schedule: &Schedule,
    limit: u64,
) -> Result<Option<Charged<String>>> {
    let Some(root) = schedule.source.node().and_then(Node::as_list) else {
        return Ok(None);
    };
    let Ok(parsed) = fields(root, false) else {
        return Ok(None);
    };
    for detail in parsed.detailed {
        if detail
            .as_list()
            .is_none_or(|nodes| fields(nodes, true).is_err())
        {
            return Ok(None);
        }
    }
    let mut writer = ManagedXmlWriter::part_document(
        budget,
        limit,
        "JobSchedule",
        &[
            ("xmlns", "http://v8.1c.ru/8.3/xcf/extrnprops"),
            ("xmlns:ent", "http://v8.1c.ru/8.1/data/enterprise"),
            ("xmlns:xs", "http://www.w3.org/2001/XMLSchema"),
            ("xmlns:xsi", "http://www.w3.org/2001/XMLSchema-instance"),
            ("version", DUMP_VERSION),
        ],
    )?;
    write_schedule(&mut writer, budget, "Schedule", &parsed)?;
    for detail in parsed.detailed {
        let detail = fields(detail.as_list().ok_or(BudgetError::Overflow)?, true)
            .map_err(|_| BudgetError::Overflow)?;
        write_schedule(&mut writer, budget, "ent:DetailedDailySchedules", &detail)?;
        writer.close("ent:DetailedDailySchedules")?;
    }
    writer.close("Schedule")?;
    writer.finish().map(Some)
}
