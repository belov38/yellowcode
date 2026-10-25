use super::{ManagedValueContext, values::empty};
use crate::{
    EntryData,
    parts::{MiscSource, Style},
};
use brace::Node;
use memory_budget::{BudgetError, BudgetedVec, Category, Charged, Shared};

#[derive(Debug)]
pub struct ManagedStyle {
    pub style: Option<Charged<Style>>,
    pub miss: Option<&'static str>,
}
enum Failure {
    Memory(BudgetError),
    Bad(&'static str),
}
impl From<BudgetError> for Failure {
    fn from(value: BudgetError) -> Self {
        Self::Memory(value)
    }
}
type R<T> = Result<T, Failure>;
fn at(nodes: &[Node], index: usize) -> R<&Node> {
    nodes.get(index).ok_or(Failure::Bad("index"))
}
fn text(node: &Node) -> R<&str> {
    node.text().ok_or(Failure::Bad("shape"))
}
struct Header<'a> {
    rank: usize,
    original: usize,
    kind: &'a str,
    value: &'a Node,
}
fn validate(context: &ManagedValueContext<'_>, root: &Node) -> R<()> {
    let nodes = root.as_list().ok_or(Failure::Bad("shape"))?;
    let n = text(at(nodes, 1)?)?
        .trim()
        .parse::<i64>()
        .ok()
        .and_then(|n| usize::try_from(n).ok())
        .ok_or(Failure::Bad("number"))?;
    let tables = mdschema::static_schema()
        .table("ui_tables.json")
        .ok_or(Failure::Bad("tables"))?;
    let order = tables
        .get("style_order")
        .and_then(|v| v.as_array())
        .ok_or(Failure::Bad("tables"))?;
    let mut headers = BudgetedVec::try_with_capacity(
        context.memory_budget(),
        Category::Metadata,
        n.min(nodes.len().saturating_sub(2)),
    )?;
    for (original, item) in nodes.iter().skip(2).take(n).enumerate() {
        let item = item.as_list().ok_or(Failure::Bad("shape"))?;
        let id = text(at(at(item, 0)?.as_list().ok_or(Failure::Bad("shape"))?, 0)?)?;
        let kind = text(at(item, 1)?)?;
        let value = at(item, 2)?;
        let (table, group) = match kind {
            "0" => ("style_colors", 0),
            "1" => ("style_fonts", 2),
            "2" => ("style_borders", 1),
            _ => return Err(Failure::Bad("item")),
        };
        let name = tables
            .get(table)
            .and_then(|v| v.get(id))
            .and_then(|v| v.as_str())
            .ok_or(Failure::Bad("item"))?;
        let rank = order
            .iter()
            .position(|v| v.as_str() == Some(name))
            .unwrap_or(1000 + group * 100);
        headers.try_push(Header {
            rank,
            original,
            kind,
            value,
        })?;
    }
    headers
        .as_mut_slice()
        .sort_unstable_by_key(|h| (h.rank, h.original));
    for header in headers.as_slice() {
        if !context.validate_style_item(header.kind, header.value)? {
            return Err(Failure::Bad(match header.kind {
                "0" => "color",
                "1" => "font",
                _ => "border",
            }));
        }
    }
    Ok(())
}
impl ManagedValueContext<'_> {
    pub fn style(&self, data: &Shared<EntryData>) -> Result<ManagedStyle, BudgetError> {
        if !data.reservation().budget().same_scope(self.memory_budget()) {
            return Err(BudgetError::Overflow);
        }
        let EntryData::Brace(doc) = &**data else {
            return Ok(ManagedStyle {
                style: None,
                miss: Some("not_brace"),
            });
        };
        match validate(self, &doc.root) {
            Ok(()) => {
                let source =
                    MiscSource::managed(data.clone(), None).ok_or(BudgetError::Overflow)?;
                Ok(ManagedStyle {
                    style: Some(empty(self.memory_budget(), Style { source })?),
                    miss: None,
                })
            }
            Err(Failure::Bad(miss)) => Ok(ManagedStyle {
                style: None,
                miss: Some(miss),
            }),
            Err(Failure::Memory(error)) => Err(error),
        }
    }
}
