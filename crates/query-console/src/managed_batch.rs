use crate::managed_results::{RetainedResult, try_result};
use memory_budget::{
    BudgetError, BudgetedText, BudgetedVec, Category, Charged, MemoryBudget, Reservation,
};
use query_ir::managed_value::ManagedNestedResult;
use query_syntax::Lang;
use query_testhost::TestBase;
use serde::ser::{Serialize, SerializeStruct, Serializer};

#[derive(Clone, Copy)]
pub enum RawStatement<'a> {
    Result {
        raw: &'a ManagedNestedResult,
        ordered: bool,
    },
    Count {
        name: &'a str,
        n: u64,
    },
    Dropped,
}
#[derive(Debug)]
enum Statement {
    Result(RetainedResult),
    Count { name: Charged<String>, n: u64 },
    Dropped,
}
#[derive(Debug)]
pub struct RetainedReport {
    statements: BudgetedVec<Statement>,
    elapsed_ms: u64,
    _charge: Reservation,
}

pub fn try_report(
    outcomes: &[RawStatement<'_>],
    base: &TestBase,
    lang: Lang,
    budget: &MemoryBudget,
    max_rows: usize,
    canonical_limit: u64,
    elapsed_ms: u64,
) -> Result<RetainedReport, BudgetError> {
    let charge = budget.try_reserve(
        Category::Result,
        std::mem::size_of::<RetainedReport>() as u64,
    )?;
    let mut statements = BudgetedVec::try_with_capacity(budget, Category::Result, outcomes.len())?;
    for outcome in outcomes {
        let statement = match *outcome {
            RawStatement::Result { raw, ordered } => Statement::Result(try_result(
                raw,
                base,
                lang,
                budget,
                max_rows,
                canonical_limit,
                ordered,
            )?),
            RawStatement::Count { name, n } => {
                let len = u64::try_from(name.len()).map_err(|_| BudgetError::Overflow)?;
                let mut text =
                    BudgetedText::try_with_capacity(budget, Category::Result, len, name.len())?;
                text.try_push_str(name)?;
                Statement::Count {
                    name: text.into_string()?,
                    n,
                }
            }
            RawStatement::Dropped => Statement::Dropped,
        };
        statements.try_push(statement)?;
    }
    Ok(RetainedReport {
        statements,
        elapsed_ms,
        _charge: charge,
    })
}

impl Serialize for Statement {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Result(result) => result.serialize(serializer),
            Self::Count { name, n } => {
                let mut output = serializer.serialize_struct("Count", 3)?;
                output.serialize_field("type", "count")?;
                output.serialize_field("name", name.as_ref())?;
                output.serialize_field("n", n)?;
                output.end()
            }
            Self::Dropped => {
                let mut output = serializer.serialize_struct("Dropped", 1)?;
                output.serialize_field("type", "dropped")?;
                output.end()
            }
        }
    }
}
impl Serialize for RetainedReport {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut output = serializer.serialize_struct("Output", 6)?;
        output.serialize_field("schemaVersion", &1u32)?;
        output.serialize_field("statements", self.statements.as_slice())?;
        output.serialize_field("elapsedMs", &self.elapsed_ms)?;
        output.serialize_field("error", &Option::<()>::None)?;
        output.serialize_field("cancelled", &false)?;
        output.serialize_field("timedOut", &false)?;
        output.end()
    }
}
