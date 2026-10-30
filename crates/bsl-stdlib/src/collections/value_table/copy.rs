use super::*;
use crate::collections::{array::Array, structure::Structure};

impl TableState {
    pub(super) fn selected_records(
        &self,
        ex: &mut Execution,
        selector: &Value,
        live: u64,
        revision: u64,
        schema: u64,
    ) -> CallResult<(Vec<Rc<RowRecord>>, u64)> {
        if Structure::of(selector).is_some() {
            return self.matching_records(ex, selector, live, revision, schema);
        }
        if matches!(selector, Value::Undefined) {
            let count = self.len();
            ex.charge(count as u64)?;
            let live = live.saturating_add(
                (count as u64).saturating_mul(std::mem::size_of::<Rc<RowRecord>>() as u64),
            );
            ex.reserve(live)?;
            let mut records = vec_with_capacity(ex, count as u64)?;
            records.extend(self.rows.borrow().iter().cloned());
            return Ok((records, live));
        }
        let array = Array::of(selector)
            .filter(|a| !a.is_fixed())
            .ok_or_else(|| type_mismatch(1))?;
        let (items, live) = array.guarded_items(ex, live)?;
        let live = live.saturating_add(
            (items.len() as u64).saturating_mul(std::mem::size_of::<Rc<RowRecord>>() as u64),
        );
        ex.reserve(live)?;
        ex.charge(items.len() as u64)?;
        let mut records = vec_with_capacity(ex, items.len() as u64)?;
        for item in &items {
            let row = item
                .as_object()
                .and_then(|o| o.as_any().downcast_ref::<TableRow>())
                .filter(|r| std::ptr::eq(self, &*r.state) && r.record.member.get())
                .ok_or_else(|| value_mismatch(1))?;
            records.push(row.record.clone());
        }
        self.check_revision(revision, schema)?;
        Ok((records, live))
    }

    pub(super) fn copy_table(
        self: &Rc<Self>,
        ex: &mut Execution,
        args: &mut [Arg],
        columns_only: bool,
    ) -> CallResult<Value> {
        let revision = self.revision.get();
        let schema = self.columns.revision();
        let live = self.live(ex)?;
        let selector = if columns_only {
            Value::Undefined
        } else {
            arg_value_cached(ex, args.first(), live)?
        };
        let live = live.saturating_add(value_bytes(&selector));
        ex.reserve(live)?;
        let column_selector = arg_value_cached(ex, args.get(usize::from(!columns_only)), live)?;
        let live = live.saturating_add(value_bytes(&column_selector));
        ex.reserve(live)?;
        let mut columns = self
            .search_columns(ex, &column_selector, live)
            .map_err(|error| {
                if columns_only && !matches!(column_selector, Value::Undefined | Value::String(_)) {
                    type_mismatch(1).into()
                } else {
                    error
                }
            })?;
        let live = live.saturating_add(
            (columns.capacity() as u64)
                .saturating_mul(std::mem::size_of::<Rc<ColumnRecord>>() as u64),
        );
        ex.charge((columns.len() as u64).saturating_mul(columns.len() as u64))?;
        let mut unique = 0;
        for i in 0..columns.len() {
            if !columns[..unique].iter().any(|c| c.id == columns[i].id) {
                columns.swap(unique, i);
                unique += 1;
            }
        }
        columns.truncate(unique);
        self.check_revision(revision, schema)?;
        let (rows, live) = if columns_only {
            (Vec::new(), live)
        } else {
            self.selected_records(ex, &selector, live, revision, schema)?
        };
        let next_id = u64::try_from(rows.len()).map_err(|_| ex.out_of_memory())?;
        let (copied_columns, mut live) = self.columns.copy_selected(ex, &columns, live)?;
        live = live
            .saturating_add(
                (std::mem::size_of::<Self>()
                    + std::mem::size_of::<ValueTable>()
                    + 4 * std::mem::size_of::<usize>()) as u64,
            )
            .saturating_add(
                (rows.len() as u64).saturating_mul(std::mem::size_of::<Rc<RowRecord>>() as u64),
            );
        ex.reserve(live)?;
        ex.charge(rows.len() as u64)?;
        let mut copied_rows = vec_with_capacity(ex, rows.len() as u64)?;
        let mut total = 0u64;
        for (id, row) in rows.iter().enumerate() {
            live = live
                .saturating_add(
                    (std::mem::size_of::<RowRecord>() + 2 * std::mem::size_of::<usize>()) as u64,
                )
                .saturating_add(
                    (columns.len() as u64)
                        .saturating_mul(std::mem::size_of::<(u32, Value)>() as u64),
                );
            ex.reserve(live)?;
            ex.charge(columns.len() as u64)?;
            let mut cells = Cells(vec_with_capacity(ex, columns.len() as u64)?);
            for (new_id, column) in columns.iter().enumerate() {
                let value = self.search_cell(ex, row, column.id, live)?;
                live = live.saturating_add(value_bytes(&value));
                ex.reserve(live)?;
                cells.0.push((new_id as u32, value));
            }
            let record = Rc::new(RowRecord {
                id: id as u64,
                hash: creation_number(),
                member: Cell::new(true),
                cells: RefCell::new(cells),
                bytes: Cell::new(0),
            });
            ex.charge(columns.len() as u64)?;
            record.bytes.set(record.calculate_bytes());
            total = total.saturating_add(record.bytes.get());
            copied_rows.push(record);
        }
        self.check_revision(revision, schema)?;
        let indexes = indexes::Indexes::new(ex, copied_columns.clone(), live)?;
        let state = Rc::new(Self {
            columns: copied_columns,
            indexes,
            rows: RefCell::new(copied_rows),
            revision: Cell::new(0),
            next_id: Cell::new(next_id),
            records_bytes: Cell::new(total),
            producer_capacity_extra: Cell::new(None),
            hash: creation_number(),
        });
        let observer: Rc<dyn SchemaRows> = state.clone();
        state.columns.attach_rows(Rc::downgrade(&observer));
        state.table(ex)
    }
}
