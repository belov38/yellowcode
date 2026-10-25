use super::*;
use crate::collections::{array::Array, same_value, structure::Structure, value_list::guard_equal};
use bsl_prim::Number;

fn number_work(value: &Value) -> u64 {
    match value {
        Value::Number(n) => n.size_hint_limbs(),
        _ => 0,
    }
}

impl TableState {
    pub(super) fn search_column(
        &self,
        ex: &mut Execution,
        key: &Value,
        live: u64,
    ) -> CallResult<Rc<ColumnRecord>> {
        let units = match key {
            Value::String(s) => s.len() as u64,
            _ => 0,
        };
        ex.reserve(
            live.saturating_add(units.saturating_mul(40))
                .saturating_add(2048),
        )?;
        self.columns.cell_column(ex, key)
    }

    pub(super) fn search_columns(
        &self,
        ex: &mut Execution,
        selector: &Value,
        live: u64,
    ) -> CallResult<Vec<Rc<ColumnRecord>>> {
        let name = match selector {
            Value::Undefined => return self.columns.records(ex, live),
            Value::String(s) if s.is_empty() => return self.columns.records(ex, live),
            Value::String(s) => s,
            _ => return Err(type_mismatch(2).into()),
        };
        ex.charge(name.len() as u64 / 32 + 1)?;
        let n = name
            .units()
            .iter()
            .filter(|u| **u == u16::from(b','))
            .count() as u64
            + 1;
        let live =
            live.saturating_add(n.saturating_mul(std::mem::size_of::<Rc<ColumnRecord>>() as u64));
        ex.reserve(live)?;
        let mut columns = vec_with_capacity(ex, n)?;
        for units in name.units().split(|u| *u == u16::from(b',')) {
            let blank = |u: &u16| matches!(*u, 9 | 10 | 13 | 32);
            let first = units.iter().position(|u| !blank(u)).unwrap_or(units.len());
            let end = units
                .iter()
                .rposition(|u| !blank(u))
                .map_or(first, |i| i + 1);
            let units = &units[first..end];
            ex.reserve(
                live.saturating_add((units.len() as u64).saturating_mul(42))
                    .saturating_add(2048),
            )?;
            let key = Value::String(Str::from_units(units));
            columns.push(self.search_column(ex, &key, live)?);
        }
        Ok(columns)
    }

    pub(super) fn search_cell(
        &self,
        ex: &mut Execution,
        row: &RowRecord,
        id: u32,
        live: u64,
    ) -> CallResult<Value> {
        let cells = row.cells.borrow();
        ex.charge(cells.0.len() as u64)?;
        match cells.0.iter().find(|(key, _)| *key == id) {
            Some((_, value)) => clone_cell(ex, value, live),
            None => Ok(Value::Undefined),
        }
    }

    pub(super) fn total(&self, ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
        let rev = self.revision.get();
        let schema = self.columns.revision();
        let live = self.live(ex)?;
        let key = arg_value_cached(ex, args.first(), live)?;
        let live = live.saturating_add(scalar_clone_bytes(&key));
        let column = self.search_column(ex, &key, live)?;
        self.check_revision(rev, schema)?;
        ex.charge(self.len() as u64)?;
        let mut sum = Number::from_i64(0);
        for row in self.rows.borrow().iter() {
            let cells = row.cells.borrow();
            ex.charge(cells.0.len() as u64)?;
            if let Some((_, Value::Number(n))) = cells.0.iter().find(|(id, _)| *id == column.id) {
                let limbs = sum
                    .size_hint_limbs()
                    .saturating_add(n.size_hint_limbs())
                    .saturating_add(2);
                ex.charge(limbs / 64 + 1)?;
                ex.reserve(live.saturating_add(limbs.saturating_mul(32)))?;
                sum = sum.add(n);
            }
        }
        Ok(Value::Number(sum))
    }

    pub(super) fn find(self: &Rc<Self>, ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
        let rev = self.revision.get();
        let schema = self.columns.revision();
        let base = self.live(ex)?;
        let needle = arg_value_cached(ex, args.first(), base)?;
        let base = base.saturating_add(value_bytes(&needle));
        ex.reserve(base)?;
        let selector = arg_value_cached(ex, args.get(1), base)?;
        let base = base.saturating_add(value_bytes(&selector));
        ex.reserve(base)?;
        let columns = self.search_columns(ex, &selector, base)?;
        let live = base.saturating_add(
            (columns.capacity() as u64)
                .saturating_mul(std::mem::size_of::<Rc<ColumnRecord>>() as u64),
        );
        self.check_revision(rev, schema)?;
        let count = self.len();
        ex.charge(count as u64)?;
        for i in 0..count {
            let row = self.rows.borrow()[i].clone();
            for column in &columns {
                let value = self.search_cell(ex, &row, column.id, live)?;
                guard_equal(
                    ex,
                    &value,
                    &needle,
                    live.saturating_add(scalar_clone_bytes(&value)),
                )?;
                ex.charge(number_work(&value).saturating_add(number_work(&needle)) / 64 + 1)?;
                let equal = same_value(&value, &needle);
                self.check_revision(rev, schema)?;
                if equal {
                    ex.reserve(
                        live.saturating_add(scalar_clone_bytes(&value))
                            .saturating_add(
                                std::mem::size_of::<TableRow>() as u64
                                    + 2 * std::mem::size_of::<usize>() as u64,
                            ),
                    )?;
                    return self.handle(ex, row);
                }
            }
        }
        Ok(Value::Undefined)
    }

    pub(super) fn find_rows(
        self: &Rc<Self>,
        ex: &mut Execution,
        args: &mut [Arg],
    ) -> CallResult<Value> {
        let rev = self.revision.get();
        let schema = self.columns.revision();
        let base = self.live(ex)?;
        let filter = arg_value_cached(ex, args.first(), base)?;
        let (rows, live) = self.matching_records(ex, &filter, base, rev, schema)?;
        let result_bytes = (self.len() as u64).saturating_mul(
            (std::mem::size_of::<Value>()
                + std::mem::size_of::<TableRow>()
                + 2 * std::mem::size_of::<usize>()) as u64,
        );
        let live = live.saturating_add(result_bytes).saturating_add(
            (std::mem::size_of::<Array>() + 2 * std::mem::size_of::<usize>()) as u64,
        );
        ex.reserve(live)?;
        ex.charge(rows.len() as u64)?;
        let mut result = vec_with_capacity(ex, rows.len() as u64)?;
        for row in rows {
            result.push(self.handle(ex, row)?);
        }
        Ok(Array::new_value(result, false))
    }

    pub(super) fn matching_records(
        &self,
        ex: &mut Execution,
        filter: &Value,
        base: u64,
        rev: u64,
        schema: u64,
    ) -> CallResult<(Vec<Rc<RowRecord>>, u64)> {
        let structure = Structure::of(filter)
            .filter(|s| !s.is_fixed())
            .ok_or_else(|| type_mismatch(1))?;
        let (pairs, base) = structure.guarded_pairs(ex, base)?;
        let live = base.saturating_add(
            (pairs.len() as u64).saturating_mul(std::mem::size_of::<Rc<ColumnRecord>>() as u64),
        );
        ex.reserve(live)?;
        let mut columns = vec_with_capacity(ex, pairs.len() as u64)?;
        for (name, _) in &pairs {
            columns.push(self.search_column(ex, &Value::String(name.clone()), live)?);
        }
        self.check_revision(rev, schema)?;
        let count = self.len();
        let live = live.saturating_add(
            (count as u64).saturating_mul(std::mem::size_of::<Rc<RowRecord>>() as u64),
        );
        ex.reserve(live)?;
        ex.charge(count as u64)?;
        let mut result = vec_with_capacity(ex, count as u64)?;
        for i in 0..count {
            let row = self.rows.borrow()[i].clone();
            let mut matches = true;
            for (column, (_, needle)) in columns.iter().zip(&pairs) {
                let value = self.search_cell(ex, &row, column.id, live)?;
                guard_equal(
                    ex,
                    &value,
                    needle,
                    live.saturating_add(scalar_clone_bytes(&value)),
                )?;
                ex.charge(number_work(&value).saturating_add(number_work(needle)) / 64 + 1)?;
                let equal = same_value(&value, needle);
                self.check_revision(rev, schema)?;
                if !equal {
                    matches = false;
                    break;
                }
            }
            if matches {
                result.push(row);
            }
        }
        self.check_revision(rev, schema)?;
        Ok((result, live))
    }
}
