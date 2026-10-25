use super::base64::node_bytes;
use super::index::text;
use super::values::append;
use crate::parts::Help;
use brace::Node;
use memory_budget::{BudgetError, BudgetedVec, Category, Charged, MemoryBudget};
const CATEGORY: Category = Category::Metadata;
#[derive(Debug)]
pub struct ManagedHelp {
    pub help: Option<Charged<Help>>,
    pub miss: Option<&'static str>,
}
impl ManagedHelp {
    fn missing(why: &'static str) -> Self {
        Self {
            help: None,
            miss: Some(why),
        }
    }
    pub fn decode(budget: &MemoryBudget, node: &Node) -> Result<Self, BudgetError> {
        let Some(list) = node.as_list() else {
            return Ok(Self::missing("malformed"));
        };
        let number = |node: &Node| node.text()?.trim().parse::<i64>().ok();
        let Some(count) = list
            .get(1)
            .and_then(number)
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n <= list.len())
        else {
            return Ok(Self::missing("malformed"));
        };
        let mut pages = BudgetedVec::try_with_capacity(budget, CATEGORY, count)?.into_charged_vec();
        for page in 0..count {
            let at = page
                .checked_mul(2)
                .and_then(|at| at.checked_add(2))
                .ok_or(BudgetError::Overflow)?;
            let Some(lang) = list.get(at).and_then(Node::text) else {
                return Ok(Self::missing("malformed"));
            };
            let Some(payload) = list.get(at + 1).and_then(|node| node.at(0)) else {
                return Ok(Self::missing("malformed"));
            };
            let Some(data) = node_bytes(budget, payload)? else {
                return Ok(Self::missing("malformed"));
            };
            pages = append(pages, text(budget, &[lang])?.into_string()?.try_zip(data)?)?;
        }
        let mut at = count
            .checked_mul(2)
            .and_then(|at| at.checked_add(2))
            .ok_or(BudgetError::Overflow)?;
        let count = if let Some(value) = list.get(at) {
            let Some(count) = number(value) else {
                return Ok(Self::missing("malformed"));
            };
            let count = usize::try_from(count).unwrap_or(0);
            if count > list.len() {
                return Ok(Self::missing("malformed"));
            }
            count
        } else {
            0
        };
        let mut files = BudgetedVec::try_with_capacity(budget, CATEGORY, count)?.into_charged_vec();
        at = at.checked_add(1).ok_or(BudgetError::Overflow)?;
        for _ in 0..count {
            let Some(name) = list.get(at).and_then(Node::text) else {
                return Ok(Self::missing("malformed"));
            };
            let Some(payload) = list
                .get(at.checked_add(2).ok_or(BudgetError::Overflow)?)
                .and_then(|node| node.at(0))
            else {
                return Ok(Self::missing("malformed"));
            };
            let Some(data) = node_bytes(budget, payload)? else {
                return Ok(Self::missing("malformed"));
            };
            files = append(files, text(budget, &[name])?.into_string()?.try_zip(data)?)?;
            at = at.checked_add(3).ok_or(BudgetError::Overflow)?;
        }
        for (lang, _) in pages.iter() {
            let path = text(budget, &[lang, ".html"])?;
            if !plain_path(path.as_str()) {
                return Ok(Self::missing("path"));
            }
        }
        if files.iter().any(|(name, _)| !plain_path(name)) {
            return Ok(Self::missing("path"));
        }
        Ok(Self {
            help: Some(
                pages
                    .try_zip(files)?
                    .map(|(pages, files)| Help { pages, files }),
            ),
            miss: None,
        })
    }
}
fn plain_path(path: &str) -> bool {
    path.split('/').all(|component| {
        !component.is_empty()
            && component != "."
            && component != ".."
            && !component.contains(['\\', ':', '\0'])
    })
}
