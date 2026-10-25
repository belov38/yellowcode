use memory_budget::{BudgetError, Category};
use query_ir::managed_value::{ManagedRow, row::ScopedRow};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectError {
    Memory(BudgetError),
    Column { index: usize, columns: usize },
}
impl From<BudgetError> for ProjectError {
    fn from(error: BudgetError) -> Self {
        Self::Memory(error)
    }
}
impl std::fmt::Display for ProjectError {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Memory(error) => error.fmt(output),
            Self::Column { index, columns } => {
                write!(
                    output,
                    "projection column {index} outside row of {columns} columns"
                )
            }
        }
    }
}
impl std::error::Error for ProjectError {}

pub fn try_project(source: &ScopedRow, columns: &[usize]) -> Result<ScopedRow, ProjectError> {
    for &index in columns {
        if source.values().get(index).is_none() {
            return Err(ProjectError::Column {
                index,
                columns: source.values().len(),
            });
        }
    }
    let mut row =
        ManagedRow::try_with_capacity(source.budget(), Category::Execution, columns.len())?;
    for &index in columns {
        let value = source.values().get(index).ok_or(ProjectError::Column {
            index,
            columns: source.values().len(),
        })?;
        row.try_push(value.clone())?;
    }
    Ok(ScopedRow::try_from_owned(row)?)
}
