use super::*;
use crate::collections::columns::SerialColumn;
use bsl_vm::Exception;

pub(crate) struct SerialRow {
    pub(crate) id: u64,
    pub(crate) cells: Vec<Value>,
}
pub(crate) struct SerialSnapshot {
    pub(crate) columns: Vec<SerialColumn>,
    pub(crate) rows: Vec<SerialRow>,
    pub(crate) indexes: Vec<Vec<u32>>,
    pub(crate) next_column: u32,
    pub(crate) next_row: u64,
    pub(crate) revision: [u64; 2],
}
impl ValueTable {
    pub(crate) fn serial_snapshot_bytes(&self, ex: &mut Execution) -> Result<u64, Exception> {
        self.serial_snapshot_bytes_mode(ex, false)
    }
    pub(crate) fn serial_xml_snapshot_bytes(&self, ex: &mut Execution) -> Result<u64, Exception> {
        self.serial_snapshot_bytes_mode(ex, true)
    }
    fn serial_snapshot_bytes_mode(
        &self,
        ex: &mut Execution,
        all_rows: bool,
    ) -> Result<u64, Exception> {
        let state = &self.state;
        ex.charge(1)?;
        let mut bytes = state
            .live(ex)
            .map_err(|e| match e {
                bsl_vm::CallError::Host(e) => ex.error(e),
                bsl_vm::CallError::Exc(e) => e,
            })?
            .saturating_add(state.columns.serial_bytes(ex)?)
            .saturating_add(state.indexes.serial_bytes(ex)?);
        let rows = state.rows.borrow();
        ex.charge(rows.len() as u64)?;
        bytes = bytes.saturating_add(
            (rows.len() as u64).saturating_mul(std::mem::size_of::<SerialRow>() as u64),
        );
        for row in rows.iter().filter(|row| all_rows || row.materialized.get()) {
            let cells = row.cells.borrow();
            ex.charge(cells.0.len() as u64)?;
            bytes = bytes.saturating_add(
                (state.columns.len() as u64).saturating_mul(std::mem::size_of::<Value>() as u64),
            );
            for (_, value) in &cells.0 {
                bytes = bytes.saturating_add(match value {
                    Value::Number(n) => n
                        .size_hint_limbs()
                        .saturating_mul(64)
                        .saturating_add(256)
                        .saturating_add(scalar_clone_bytes(value))
                        .saturating_add(bridge::capacity_extra(value)),
                    Value::String(text) => value_bytes(value)
                        .saturating_add(bridge::capacity_extra(value))
                        .saturating_add((text.len() as u64).saturating_mul(12)),
                    _ => value_bytes(value).saturating_add(bridge::capacity_extra(value)),
                });
            }
        }
        Ok(bytes)
    }
    pub(crate) fn serial_snapshot(&self, ex: &mut Execution) -> Result<SerialSnapshot, Exception> {
        self.serial_snapshot_mode(ex, false)
    }
    pub(crate) fn serial_xml_snapshot(
        &self,
        ex: &mut Execution,
    ) -> Result<SerialSnapshot, Exception> {
        self.serial_snapshot_mode(ex, true)
    }
    fn serial_snapshot_mode(
        &self,
        ex: &mut Execution,
        all_rows: bool,
    ) -> Result<SerialSnapshot, Exception> {
        let state = &self.state;
        let revision = [state.revision.get(), state.columns.revision()];
        let columns = state.columns.serial_columns(ex)?;
        let indexes = state.indexes.serial_definitions(ex)?;
        let source = state.rows.borrow();
        ex.charge(source.len() as u64)?;
        let mut rows = Vec::new();
        rows.try_reserve_exact(source.len())
            .map_err(|_| ex.out_of_memory())?;
        for row in source.iter() {
            let mut cells = Vec::new();
            if all_rows || row.materialized.get() {
                let values = row.cells.borrow();
                ex.charge((columns.len() as u64).saturating_mul(values.0.len() as u64 + 1))?;
                cells
                    .try_reserve_exact(columns.len())
                    .map_err(|_| ex.out_of_memory())?;
                for column in &columns {
                    let value = values
                        .0
                        .iter()
                        .find(|(id, _)| *id == column.id)
                        .map(|(_, v)| v)
                        .ok_or_else(|| ex.error(changed()))?;
                    cells.push(match value {
                        Value::Number(n) => {
                            ex.charge(n.size_hint_limbs() / 64 + 1)?;
                            Value::Number(
                                n.try_clone_with(|_| Ok::<_, Exception>(()))
                                    .map_err(|_| ex.out_of_memory())?,
                            )
                        }
                        _ => value.clone(),
                    });
                }
            }
            rows.push(SerialRow { id: row.id, cells });
        }
        Ok(SerialSnapshot {
            columns,
            rows,
            indexes,
            next_column: state.columns.serial_next_id(),
            next_row: state.next_id.get(),
            revision,
        })
    }
    pub(crate) fn serial_check_revision(
        &self,
        ex: &mut Execution,
        revision: [u64; 2],
    ) -> Result<(), Exception> {
        self.state
            .check_revision(revision[0], revision[1])
            .map_err(|e| match e {
                bsl_vm::CallError::Host(e) => ex.error(e),
                bsl_vm::CallError::Exc(e) => e,
            })
    }
}

fn exception(ex: &mut Execution, error: bsl_vm::CallError) -> Exception {
    match error {
        bsl_vm::CallError::Host(error) => ex.error(error),
        bsl_vm::CallError::Exc(error) => error,
    }
}
impl ValueTable {
    pub(crate) fn serial_new(ex: &mut Execution) -> Result<Value, Exception> {
        super::ctor(ex, &mut []).map_err(|e| exception(ex, e))
    }

    pub(crate) fn serial_new_bytes() -> u64 {
        (std::mem::size_of::<TableState>()
            + std::mem::size_of::<ValueTable>()
            + 4 * std::mem::size_of::<usize>()) as u64
            + Columns::allocation_bytes()
            + indexes::Indexes::allocation_bytes()
    }

    pub(crate) fn serial_column_bytes(name_units: usize, title_units: usize) -> Option<u64> {
        let record = std::mem::size_of::<ColumnRecord>() + 2 * std::mem::size_of::<usize>() + 128;
        u64::try_from(name_units)
            .ok()?
            .checked_mul(12)?
            .checked_add(u64::try_from(title_units).ok()?.checked_mul(2)?)?
            .checked_add(record as u64)
    }

    pub(crate) fn serial_row_bytes(columns: usize) -> Option<u64> {
        let record = std::mem::size_of::<RowRecord>() + 2 * std::mem::size_of::<usize>() + 1024;
        let cell = std::mem::size_of::<(u32, Value)>() + std::mem::size_of::<usize>();
        u64::try_from(columns.checked_mul(cell)?.checked_add(record)?).ok()
    }

    pub(crate) fn serial_names_valid(
        ex: &mut Execution,
        names: &[bsl_prim::Str],
    ) -> Result<bool, Exception> {
        Columns::serial_name_keys(ex, names).map_err(|e| exception(ex, e))
    }

    pub(crate) fn serial_restore_column(
        &self,
        ex: &mut Execution,
        id: u32,
        name: bsl_prim::Str,
        value_type: Value,
        title: bsl_prim::Str,
        width: i64,
    ) -> Result<(), Exception> {
        self.state
            .columns
            .serial_restore_column(
                ex,
                SerialColumn {
                    id,
                    name,
                    value_type,
                    title,
                    width,
                },
            )
            .map_err(|e| exception(ex, e))
    }

    pub(crate) fn serial_reserve_entries(
        &self,
        ex: &mut Execution,
        columns: usize,
        rows: usize,
        indexes: usize,
        root: &Value,
        scratch: u64,
    ) -> Result<(), Exception> {
        self.state
            .columns
            .serial_reserve_entries(ex, columns)
            .map_err(|e| exception(ex, e))?;
        let remaining = (rows as u64)
            .saturating_add(indexes as u64)
            .saturating_mul(std::mem::size_of::<usize>() as u64);
        Self::serial_admit(ex, root, None, scratch.saturating_add(remaining))?;
        self.state
            .rows
            .borrow_mut()
            .try_reserve_exact(rows)
            .map_err(|_| ex.out_of_memory())?;
        let remaining = (indexes as u64).saturating_mul(std::mem::size_of::<usize>() as u64);
        Self::serial_admit(ex, root, None, scratch.saturating_add(remaining))?;
        self.state
            .indexes
            .serial_reserve_entries(ex, indexes)
            .map_err(|e| exception(ex, e))?;
        Self::serial_admit(ex, root, None, scratch)
    }

    pub(crate) fn serial_restore_counters(&self, column: u32, row: u64) {
        self.state.columns.serial_set_next(column);
        self.state.next_id.set(row);
    }

    pub(crate) fn serial_restore_index(
        &self,
        ex: &mut Execution,
        ids: &[u32],
    ) -> Result<(), Exception> {
        self.state
            .indexes
            .serial_restore(ex, ids)
            .map_err(|e| exception(ex, e))
    }

    pub(crate) fn serial_restore_row(
        &self,
        ex: &mut Execution,
        id: u64,
        materialized: bool,
    ) -> Result<(), Exception> {
        let state = &self.state;
        let mut live = state
            .live(ex)
            .map_err(|e| exception(ex, e))?
            .saturating_add(512);
        let columns = state
            .columns
            .records(ex, live)
            .map_err(|e| exception(ex, e))?;
        live = live.saturating_add(
            (columns.len() as u64).saturating_mul(std::mem::size_of::<(u32, Value)>() as u64),
        );
        ex.reserve(live)?;
        let mut cells = Cells(vec_with_capacity(ex, columns.len() as u64)?);
        for column in &columns {
            let value = if materialized {
                Value::Undefined
            } else {
                default_cell(ex, column, live).map_err(|e| exception(ex, e))?
            };
            live = live.saturating_add(value_bytes(&value));
            ex.reserve(live)?;
            cells.0.push((column.id, value));
        }
        let record = Rc::new(RowRecord {
            id,
            hash: creation_number(),
            member: Cell::new(true),
            cells: RefCell::new(cells),
            bytes: Cell::new(0),
            materialized: Cell::new(materialized),
        });
        record.bytes.set(record.calculate_bytes());
        let mut rows = state.rows.borrow_mut();
        if rows.len() == rows.capacity() {
            return Err(ex.out_of_memory());
        }
        state
            .records_bytes
            .set(state.records_bytes.get().saturating_add(record.bytes.get()));
        rows.push(record);
        Ok(())
    }

    pub(crate) fn serial_restore_cell(
        &self,
        ex: &mut Execution,
        position: usize,
        value: Value,
    ) -> Result<(), Exception> {
        let live = self.state.live(ex).map_err(|e| exception(ex, e))?;
        ex.reserve(
            live.saturating_add(value_bytes(&value))
                .saturating_add(bridge::capacity_extra(&value)),
        )?;
        let rows = self.state.rows.borrow();
        let record = rows.last().ok_or_else(|| ex.out_of_memory())?;
        ex.charge(record.cells.borrow().0.len() as u64 + 1)?;
        let mut cells = record.cells.borrow_mut();
        let cell = cells
            .0
            .get_mut(position)
            .ok_or_else(|| ex.out_of_memory())?;
        cell.1 = value;
        drop(cells);
        let before = record.bytes.get();
        let after = record.calculate_bytes();
        record.bytes.set(after);
        self.state.records_bytes.set(
            self.state
                .records_bytes
                .get()
                .saturating_sub(before)
                .saturating_add(after),
        );
        Ok(())
    }
}

impl ValueTable {
    pub(crate) fn serial_admit(
        ex: &mut Execution,
        table: &Value,
        incoming: Option<&Value>,
        scratch: u64,
    ) -> Result<(), Exception> {
        ex.measure_retained_lenient(scratch, |visit| {
            visit.value(table)?;
            if let Some(incoming) = incoming {
                visit.value(incoming)?;
            }
            Ok(())
        })?;
        Ok(())
    }
}
