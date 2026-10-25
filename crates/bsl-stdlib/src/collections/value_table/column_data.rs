use super::*;
use crate::collections::array::Array;

enum Inputs<'a> {
    Load(&'a [Value]),
    Fill(&'a Value),
}
impl Inputs<'_> {
    fn count(&self, rows: usize) -> usize {
        match self {
            Self::Load(values) => rows.min(values.len()),
            Self::Fill(_) => rows,
        }
    }
    fn value(&self, index: usize) -> &Value {
        match self {
            Self::Load(values) => &values[index],
            Self::Fill(value) => value,
        }
    }
}

impl TableState {
    pub(super) fn unload_column(&self, ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
        let revision = self.revision.get();
        let schema = self.columns.revision();
        let live = self.live(ex)?;
        let key = arg_value_cached(ex, args.first(), live)?;
        let live = live.saturating_add(value_bytes(&key));
        let column = self.search_column(ex, &key, live)?;
        self.check_revision(revision, schema)?;
        let count = self.len();
        ex.charge(count as u64)?;
        let mut live = live
            .saturating_add((count as u64).saturating_mul(std::mem::size_of::<Value>() as u64))
            .saturating_add(
                (std::mem::size_of::<Array>() + 2 * std::mem::size_of::<usize>()) as u64,
            );
        ex.reserve(live)?;
        let mut values = vec_with_capacity(ex, count as u64)?;
        for row in self.rows.borrow().iter() {
            let value = self.search_cell(ex, row, column.id, live)?;
            live = live.saturating_add(scalar_clone_bytes(&value));
            values.push(value);
        }
        Ok(Array::new_value(values, false))
    }

    pub(super) fn load_column(
        self: &Rc<Self>,
        ex: &mut Execution,
        args: &mut [Arg],
    ) -> CallResult<Value> {
        let revision = self.revision.get();
        let schema = self.columns.revision();
        let live = self.live(ex)?;
        let source = arg_value_cached(ex, args.first(), live)?;
        let array = Array::of(&source)
            .filter(|a| !a.is_fixed())
            .ok_or_else(|| type_mismatch(1))?;
        let (values, live) = array.guarded_items(ex, live)?;
        let key = arg_value_cached(ex, args.get(1), live)?;
        let live = live.saturating_add(value_bytes(&key));
        let column = self.search_column(ex, &key, live)?;
        self.check_revision(revision, schema)?;
        if values.is_empty() || self.len() == 0 {
            return Ok(Value::Undefined);
        }
        let prepared = self.prepare_column_values(
            ex,
            &[column],
            Inputs::Load(&values),
            live,
            revision,
            schema,
        )?;
        prepared.check()?;
        prepared.commit();
        Ok(Value::Undefined)
    }

    pub(super) fn fill_values(
        self: &Rc<Self>,
        ex: &mut Execution,
        args: &mut [Arg],
    ) -> CallResult<Value> {
        let revision = self.revision.get();
        let schema = self.columns.revision();
        let live = self.live(ex)?;
        let value = arg_value_cached(ex, args.first(), live)?;
        let live = live.saturating_add(value_bytes(&value));
        ex.reserve(live)?;
        let selector = arg_value_cached(ex, args.get(1), live)?;
        let live = live.saturating_add(value_bytes(&selector));
        ex.reserve(live)?;
        let columns = self.search_columns(ex, &selector, live)?;
        let live = live.saturating_add(
            (columns.capacity() as u64)
                .saturating_mul(std::mem::size_of::<Rc<ColumnRecord>>() as u64),
        );
        self.check_revision(revision, schema)?;
        if columns.is_empty() || self.len() == 0 {
            return Ok(Value::Undefined);
        }
        let prepared =
            self.prepare_column_values(ex, &columns, Inputs::Fill(&value), live, revision, schema)?;
        prepared.check()?;
        prepared.commit();
        Ok(Value::Undefined)
    }

    fn prepare_column_values(
        self: &Rc<Self>,
        ex: &mut Execution,
        columns: &[Rc<ColumnRecord>],
        inputs: Inputs<'_>,
        live: u64,
        revision: u64,
        schema: u64,
    ) -> CallResult<Box<PreparedRows>> {
        let count = inputs.count(self.len());
        let mut live = live
            .saturating_add(std::mem::size_of::<PreparedRows>() as u64)
            .saturating_add((count as u64).saturating_mul(
                (std::mem::size_of::<Rc<RowRecord>>()
                    + std::mem::size_of::<(Rc<RowRecord>, Cells)>()) as u64,
            ));
        ex.reserve(live)?;
        ex.charge(count as u64)?;
        let mut rows = vec_with_capacity(ex, count as u64)?;
        rows.extend(self.rows.borrow().iter().take(count).cloned());
        let mut prepared = Box::new(PreparedRows {
            state: self.clone(),
            revision,
            schema,
            cells: vec_with_capacity(ex, count as u64)?,
            clear: false,
            retired: Vec::new(),
        });
        for (index, record) in rows.iter().enumerate() {
            let count = record.cells.borrow().0.len();
            ex.charge((count as u64).saturating_mul(columns.len() as u64 + 2))?;
            live = live.saturating_add(
                (count as u64).saturating_mul(std::mem::size_of::<(u32, Value)>() as u64),
            );
            ex.reserve(live)?;
            let mut cells = Cells(vec_with_capacity(ex, count as u64)?);
            for cell_index in 0..count {
                let (id, value) = {
                    let old = record.cells.borrow();
                    let (id, value) = &old.0[cell_index];
                    (*id, clone_cell(ex, value, live)?)
                };
                let value = if let Some(column) = columns.iter().find(|c| c.id == id) {
                    let input = clone_cell(
                        ex,
                        inputs.value(index),
                        live.saturating_add(scalar_clone_bytes(&value)),
                    )?;
                    let adjusted = adjust_cell(
                        ex,
                        &column.value_type(),
                        input,
                        live.saturating_add(scalar_clone_bytes(&value)),
                    )?;
                    self.check_revision(revision, schema)?;
                    adjusted
                } else {
                    value
                };
                live = live.saturating_add(value_bytes(&value));
                ex.reserve(live)?;
                cells.0.push((id, value));
            }
            prepared.cells.push((record.clone(), cells));
            self.check_revision(revision, schema)?;
        }
        prepared.retired = super::super::retire::prepare(ex, live, |visit| {
            for (record, _) in &prepared.cells {
                for (_, value) in &record.cells.borrow().0 {
                    visit(value)?;
                }
            }
            Ok(())
        })?;
        self.check_revision(revision, schema)?;
        Ok(prepared)
    }
}
