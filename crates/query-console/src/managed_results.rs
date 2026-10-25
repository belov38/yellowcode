use crate::managed::{ManagedCell, ManagedColumn, try_columns};
use crate::managed_rows::try_retain_scoped_row;
use memory_budget::{BudgetError, BudgetedVec, Category, LimitReason, MemoryBudget, Reservation};
use query_ir::managed_value::{ManagedNestedResult, row::ScopedRow};
use query_syntax::Lang;
use query_testhost::TestBase;
use serde::ser::{Serialize, SerializeSeq, SerializeStruct, Serializer};

#[derive(Debug)]
pub struct RetainedResult {
    columns: BudgetedVec<ManagedColumn>,
    rows: BudgetedVec<BudgetedVec<ManagedCell>>,
    total: u64,
    ordered: bool,
    _charge: Reservation,
}
impl RetainedResult {
    pub fn columns(&self) -> &[ManagedColumn] {
        self.columns.as_slice()
    }
    pub fn rows(&self) -> &[BudgetedVec<ManagedCell>] {
        self.rows.as_slice()
    }
    pub fn total(&self) -> u64 {
        self.total
    }
    pub fn kept(&self) -> usize {
        self.rows.len()
    }
    pub fn truncated(&self) -> bool {
        self.rows.len() as u64 != self.total
    }
}

pub fn try_result(
    raw: &ManagedNestedResult,
    base: &TestBase,
    lang: Lang,
    budget: &MemoryBudget,
    max_rows: usize,
    canonical_limit: u64,
    ordered: bool,
) -> Result<RetainedResult, BudgetError> {
    let charge = budget.try_reserve(
        Category::Result,
        std::mem::size_of::<RetainedResult>() as u64,
    )?;
    let total = u64::try_from(raw.rows.len()).map_err(|_| BudgetError::Overflow)?;
    let columns = try_columns(&raw.columns, base, lang, budget)?;
    let mut rows = BudgetedVec::new(budget, Category::Result)?;
    for raw_row in raw.rows.iter().take(max_rows) {
        match rows.try_reserve(1) {
            Ok(()) => {}
            Err(BudgetError::Limit(limit))
                if limit.category == Category::Result && limit.reason == LimitReason::Category =>
            {
                break;
            }
            Err(error) => return Err(error),
        }
        let scope = if budget.scope_snapshot().is_some() {
            budget.clone()
        } else {
            budget.try_scope(Category::Row, canonical_limit)?
        };
        let admitted = ScopedRow::try_from_shared(raw_row.clone(), &scope)?;
        let Some(row) =
            try_retain_scoped_row(&admitted, columns.len(), base, lang, canonical_limit)?
        else {
            break;
        };
        rows.try_push(row)?;
    }
    Ok(RetainedResult {
        columns,
        rows,
        total,
        ordered,
        _charge: charge,
    })
}

struct Rows<'a>(&'a [BudgetedVec<ManagedCell>]);
impl Serialize for Rows<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut output = serializer.serialize_seq(Some(self.0.len()))?;
        for row in self.0 {
            output.serialize_element(row.as_slice())?;
        }
        output.end()
    }
}
impl Serialize for RetainedResult {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut output = serializer.serialize_struct("Result", 7)?;
        output.serialize_field("type", "result")?;
        output.serialize_field("columns", self.columns())?;
        output.serialize_field("total", &self.total())?;
        output.serialize_field("kept", &self.kept())?;
        output.serialize_field("truncated", &self.truncated())?;
        output.serialize_field("ordered", &self.ordered)?;
        output.serialize_field("rows", &Rows(self.rows()))?;
        output.end()
    }
}
