use super::*;
use crate::collections::{same_value, value_list::guard_equal};
use bsl_prim::Number;

struct Group {
    keys: Vec<Value>,
    sums: Vec<Number>,
}
fn bad_name() -> HostError {
    HostError::key("core85:IDS_VT_COLUMNCODE_ERROR")
}
fn vector_bytes<T>(n: u64) -> u64 {
    n.saturating_mul(std::mem::size_of::<T>() as u64)
}
fn number_bytes(n: &Number) -> u64 {
    (n.backing_capacity_limbs() as u64).saturating_mul(4)
}
fn numeric_slack(value: &Value) -> u64 {
    match value {
        Value::Number(n) => number_bytes(n).saturating_sub(scalar_clone_bytes(value)),
        _ => 0,
    }
}
fn retained_value_bytes(value: &Value) -> u64 {
    value_bytes(value).saturating_add(numeric_slack(value))
}
fn number_work(value: &Value) -> u64 {
    match value {
        Value::Number(n) => n.size_hint_limbs(),
        _ => 0,
    }
}
impl TableState {
    fn group_source_live(&self, ex: &mut Execution) -> CallResult<u64> {
        let mut live = self.live(ex)?;
        ex.charge(self.len() as u64)?;
        for row in self.rows.borrow().iter() {
            let cells = row.cells.borrow();
            ex.charge(cells.0.len() as u64)?;
            for (_, value) in &cells.0 {
                live = live.saturating_add(numeric_slack(value));
            }
        }
        ex.reserve(live)?;
        Ok(live)
    }
    fn group_columns(
        &self,
        ex: &mut Execution,
        selector: &Str,
        live: u64,
    ) -> CallResult<Vec<Rc<ColumnRecord>>> {
        ex.charge(selector.len() as u64 / 32 + 1)?;
        if selector.is_empty() {
            return Ok(Vec::new());
        }
        let count = selector.units().iter().filter(|u| **u == 44).count() as u64 + 1;
        let live = live.saturating_add(vector_bytes::<Rc<ColumnRecord>>(count));
        ex.reserve(live)?;
        let mut columns: Vec<Rc<ColumnRecord>> = vec_with_capacity(ex, count)?;
        for clause in selector.units().split(|u| *u == 44) {
            let blank = |u: &u16| matches!(*u, 9 | 10 | 13 | 32);
            let first = clause
                .iter()
                .position(|u| !blank(u))
                .unwrap_or(clause.len());
            let end = clause
                .iter()
                .rposition(|u| !blank(u))
                .map_or(first, |i| i + 1);
            let clause = &clause[first..end];
            if clause.is_empty() {
                return Err(bad_name().into());
            }
            ex.reserve(
                live.saturating_add((clause.len() as u64).saturating_mul(42))
                    .saturating_add(2048),
            )?;
            let name = Str::from_units(clause);
            let column = self
                .columns
                .named_record(
                    ex,
                    &name,
                    live.saturating_add((name.len() as u64).saturating_mul(2)),
                )?
                .ok_or_else(bad_name)?;
            ex.charge(columns.len() as u64)?;
            if !columns.iter().any(|c| c.id == column.id) {
                columns.push(column);
            }
        }
        Ok(columns)
    }

    pub(super) fn group(
        self: &Rc<Self>,
        ex: &mut Execution,
        args: &mut [Arg],
    ) -> CallResult<Value> {
        let revision = self.revision.get();
        let schema = self.columns.revision();
        let mut live = self.group_source_live(ex)?;
        for arg in args.iter() {
            if let Arg::Val(value) = arg {
                live = live.saturating_add(retained_value_bytes(value));
            }
        }
        ex.reserve(live)?;
        let keys = arg_value_cached(ex, args.first(), live)?;
        live = live.saturating_add(if matches!(args.first(), Some(Arg::Val(_))) {
            scalar_clone_bytes(&keys)
        } else {
            retained_value_bytes(&keys)
        });
        ex.reserve(live)?;
        let Value::String(ref key_names) = keys else {
            return Err(type_mismatch(1).into());
        };
        let sums = arg_value_cached(ex, args.get(1), live)?;
        live = live.saturating_add(if matches!(args.get(1), Some(Arg::Val(_))) {
            scalar_clone_bytes(&sums)
        } else {
            retained_value_bytes(&sums)
        });
        ex.reserve(live)?;
        let sum_names = match &sums {
            Value::Undefined => Str::new(),
            Value::String(s) => s.clone(),
            _ => return Err(type_mismatch(2).into()),
        };
        self.check_revision(revision, schema)?;
        let key_columns = self.group_columns(ex, key_names, live)?;
        live = live.saturating_add(vector_bytes::<Rc<ColumnRecord>>(
            key_columns.capacity() as u64
        ));
        let sum_columns = self.group_columns(ex, &sum_names, live)?;
        live = live.saturating_add(vector_bytes::<Rc<ColumnRecord>>(
            sum_columns.capacity() as u64
        ));
        ex.charge((key_columns.len() as u64).saturating_mul(sum_columns.len() as u64))?;
        if key_columns
            .iter()
            .any(|k| sum_columns.iter().any(|s| k.id == s.id))
        {
            return Err(value_mismatch(2).into());
        }
        let mut selected = self.columns.records(ex, live)?;
        live = live.saturating_add(vector_bytes::<Rc<ColumnRecord>>(selected.capacity() as u64));
        let key_width = key_columns.len() as u64;
        let sum_width = sum_columns.len() as u64;
        ex.charge((selected.len() as u64).saturating_mul(key_width.saturating_add(sum_width)))?;
        selected.retain(|c| key_columns.iter().chain(&sum_columns).any(|s| s.id == c.id));
        let width = selected.len() as u64;
        live = live.saturating_add(vector_bytes::<(bool, usize)>(width));
        ex.reserve(live)?;
        let mut layout = vec_with_capacity(ex, width)?;
        ex.charge(width.saturating_mul(key_width.saturating_add(sum_width)))?;
        for c in &selected {
            if let Some(i) = key_columns.iter().position(|k| k.id == c.id) {
                layout.push((false, i));
            } else if let Some(i) = sum_columns.iter().position(|s| s.id == c.id) {
                layout.push((true, i));
            } else {
                return Err(changed().into());
            }
        }
        let n = self.len() as u64;
        live = live.saturating_add(n.saturating_mul(
            (std::mem::size_of::<Group>()
                + 2 * std::mem::size_of::<Rc<RowRecord>>()
                + std::mem::size_of::<RowRecord>()
                + 2 * std::mem::size_of::<usize>()) as u64
                + vector_bytes::<Value>(key_width)
                + vector_bytes::<Number>(sum_width)
                + vector_bytes::<(u32, Value)>(width),
        ));
        ex.reserve(live)?;
        ex.charge(n)?;
        let mut rows = vec_with_capacity(ex, n)?;
        rows.extend(self.rows.borrow().iter().cloned());
        let mut groups: Vec<Group> = vec_with_capacity(ex, n)?;
        self.check_revision(revision, schema)?;
        for row in &rows {
            let mut row_keys = vec_with_capacity(ex, key_width)?;
            let mut row_live = live;
            for column in &key_columns {
                let value = self.search_cell(ex, row, column.id, row_live)?;
                row_live = row_live.saturating_add(value_bytes(&value));
                ex.reserve(row_live)?;
                row_keys.push(value);
            }
            let mut found = None;
            for (i, group) in groups.iter().enumerate() {
                ex.charge(1)?;
                let mut equal = true;
                for (a, b) in group.keys.iter().zip(&row_keys) {
                    ex.charge(1)?;
                    guard_equal(ex, a, b, row_live)?;
                    ex.charge(number_work(a).saturating_add(number_work(b)) / 64 + 1)?;
                    let same = same_value(a, b);
                    self.check_revision(revision, schema)?;
                    if !same {
                        equal = false;
                        break;
                    }
                }
                if equal {
                    found = Some(i);
                    break;
                }
            }
            let i = if let Some(i) = found {
                i
            } else {
                live = row_live;
                let mut totals = vec_with_capacity(ex, sum_width)?;
                ex.charge(sum_width)?;
                for _ in 0..sum_width {
                    let zero = Number::from_i64(0);
                    live = live.saturating_add(number_bytes(&zero));
                    ex.reserve(live)?;
                    totals.push(zero);
                }
                row_live = live;
                let i = groups.len();
                groups.push(Group {
                    keys: row_keys,
                    sums: totals,
                });
                i
            };
            for (column, sum) in sum_columns.iter().zip(&mut groups[i].sums) {
                let cells = row.cells.borrow();
                ex.charge(cells.0.len() as u64)?;
                if let Some((_, Value::Number(value))) =
                    cells.0.iter().find(|(id, _)| *id == column.id)
                {
                    let limbs = sum
                        .size_hint_limbs()
                        .saturating_add(value.size_hint_limbs())
                        .saturating_add(2);
                    ex.charge(limbs / 64 + 1)?;
                    ex.reserve(row_live.saturating_add(limbs.saturating_mul(32)))?;
                    let next = sum.add(value);
                    let old_bytes = number_bytes(sum);
                    let next_bytes = number_bytes(&next);
                    live = live.saturating_sub(old_bytes).saturating_add(next_bytes);
                    row_live = row_live
                        .saturating_sub(old_bytes)
                        .saturating_add(next_bytes);
                    ex.reserve(row_live)?;
                    *sum = next;
                }
            }
        }
        let count = groups.len() as u64;
        let next_id = self
            .next_id
            .get()
            .checked_add(count)
            .ok_or_else(|| ex.out_of_memory())?;
        let mut output = vec_with_capacity(ex, count)?;
        let mut total = 0u64;
        for (offset, group) in groups.iter().enumerate() {
            let mut cells = Cells(vec_with_capacity(ex, width)?);
            ex.charge(width)?;
            for (column, &(sum, index)) in selected.iter().zip(&layout) {
                let value = if sum {
                    ex.charge(group.sums[index].size_hint_limbs() / 64)?;
                    ex.reserve(live.saturating_add(number_bytes(&group.sums[index])))?;
                    Value::Number(group.sums[index].clone())
                } else {
                    clone_cell(ex, &group.keys[index], live)?
                };
                let input_live = live.saturating_add(value_bytes(&value));
                ex.reserve(input_live)?;
                let adjusted = adjust_cell(ex, &column.value_type(), value, input_live);
                self.check_revision(revision, schema)?;
                let value = adjusted?;
                live = live.saturating_add(value_bytes(&value));
                ex.reserve(live)?;
                cells.0.push((column.id, value));
            }
            let record = Rc::new(RowRecord {
                id: self.next_id.get() + offset as u64,
                hash: creation_number(),
                member: Cell::new(true),
                cells: RefCell::new(cells),
                bytes: Cell::new(0),
            });
            ex.charge(width)?;
            record.bytes.set(record.calculate_bytes());
            total = total.saturating_add(record.bytes.get());
            output.push(record);
        }
        let (prepared, live) = self.columns.prepare_keep(ex, &selected, live)?;
        let mut retired =
            super::super::retire::prepare(ex, live, |visit| self.visit_values(visit))?;
        self.check_revision(revision, schema)?;
        prepared.check()?;
        drop(rows);
        drop(groups);
        self.retire_into(None, &mut retired);
        prepared.commit();
        *self.rows.borrow_mut() = output;
        self.records_bytes.set(total);
        self.next_id.set(next_id);
        self.bump();
        super::super::retire::run(retired);
        Ok(Value::Undefined)
    }
}
