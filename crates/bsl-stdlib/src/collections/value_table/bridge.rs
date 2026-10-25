use super::*;

impl ValueTable {
    pub(crate) fn columns(&self) -> Value {
        Value::Object(self.state.columns.clone())
    }
    #[allow(
        dead_code,
        reason = "guarded producer contract for Task14 internal API"
    )]
    pub(crate) fn rows(&self, ex: &mut Execution) -> CallResult<Vec<Value>> {
        let state = &self.state;
        let base = state.live(ex)?;
        let (records, live) = state.selected_records(
            ex,
            &Value::Undefined,
            base,
            state.revision.get(),
            state.columns.revision(),
        )?;
        let live = live.saturating_add((records.len() as u64).saturating_mul(
            (std::mem::size_of::<Value>()
                + std::mem::size_of::<TableRow>()
                + 2 * std::mem::size_of::<usize>()) as u64,
        ));
        ex.reserve(live)?;
        ex.charge(records.len() as u64)?;
        let mut rows = vec_with_capacity(ex, records.len() as u64)?;
        for record in records {
            rows.push(state.handle(ex, record)?);
        }
        Ok(rows)
    }
    #[allow(
        dead_code,
        reason = "guarded producer contract for Task14 internal API"
    )]
    pub(crate) fn add_row(&self, ex: &mut Execution, cells: &[Value]) -> CallResult<Value> {
        let state = &self.state;
        let revision = state.revision.get();
        let schema = state.columns.revision();
        if cells.len() > state.columns.len() {
            return Err(value_mismatch(1).into());
        }
        state.insert_row(ex, state.len(), revision, schema, Some(cells))
    }
    #[allow(
        dead_code,
        reason = "guarded producer contract for Task14 internal API"
    )]
    pub(crate) fn cell(&self, ex: &mut Execution, row: usize, col: usize) -> CallResult<Value> {
        let state = &self.state;
        if row >= state.len() || col >= state.columns.len() {
            return Err(HostError::key(msg::INDEX_OFFBOUND).into());
        }
        let live = state.live(ex)?;
        ex.reserve(live)?;
        let column = state.columns.cell_column(ex, &Value::from(col as i64))?;
        let record = state.rows.borrow()[row].clone();
        state.search_cell(ex, &record, column.id, live)
    }
}
