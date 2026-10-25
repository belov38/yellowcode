use super::managed::{RowVisitError, visit_fields};
use ibschema::storage::{StorageMap, TableId, TableInfo};
use memory_budget::{
    BudgetError, BudgetedVec, Category, Charged, LimitReason, MemoryBudget, MemoryLimit,
    RequestMemory,
};

#[derive(Debug)]
pub enum TableReadError<E> {
    Memory(BudgetError),
    Storage(onecd::Error),
    NoTable,
    MissingColumn,
    ColumnType,
    Row(RowVisitError<E>),
}
impl<E> From<BudgetError> for TableReadError<E> {
    fn from(error: BudgetError) -> Self {
        Self::Memory(error)
    }
}
fn storage<E>(error: onecd::Error) -> TableReadError<E> {
    match error {
        onecd::Error::Memory(error) => TableReadError::Memory(error),
        error => TableReadError::Storage(error),
    }
}
pub struct ManagedTable<'a> {
    physical: onecd::TableRead<'a>,
    map: &'a StorageMap,
    info: &'a TableInfo,
    positions: Charged<Vec<usize>>,
    context: &'a RequestMemory,
}
impl<'a> ManagedTable<'a> {
    pub fn num_slots(&self) -> u32 {
        self.physical.num_slots()
    }
    pub fn new(
        snapshot: &'a onecd::ReadTxn,
        map: &'a StorageMap,
        table: TableId,
        context: &'a RequestMemory,
    ) -> Result<Self, TableReadError<std::convert::Infallible>> {
        if snapshot
            .memory_budget()
            .is_none_or(|budget| !budget.same_instance(&context.budget))
        {
            return Err(BudgetError::Limit(MemoryLimit {
                category: Category::Execution,
                requested: 0,
                reason: LimitReason::InvalidSize,
            })
            .into());
        }
        let info = map.try_table(table).ok_or(TableReadError::NoTable)?;
        let physical = snapshot
            .find_table(&info.phys)
            .map_err(storage)?
            .ok_or(TableReadError::NoTable)?;
        let mut positions = BudgetedVec::try_with_capacity(
            &context.budget,
            Category::Execution,
            info.columns.len(),
        )?;
        for column in &info.columns {
            let (position, description) = physical
                .descr()
                .field(&column.name)
                .ok_or(TableReadError::MissingColumn)?;
            if crate::colval::col_type_of(description) != Some(column.ty) {
                return Err(TableReadError::ColumnType);
            }
            positions.try_push(position)?;
        }
        Ok(Self {
            physical,
            map,
            info,
            positions: positions.into_charged_vec(),
            context,
        })
    }
    pub fn visit<E>(
        &self,
        row: onecd::RowId,
        visitor: &mut impl FnMut(usize, ibschema::field_view::FieldView<'_>) -> Result<(), E>,
    ) -> Result<bool, TableReadError<E>> {
        self.map_with_budget(
            row,
            &self.context.budget,
            self.context.row_limit,
            |_| Ok(()),
            &mut |_, index, field, _| visitor(index, field),
        )
        .map(|value| value.is_some())
    }
    pub fn visit_scoped<E>(
        &self,
        row: onecd::RowId,
        visitor: &mut impl FnMut(
            usize,
            ibschema::field_view::FieldView<'_>,
            &MemoryBudget,
        ) -> Result<(), E>,
    ) -> Result<bool, TableReadError<E>> {
        self.map_scoped(row, |_| Ok(()), &mut |_, index, field, budget| {
            visitor(index, field, budget)
        })
        .map(|value| value.is_some())
    }
    pub fn map_scoped<T, E>(
        &self,
        row: onecd::RowId,
        initialize: impl FnOnce(&MemoryBudget) -> Result<T, E>,
        visitor: &mut impl FnMut(
            &mut T,
            usize,
            ibschema::field_view::FieldView<'_>,
            &MemoryBudget,
        ) -> Result<(), E>,
    ) -> Result<Option<T>, TableReadError<E>> {
        let scope = self
            .context
            .budget
            .try_scope(Category::Row, self.context.row_limit)?;
        self.map_with_budget(row, &scope, u64::MAX, initialize, visitor)
    }
    fn map_with_budget<T, E>(
        &self,
        row: onecd::RowId,
        budget: &MemoryBudget,
        source_limit: u64,
        initialize: impl FnOnce(&MemoryBudget) -> Result<T, E>,
        visitor: &mut impl FnMut(
            &mut T,
            usize,
            ibschema::field_view::FieldView<'_>,
            &MemoryBudget,
        ) -> Result<(), E>,
    ) -> Result<Option<T>, TableReadError<E>> {
        let Some(raw) = self
            .physical
            .get_managed(row, budget, Category::Execution, source_limit)
            .map_err(storage)?
        else {
            return Ok(None);
        };
        let mut output = initialize(budget)
            .map_err(|error| TableReadError::Row(RowVisitError::Visitor(error)))?;
        visit_fields(
            self.map,
            self.info,
            &self.positions,
            &raw,
            budget,
            self.context.row_limit,
            &mut |reference, budget, limit| {
                self.physical
                    .blob_bytes_managed(reference, budget, Category::Execution, limit)
            },
            &mut |index, field| visitor(&mut output, index, field, budget),
        )
        .map_err(TableReadError::Row)?;
        Ok(Some(output))
    }
}
