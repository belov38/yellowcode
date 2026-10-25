use ibschema::storage::{StorageMap, TableId, TableInfo};
use infobase::store1cd::managed_table::{ManagedTable, TableReadError};
use memory_budget::{BudgetError, Category, RequestMemory, Reservation};
use query_ir::{
    convert::managed::{FieldConversionError, try_table_field_to_q},
    managed_value::ManagedRow,
};

pub struct ManagedStoredTable<'a> {
    table: ManagedTable<'a>,
    info: &'a TableInfo,
    context: &'a RequestMemory,
    _charge: Reservation,
}
impl<'a> ManagedStoredTable<'a> {
    pub fn new(
        snapshot: &'a onecd::ReadTxn,
        map: &'a StorageMap,
        table: TableId,
        context: &'a RequestMemory,
    ) -> Result<Self, TableReadError<std::convert::Infallible>> {
        let info = map.try_table(table).ok_or(TableReadError::NoTable)?;
        let charge = context
            .budget
            .try_reserve(Category::Execution, std::mem::size_of::<Self>() as u64)
            .map_err(BudgetError::from)?;
        Ok(Self {
            table: ManagedTable::new(snapshot, map, table, context)?,
            info,
            context,
            _charge: charge,
        })
    }
    pub fn get(
        &self,
        row: onecd::RowId,
    ) -> Result<Option<ManagedRow>, TableReadError<FieldConversionError>> {
        self.table.map_scoped(
            row,
            |budget| {
                ManagedRow::try_with_capacity(budget, Category::Execution, self.info.fields.len())
                    .map_err(FieldConversionError::Memory)
            },
            &mut |output: &mut ManagedRow, index, field, budget| {
                output.try_push(try_table_field_to_q(self.info, index, field, budget)?)?;
                Ok(())
            },
        )
    }
    pub fn scan_after(&self, after: u64) -> Result<ManagedStoredRows<'_, 'a>, BudgetError> {
        let end = self.table.num_slots();
        let next = scan_start(after, end);
        let charge = self.context.budget.try_reserve(
            Category::Execution,
            std::mem::size_of::<ManagedStoredRows<'_, '_>>() as u64,
        )?;
        Ok(ManagedStoredRows {
            table: self,
            next,
            end,
            _charge: charge,
        })
    }
    pub fn into_scoped_scan_after(
        self,
        after: u64,
    ) -> Result<query_exec::managed::source::ScopedRows<ManagedStoredOwnedRows<'a>>, BudgetError>
    {
        let context = self.context;
        query_exec::managed::source::ScopedRows::try_new(
            self.into_scan_after(after)?,
            &context.budget,
        )
    }
    pub fn into_scan_after(self, after: u64) -> Result<ManagedStoredOwnedRows<'a>, BudgetError> {
        let end = self.table.num_slots();
        let next = scan_start(after, end);
        let charge = self.context.budget.try_reserve(
            Category::Execution,
            std::mem::size_of::<ManagedStoredOwnedRows<'_>>() as u64,
        )?;
        Ok(ManagedStoredOwnedRows {
            table: self,
            next,
            end,
            _charge: charge,
        })
    }
}

pub struct ManagedStoredRows<'s, 'a> {
    table: &'s ManagedStoredTable<'a>,
    next: u32,
    end: u32,
    _charge: Reservation,
}
impl Iterator for ManagedStoredRows<'_, '_> {
    type Item = Result<(u64, ManagedRow), TableReadError<FieldConversionError>>;
    fn next(&mut self) -> Option<Self::Item> {
        next_row(self.table, &mut self.next, self.end)
    }
}

pub struct ManagedStoredOwnedRows<'a> {
    table: ManagedStoredTable<'a>,
    next: u32,
    end: u32,
    _charge: Reservation,
}
impl Iterator for ManagedStoredOwnedRows<'_> {
    type Item = Result<(u64, ManagedRow), TableReadError<FieldConversionError>>;
    fn next(&mut self) -> Option<Self::Item> {
        next_row(&self.table, &mut self.next, self.end)
    }
}
impl std::iter::FusedIterator for ManagedStoredRows<'_, '_> {}
impl std::iter::FusedIterator for ManagedStoredOwnedRows<'_> {}

fn scan_start(after: u64, end: u32) -> u32 {
    after
        .checked_add(1)
        .and_then(|next| u32::try_from(next).ok())
        .map_or(end, |next| next.min(end))
}
fn next_row(
    table: &ManagedStoredTable<'_>,
    next: &mut u32,
    end: u32,
) -> Option<Result<(u64, ManagedRow), TableReadError<FieldConversionError>>> {
    while *next < end {
        let row = *next;
        *next = row.checked_add(1).unwrap_or(end);
        match table.get(row) {
            Ok(Some(output)) => return Some(Ok((u64::from(row), output))),
            Ok(None) => {}
            Err(error) => {
                *next = end;
                return Some(Err(error));
            }
        }
    }
    None
}
