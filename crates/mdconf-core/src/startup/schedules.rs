use super::{
    ManagedValueContext,
    values::{append, empty},
};
use crate::{
    EntryData,
    parts::{MiscSource, Schedule},
};
use brace::Node;
use memory_budget::{
    BudgetError, BudgetedText, BudgetedVec, Category, Charged, MemoryBudget, Shared,
};
#[derive(Debug)]
pub struct ManagedSchedule {
    pub schedule: Option<Charged<Schedule>>,
    pub miss: Option<&'static str>,
}
enum Failure {
    Memory(BudgetError),
    Bad(&'static str),
}
impl From<BudgetError> for Failure {
    fn from(error: BudgetError) -> Self {
        Self::Memory(error)
    }
}
type R<T> = Result<T, Failure>;
fn at(nodes: &[Node], at: usize) -> R<&Node> {
    nodes.get(at).ok_or(Failure::Bad("index"))
}
fn text(node: &Node) -> R<&str> {
    node.text().ok_or(Failure::Bad("shape"))
}
fn add(a: usize, b: usize) -> R<usize> {
    a.checked_add(b).ok_or(Failure::Bad("number"))
}
fn count(node: &Node) -> R<usize> {
    text(node)?
        .trim()
        .parse::<i64>()
        .ok()
        .and_then(|n| usize::try_from(n).ok())
        .ok_or(Failure::Bad("number"))
}
fn number(value: &str) -> i64 {
    value.trim().parse().unwrap_or(0)
}
struct Fields<'a> {
    date_time: [&'a str; 5],
    numbers: [&'a str; 7],
    week_days: &'a [Node],
    months: &'a [Node],
    detailed: &'a [Node],
    detail_at: usize,
}
fn list(nodes: &[Node], from: usize, n: usize) -> R<&[Node]> {
    let end = add(from, n)?;
    let items = nodes.get(from..end.min(nodes.len())).unwrap_or_default();
    for item in items {
        text(item)?;
    }
    Ok(items)
}
fn fields(nodes: &[Node], in_detail: bool) -> R<Fields<'_>> {
    let date_time = [
        text(at(nodes, 0)?)?,
        text(at(nodes, 1)?)?,
        text(at(nodes, 2)?)?,
        text(at(nodes, 3)?)?,
        text(at(nodes, 4)?)?,
    ];
    let (ci, rp, rs) = (
        text(at(nodes, 5)?)?,
        text(at(nodes, 6)?)?,
        text(at(nodes, 7)?)?,
    );
    let n = count(at(nodes, 8)?)?;
    let week_days = list(nodes, 9, n)?;
    let mut i = add(9, n)?;
    let (wdm, dim) = (text(at(nodes, i)?)?, text(at(nodes, add(i, 1)?)?)?);
    i = add(i, 2)?;
    let m = count(at(nodes, i)?)?;
    let months = list(nodes, add(i, 1)?, m)?;
    i = add(add(i, 1)?, m)?;
    let tail = |i| nodes.get(i).map_or(Ok("0"), text);
    let (wp, drp) = (tail(i)?, tail(add(i, 1)?)?);
    i = add(i, 2)?;
    let detail_at = add(i, 1)?;
    let detailed = if let Some(k) = nodes.get(i) {
        let k = count(k)?;
        let tail = nodes.get(detail_at..).unwrap_or_default();
        let items = tail.get(..k.min(tail.len())).unwrap_or_default();
        if in_detail && !items.is_empty() {
            return Err(Failure::Bad("nested"));
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
        detail_at,
    })
}
fn date_time(
    budget: &MemoryBudget,
    value: &str,
    date: bool,
) -> Result<Charged<String>, BudgetError> {
    let (offset, widths, separator) = if date {
        (0, [4, 2, 2], '-')
    } else {
        (8, [2, 2, 2], ':')
    };
    let mut out = BudgetedText::new(budget, Category::Metadata, u64::MAX)?;
    let mut chars = value.chars().skip(offset);
    for (part, width) in widths.into_iter().enumerate() {
        if part != 0 {
            out.try_push(separator)?;
        }
        for ch in chars.by_ref().take(width) {
            out.try_push(ch)?;
        }
    }
    out.into_string()
}
fn numbers(budget: &MemoryBudget, nodes: &[Node]) -> R<Charged<Vec<i64>>> {
    let mut values = BudgetedVec::try_with_capacity(budget, Category::Metadata, nodes.len())?;
    for node in nodes {
        if let Ok(n) = text(node)?.trim().parse::<i64>() {
            values.try_push(n)?;
        }
    }
    Ok(values.into_charged_vec())
}
fn decode(
    budget: &MemoryBudget,
    data: &Shared<EntryData>,
    child: Option<usize>,
    in_detail: bool,
) -> R<Charged<Schedule>> {
    let source = MiscSource::managed(data.clone(), child).ok_or(Failure::Bad("shape"))?;
    let nodes = source
        .node()
        .and_then(Node::as_list)
        .ok_or(Failure::Bad("shape"))?;
    let f = fields(nodes, in_detail)?;
    let mut detailed =
        BudgetedVec::try_with_capacity(budget, Category::Metadata, f.detailed.len())?
            .into_charged_vec();
    for (i, _) in f.detailed.iter().enumerate() {
        detailed = append(
            detailed,
            decode(budget, data, Some(add(f.detail_at, i)?), true)?,
        )?;
    }
    let [ci, rp, rs, wdm, dim, wp, drp] = f.numbers;
    let mut output = empty(
        budget,
        Schedule {
            begin_date: String::new(),
            end_date: String::new(),
            begin_time: String::new(),
            end_time: String::new(),
            completion_time: String::new(),
            completion_interval: number(ci),
            repeat_period_in_day: number(rp),
            repeat_pause: number(rs),
            week_days: Vec::new(),
            week_day_in_month: number(wdm),
            day_in_month: number(dim),
            months: Vec::new(),
            weeks_period: number(wp),
            days_repeat_period: number(drp),
            detailed: Vec::new(),
            source: source.clone(),
        },
    )?;
    for (i, raw) in f.date_time.into_iter().enumerate() {
        output = output
            .try_zip(date_time(budget, raw, i < 2)?)?
            .map(|(mut output, value)| {
                match i {
                    0 => output.begin_date = value,
                    1 => output.end_date = value,
                    2 => output.begin_time = value,
                    3 => output.end_time = value,
                    _ => output.completion_time = value,
                }
                output
            });
    }
    output = output
        .try_zip(numbers(budget, f.week_days)?)?
        .map(|(mut output, value)| {
            output.week_days = value;
            output
        });
    output = output
        .try_zip(numbers(budget, f.months)?)?
        .map(|(mut output, value)| {
            output.months = value;
            output
        });
    Ok(output.try_zip(detailed)?.map(|(mut output, detailed)| {
        output.detailed = detailed;
        output
    }))
}
impl ManagedSchedule {
    pub(crate) fn decode(
        budget: &MemoryBudget,
        data: &Shared<EntryData>,
    ) -> Result<Self, BudgetError> {
        if !data.reservation().budget().same_scope(budget) {
            return Err(BudgetError::Overflow);
        }
        if !matches!(&**data, EntryData::Brace(_)) {
            return Ok(Self {
                schedule: None,
                miss: Some("not_brace"),
            });
        }
        match decode(budget, data, None, false) {
            Ok(schedule) => Ok(Self {
                schedule: Some(schedule),
                miss: None,
            }),
            Err(Failure::Bad(miss)) => Ok(Self {
                schedule: None,
                miss: Some(miss),
            }),
            Err(Failure::Memory(error)) => Err(error),
        }
    }
}
impl ManagedValueContext<'_> {
    pub fn schedule(&self, data: &Shared<EntryData>) -> Result<ManagedSchedule, BudgetError> {
        ManagedSchedule::decode(self.memory_budget(), data)
    }
}
