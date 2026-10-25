use super::*;
use crate::sort::{SortKey, sort_by_keys, sort_key};

fn bad_name() -> HostError {
    HostError::key("core85:IDS_VT_COLUMNCODE_ERROR")
}
fn blank(u: &u16) -> bool {
    matches!(*u, 9 | 10 | 13 | 32)
}
fn numeric_slack(value: &Value) -> u64 {
    match value {
        Value::Number(n) => (n.backing_capacity_limbs() as u64)
            .saturating_mul(4)
            .saturating_sub(scalar_clone_bytes(value)),
        _ => 0,
    }
}
fn retained_bytes(value: &Value) -> u64 {
    value_bytes(value).saturating_add(numeric_slack(value))
}

impl TableState {
    fn sort_source_live(&self, ex: &mut Execution) -> CallResult<u64> {
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
    fn sort_columns(
        &self,
        ex: &mut Execution,
        selector: &Str,
        live: u64,
    ) -> CallResult<Vec<(Rc<ColumnRecord>, bool)>> {
        ex.charge(selector.len() as u64 / 32 + 1)?;
        if selector.is_empty() {
            return Ok(Vec::new());
        }
        let n = selector
            .units()
            .iter()
            .filter(|u| **u == u16::from(b','))
            .count() as u64
            + 1;
        let live = live.saturating_add(
            n.saturating_mul(std::mem::size_of::<(Rc<ColumnRecord>, bool)>() as u64),
        );
        ex.reserve(live)?;
        let mut columns = vec_with_capacity(ex, n)?;
        for clause in selector.units().split(|u| *u == u16::from(b',')) {
            ex.reserve(
                live.saturating_add((clause.len() as u64).saturating_mul(42))
                    .saturating_add(2048),
            )?;
            let mut words = clause.split(blank).filter(|part| !part.is_empty());
            let name = words.next().ok_or_else(bad_name)?;
            let desc = match words.next() {
                None => false,
                Some(direction) => {
                    match bsl_prim::fold_name(&Str::from_units(direction).to_string_lossy())
                        .as_str()
                    {
                        "возр" | "asc" => false,
                        "убыв" | "desc" => true,
                        _ => return Err(bad_name().into()),
                    }
                }
            };
            if words.next().is_some() {
                return Err(bad_name().into());
            }
            let name = Str::from_units(name);
            let column = self
                .columns
                .named_record(
                    ex,
                    &name,
                    live.saturating_add((name.len() as u64).saturating_mul(2)),
                )?
                .ok_or_else(bad_name)?;
            columns.push((column, desc));
        }
        Ok(columns)
    }
    pub(super) fn sort_rows(&self, ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
        let revision = self.revision.get();
        let schema = self.columns.revision();
        let mut live = self.sort_source_live(ex)?;
        for arg in args.iter() {
            if let Arg::Val(value) = arg {
                live = live.saturating_add(retained_bytes(value));
            }
        }
        ex.reserve(live)?;
        let selector = arg_value_cached(ex, args.first(), live)?;
        live = live.saturating_add(if matches!(args.first(), Some(Arg::Val(_))) {
            scalar_clone_bytes(&selector)
        } else {
            retained_bytes(&selector)
        });
        ex.reserve(live)?;
        let Value::String(ref name) = selector else {
            return Err(type_mismatch(1).into());
        };
        let compare = arg_value_cached(ex, args.get(1), live)?;
        live = live.saturating_add(if matches!(args.get(1), Some(Arg::Val(_))) {
            scalar_clone_bytes(&compare)
        } else {
            retained_bytes(&compare)
        });
        ex.reserve(live)?;
        if !matches!(compare, Value::Undefined)
            && !compare.as_object().is_some_and(|o| {
                o.as_any()
                    .is::<crate::collections::compare::CompareValues>()
            })
        {
            return Err(type_mismatch(2).into());
        }
        self.check_revision(revision, schema)?;
        let columns = self.sort_columns(ex, name, live)?;
        if columns.is_empty() {
            return Ok(Value::Undefined);
        }
        let n = self.len() as u64;
        let width = columns.len() as u64;
        live = live
            .saturating_add(
                (columns.capacity() as u64)
                    .saturating_mul(std::mem::size_of::<(Rc<ColumnRecord>, bool)>() as u64),
            )
            .saturating_add(width.saturating_mul(std::mem::size_of::<bool>() as u64))
            .saturating_add(n.saturating_mul(
                (std::mem::size_of::<Rc<RowRecord>>()
                    + std::mem::size_of::<Vec<SortKey>>()
                    + 2 * std::mem::size_of::<usize>()) as u64,
            ))
            .saturating_add(
                n.saturating_mul(width)
                    .saturating_mul(std::mem::size_of::<SortKey>() as u64),
            );
        ex.reserve(live)?;
        ex.charge(n.saturating_mul(width))?;
        let mut rows = vec_with_capacity(ex, n)?;
        rows.extend(self.rows.borrow().iter().cloned());
        let mut keys = vec_with_capacity(ex, n)?;
        let mut desc = vec_with_capacity(ex, width)?;
        desc.extend(columns.iter().map(|(_, reverse)| *reverse));
        for row in &rows {
            let mut row_keys = vec_with_capacity(ex, width)?;
            for (column, _) in &columns {
                let value = self.search_cell(ex, row, column.id, live)?;
                live = live.saturating_add(scalar_clone_bytes(&value));
                let key = sort_key(ex, &value, &mut live);
                self.check_revision(revision, schema)?;
                row_keys.push(key?);
            }
            keys.push(row_keys);
        }
        ex.reserve(live)?;
        sort_by_keys(ex, &mut rows, &keys, &desc)?;
        self.check_revision(revision, schema)?;
        let old = std::mem::replace(&mut *self.rows.borrow_mut(), rows);
        self.bump();
        drop(old);
        Ok(Value::Undefined)
    }
}
