use crate::{
    allocation::{Allocation, Owned},
    codec_base64 as base64,
};
use brace::Node;
use memory_budget::{BudgetError, Charged, MemoryBudget};
pub(super) fn picture_bytes(
    budget: &MemoryBudget,
    source: &str,
) -> Result<Option<Charged<Vec<u8>>>, BudgetError> {
    base64::picture_bytes(Allocation::Managed(budget), source)?
        .map(Owned::managed)
        .transpose()
}
pub(super) fn node_bytes(
    budget: &MemoryBudget,
    source: &Node,
) -> Result<Option<Charged<Vec<u8>>>, BudgetError> {
    base64::node_bytes(Allocation::Managed(budget), source)?
        .map(Owned::managed)
        .transpose()
}
