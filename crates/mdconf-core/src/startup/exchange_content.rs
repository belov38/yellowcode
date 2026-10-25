use super::{ManagedIndex, ManagedValueContext, index::text, values::append};
use crate::parts::ContentItem;
use brace::{Guid, Node};
use memory_budget::{BudgetError, BudgetedVec, Category, Charged};
#[derive(Debug)]
pub struct ManagedExchangeContent {
    pub items: Option<Charged<Vec<ContentItem>>>,
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
fn at(nodes: &[Node], index: usize) -> R<&Node> {
    nodes.get(index).ok_or(Failure::Bad("index"))
}
fn txt(node: &Node) -> R<&str> {
    node.text().ok_or(Failure::Bad("shape"))
}
fn pair(nodes: &[Node], k: usize) -> R<(&str, &str)> {
    let at_pair = k
        .checked_mul(2)
        .and_then(|at| at.checked_add(2))
        .ok_or(Failure::Bad("number"))?;
    let id = txt(at(nodes, at_pair)?)?;
    let auto = txt(at(
        nodes,
        at_pair.checked_add(1).ok_or(Failure::Bad("number"))?,
    )?)?;
    Ok((id, auto))
}
fn decode(index: &ManagedIndex, raw: &Node) -> R<Charged<Vec<ContentItem>>> {
    let budget = index.memory_budget();
    let nodes = raw.as_list().ok_or(Failure::Bad("shape"))?;
    if txt(at(nodes, 0)?)? != "2" {
        return Err(Failure::Bad("version"));
    }
    let n = txt(at(nodes, 1)?)?
        .trim()
        .parse::<i64>()
        .ok()
        .and_then(|n| usize::try_from(n).ok())
        .ok_or(Failure::Bad("number"))?;
    for k in 0..n {
        pair(nodes, k)?;
    }
    let mut sorted = BudgetedVec::try_with_capacity(budget, Category::Metadata, n)?;
    for k in 0..n {
        sorted.try_push(pair(nodes, k)?)?;
    }
    sorted.sort_unstable();
    let mut output =
        BudgetedVec::try_with_capacity(budget, Category::Metadata, n)?.into_charged_vec();
    for (raw_id, raw_auto) in sorted.iter() {
        let id = Guid::parse(raw_id).ok_or(Failure::Bad("object"))?;
        let name = index.names.get(&id).ok_or(Failure::Bad("object"))?;
        let auto_record = match *raw_auto {
            "1" => true,
            "0" => false,
            _ => return Err(Failure::Bad("auto_record")),
        };
        let item = text(budget, &[name.as_str()])?
            .into_string()?
            .map(|name| ContentItem {
                object: crate::MdRef { id: Some(id), name },
                auto_record,
            });
        output = append(output, item)?;
    }
    let end = n
        .checked_mul(2)
        .and_then(|n| n.checked_add(2))
        .ok_or(Failure::Bad("number"))?;
    match nodes.get(end..).unwrap_or_default() {
        [] => {}
        [node] if node.text() == Some("0") => {}
        _ => return Err(Failure::Bad("tail")),
    }
    Ok(output)
}
impl ManagedExchangeContent {
    pub(crate) fn decode(index: &ManagedIndex, raw: &Node) -> Result<Self, BudgetError> {
        match decode(index, raw) {
            Ok(items) => Ok(Self {
                items: Some(items),
                miss: None,
            }),
            Err(Failure::Bad(miss)) => Ok(Self {
                items: None,
                miss: Some(miss),
            }),
            Err(Failure::Memory(error)) => Err(error),
        }
    }
}
impl ManagedValueContext<'_> {
    pub fn exchange_content(&self, raw: &Node) -> Result<ManagedExchangeContent, BudgetError> {
        ManagedExchangeContent::decode(self.index, raw)
    }
}
