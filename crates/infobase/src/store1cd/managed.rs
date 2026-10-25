use crate::colval::managed::{ColumnDecodeError, ColumnOwner, decode_column};
use ibschema::{
    columns::Part,
    field_view::{ColumnView, FieldDecodeError, FieldView, decode_field_view},
    fields,
    storage::{FieldId, StorageMap, TableInfo},
    values::RefId,
};
use memory_budget::{
    BudgetError, BudgetedVec, Category, Charged, LimitReason, MemoryBudget, MemoryLimit,
};

#[derive(Debug)]
pub enum RowVisitError<E> {
    Memory(BudgetError),
    Column {
        field: usize,
        error: ColumnDecodeError,
    },
    Logical {
        field: usize,
        error: FieldDecodeError,
    },
    MissingColumn,
    InvalidVersion,
    UnknownReference,
    Visitor(E),
}
impl<E> From<BudgetError> for RowVisitError<E> {
    fn from(error: BudgetError) -> Self {
        Self::Memory(error)
    }
}
fn bound(bytes: u64, limit: u64) -> Result<(), BudgetError> {
    if bytes > limit {
        return Err(BudgetError::Limit(MemoryLimit {
            category: Category::Row,
            requested: bytes,
            reason: LimitReason::Category,
        }));
    }
    Ok(())
}
fn descriptor_bytes<T>(length: usize) -> Result<u64, BudgetError> {
    length
        .checked_mul(std::mem::size_of::<T>())
        .and_then(|bytes| bytes.checked_add(std::mem::size_of::<BudgetedVec<T>>()))
        .and_then(|bytes| u64::try_from(bytes).ok())
        .ok_or(BudgetError::Overflow)
}
#[allow(
    clippy::too_many_arguments,
    reason = "explicit independent source, mapping, memory and callback contracts"
)]
pub fn visit_fields<E>(
    map: &StorageMap,
    table: &TableInfo,
    positions: &[usize],
    raw: &Charged<onecd::Row>,
    budget: &MemoryBudget,
    scratch_limit: u64,
    reader: &mut impl FnMut(onecd::BlobRef, &MemoryBudget, u64) -> onecd::Result<Charged<Vec<u8>>>,
    visitor: &mut impl FnMut(usize, FieldView<'_>) -> Result<(), E>,
) -> Result<(), RowVisitError<E>> {
    let charge = raw.reservation();
    if !budget.same_instance(charge.budget()) || charge.category() != Category::Execution {
        return Err(BudgetError::Limit(MemoryLimit {
            category: Category::Execution,
            requested: 0,
            reason: LimitReason::InvalidSize,
        })
        .into());
    }
    let raw_bytes = charge.bytes();
    bound(raw_bytes, scratch_limit)?;
    for (index, field) in table.fields.iter().enumerate() {
        let cell = |column: usize| {
            positions
                .get(column)
                .and_then(|position| raw.get(*position))
        };
        if fields::is_version(field) {
            let Some(onecd::Value::RowVersion(words)) = cell(field.columns.start) else {
                return Err(RowVisitError::InvalidVersion);
            };
            visitor(index, FieldView::RowVersion(*words)).map_err(RowVisitError::Visitor)?;
            continue;
        }
        let metadata = table
            .columns
            .get(field.columns.clone())
            .ok_or(RowVisitError::MissingColumn)?;
        let descriptors = descriptor_bytes::<ColumnOwner<'_>>(metadata.len())?
            .checked_add(descriptor_bytes::<ColumnView<'_>>(metadata.len())?)
            .ok_or(BudgetError::Overflow)?;
        let mut live = raw_bytes
            .checked_add(descriptors)
            .ok_or(BudgetError::Overflow)?;
        bound(live, scratch_limit)?;
        let mut owners =
            BudgetedVec::try_with_capacity(budget, Category::Execution, metadata.len())?;
        let mut views =
            BudgetedVec::try_with_capacity(budget, Category::Execution, metadata.len())?;
        for (column, description) in field.columns.clone().zip(metadata) {
            let value = cell(column).ok_or(RowVisitError::MissingColumn)?;
            let remaining = scratch_limit
                .checked_sub(live)
                .ok_or(BudgetError::Overflow)?;
            let owner = decode_column(value, description.ty, budget, remaining, reader).map_err(
                |error| match error {
                    ColumnDecodeError::Memory(error) => RowVisitError::Memory(error),
                    error => RowVisitError::Column {
                        field: index,
                        error,
                    },
                },
            )?;
            let extra = match &owner {
                ColumnOwner::Borrowed(_) => 0,
                ColumnOwner::Text(text) => text.reservation().bytes(),
                ColumnOwner::Blob(bytes) => bytes.reservation().bytes(),
            };
            live = live.checked_add(extra).ok_or(BudgetError::Overflow)?;
            bound(live, scratch_limit)?;
            owners.try_push(owner)?;
        }
        for owner in owners.iter() {
            views.try_push(owner.view())?;
        }
        let id = FieldId(u16::try_from(index).map_err(|_| BudgetError::Overflow)?);
        let value =
            decode_field_view(table, id, &views).map_err(|error| RowVisitError::Logical {
                field: index,
                error,
            })?;
        if let FieldView::Ref {
            table: reference,
            id,
        } = value
            && id != RefId::NIL
            && map.ref_table(reference).is_none()
            && metadata
                .iter()
                .any(|column| matches!(column.part, Part::RTRef | Part::TRef))
        {
            return Err(RowVisitError::UnknownReference);
        }
        visitor(index, value).map_err(RowVisitError::Visitor)?;
    }
    Ok(())
}
