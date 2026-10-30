use super::*;

pub(super) fn capacity_extra(value: &Value) -> u64 {
    match value {
        Value::String(text) => super::super::retained::text_extra(text),
        Value::Number(number) => (number.backing_capacity_limbs() as u64)
            .saturating_mul(4)
            .saturating_sub(scalar_clone_bytes(value)),
        _ => 0,
    }
}

fn borrowed_storage(ex: &mut Execution, cells: &[Value], base: u64) -> CallResult<(u64, u64)> {
    ex.charge(cells.len() as u64)?;
    let mut live = base
        .saturating_add((cells.len() as u64).saturating_mul(std::mem::size_of::<Value>() as u64));
    ex.reserve(live)?;
    let mut extra = 0u64;
    for value in cells {
        let represented = match value {
            Value::String(text) => (text.len() as u64).saturating_mul(2),
            Value::Number(_) => scalar_clone_bytes(value),
            _ => crate::values::binary_data_bytes_direct(value).map_or(0, |b| b.len() as u64),
        };
        let slack = capacity_extra(value);
        extra = extra.saturating_add(slack);
        live = live.saturating_add(represented).saturating_add(slack);
        ex.reserve(live)?;
    }
    Ok((live, extra))
}

const DIAGNOSTIC_BYTES: u64 = 2048;

impl ValueTable {
    fn producer_live(&self, ex: &mut Execution) -> CallResult<(u64, u64)> {
        let state = &self.state;
        let base = state.live(ex)?;
        ex.reserve(base)?;
        ex.charge(1)?;
        let revision = state.revision.get();
        let schema = state.columns.revision();
        if let Some((old_revision, old_schema, extra)) = state.producer_capacity_extra.get()
            && old_revision == revision
            && old_schema == schema
        {
            let live = base.saturating_add(extra);
            ex.reserve(live)?;
            return Ok((live, extra));
        }
        let rows = state.rows.borrow();
        ex.charge(rows.len() as u64)?;
        let mut extra = 0u64;
        for row in rows.iter() {
            let cells = row.cells.borrow();
            ex.charge(cells.0.len() as u64)?;
            for (_, value) in &cells.0 {
                extra = extra.saturating_add(capacity_extra(value));
                ex.reserve(base.saturating_add(extra))?;
            }
        }
        ex.charge(state.columns.len() as u64)?;
        for index in 0..state.columns.len() {
            let column = state.columns.cell_column(ex, &Value::from(index as i64))?;
            extra = extra.saturating_add(column.text_slack_bytes());
            ex.reserve(base.saturating_add(extra))?;
        }
        state
            .producer_capacity_extra
            .set(Some((revision, schema, extra)));
        Ok((base.saturating_add(extra), extra))
    }

    pub fn columns(&self, ex: &mut Execution) -> CallResult<Value> {
        self.producer_live(ex)?;
        Ok(Value::Object(self.state.columns.clone()))
    }
    pub fn rows(&self, ex: &mut Execution) -> CallResult<Vec<Value>> {
        let state = &self.state;
        let (base, _) = self.producer_live(ex)?;
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
    pub fn add_row(&self, ex: &mut Execution, cells: &[Value]) -> CallResult<Value> {
        ex.with_reuse_check(|ex| self.add_row_guarded(ex, cells))
    }
    fn add_row_guarded(&self, ex: &mut Execution, cells: &[Value]) -> CallResult<Value> {
        let state = &self.state;
        let revision = state.revision.get();
        let schema = state.columns.revision();
        let (base, receiver_extra) = self.producer_live(ex)?;
        let (live, input_extra) = borrowed_storage(ex, cells, base)?;
        if cells.len() > state.columns.len() {
            ex.reserve(live.saturating_add(DIAGNOSTIC_BYTES))?;
            return Err(value_mismatch(1).into());
        }
        state.insert_row(
            ex,
            state.len(),
            revision,
            schema,
            Some(cells),
            Some(receiver_extra.saturating_add(input_extra)),
        )
    }
    pub fn cell(&self, ex: &mut Execution, row: usize, col: usize) -> CallResult<Value> {
        let state = &self.state;
        let (live, _) = self.producer_live(ex)?;
        if row >= state.len() || col >= state.columns.len() {
            ex.reserve(live.saturating_add(DIAGNOSTIC_BYTES))?;
            return Err(HostError::key(msg::INDEX_OFFBOUND).into());
        }
        let column = state.columns.cell_column(ex, &Value::from(col as i64))?;
        let record = state.rows.borrow()[row].clone();
        state.search_cell(ex, &record, column.id, live)
    }
}
