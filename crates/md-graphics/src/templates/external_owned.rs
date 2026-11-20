use super::{Template, TemplateType, allocation::Allocation, context::Lookup};
use crate::graph::read::ReadError;
use crate::{Error, Result};
use memory_budget::{BudgetError, Charged, MemoryBudget};

fn memory(error: BudgetError) -> Error {
    match error {
        BudgetError::Limit(limit) => Error::Memory(limit),
        other => Error::Io(std::io::Error::new(std::io::ErrorKind::OutOfMemory, other)),
    }
}

#[doc(hidden)]
pub fn read_external_owned(
    lookup: &dyn Lookup,
    data: &crate::EntryData,
    raw: &[u8],
    kind: TemplateType,
    budget: &MemoryBudget,
) -> Result<Charged<Template>> {
    if matches!(
        kind,
        TemplateType::ActiveDocument | TemplateType::GeographicalSchema
    ) {
        return Err(Error::Format {
            entry: "external template".into(),
            what: "unsupported template kind".into(),
        });
    }
    let files = super::convert_with(
        lookup,
        data,
        raw,
        kind,
        Allocation::Managed(budget),
        super::mxlxml::RowCoverage::FullHeight,
    )
    .map_err(|error| match error {
        ReadError::Memory(error) => memory(error),
        semantic => Error::Format {
            entry: "external template".into(),
            what: semantic.legacy().0,
        },
    })?
    .managed()
    .map_err(memory)?;
    Ok(files.map(|files| Template {
        kind: Some(kind),
        files,
    }))
}
