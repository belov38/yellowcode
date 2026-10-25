use super::*;
use crate::collections::{array::Array, same_value, structure::Structure, value_list::guard_equal};
use bsl_prim::Number;
const HANDLE_BYTES: u64 =
    (std::mem::size_of::<TreeRow>() + 2 * std::mem::size_of::<usize>()) as u64;
const ARRAY_BYTES: u64 = (std::mem::size_of::<Array>() + 2 * std::mem::size_of::<usize>()) as u64;

pub(super) struct Query<'a> {
    rows: &'a TreeRows,
    args: &'a [Arg],
    values: Vec<Value>,
    revision: u64,
    schema: u64,
    scratch: u64,
    source: Retained,
}
enum Retained {
    None,
    Structure,
    Array,
}
impl<'a> Query<'a> {
    pub(super) fn new(
        rows: &'a TreeRows,
        ex: &mut Execution,
        args: &'a [Arg],
        slots: u64,
        filter: bool,
    ) -> CallResult<Self> {
        let mut query = Self {
            rows,
            args,
            values: Vec::new(),
            revision: rows.state.revision.get(),
            schema: rows.state.columns.revision(),
            scratch: slots.saturating_mul(std::mem::size_of::<Value>() as u64),
            source: if filter {
                Retained::Structure
            } else {
                Retained::None
            },
        };
        query.reserve(ex, 0)?;
        query.values = vec_with_capacity(ex, slots)?;
        query.scratch =
            (query.values.capacity() as u64).saturating_mul(std::mem::size_of::<Value>() as u64);
        Ok(query)
    }
    pub(super) fn for_array(
        rows: &'a TreeRows,
        ex: &mut Execution,
        args: &'a [Arg],
    ) -> CallResult<Self> {
        let mut query = Self::new(rows, ex, args, 2, false)?;
        query.source = Retained::Array;
        Ok(query)
    }
    pub(super) fn cached(&self) -> &[Value] {
        &self.values
    }
    pub(super) fn take_values(&mut self) -> Vec<Value> {
        std::mem::take(&mut self.values)
    }
    pub(super) fn add_scratch(&mut self, bytes: u64) {
        self.scratch = self.scratch.saturating_add(bytes);
    }
    pub(super) fn check(&self) -> CallResult<()> {
        self.rows.state.check_source(self.revision, self.schema)
    }
    fn live(&self, ex: &mut Execution) -> CallResult<u64> {
        let detached = self
            .rows
            .parent
            .as_ref()
            .filter(|p| !p.active.get())
            .map_or(0, |p| p.bytes.get());
        let mut live = self
            .rows
            .state
            .live(ex)?
            .saturating_add(detached)
            .saturating_add(
                (std::mem::size_of::<TreeRows>() + 2 * std::mem::size_of::<usize>()) as u64,
            )
            .saturating_add(argument_bytes(ex, self.args))
            .saturating_add(self.scratch);
        for value in &self.values {
            live = live.saturating_add(value_bytes(value));
        }
        if let Some(value) = self.values.first() {
            live = live.saturating_add(match self.source {
                Retained::None => 0,
                Retained::Structure => Structure::of(value)
                    .filter(|s| !s.is_fixed())
                    .ok_or_else(|| type_mismatch(1))?
                    .retained_bytes(ex)?,
                Retained::Array => Array::of(value)
                    .filter(|a| !a.is_fixed())
                    .ok_or_else(|| type_mismatch(1))?
                    .retained_bytes(ex)?,
            });
        }
        Ok(live)
    }
    pub(super) fn reserve(&self, ex: &mut Execution, extra: u64) -> CallResult<u64> {
        let live = self.live(ex)?.saturating_add(extra);
        ex.reserve(live)?;
        Ok(live)
    }
    pub(super) fn extract(&mut self, ex: &mut Execution, index: usize) -> CallResult<()> {
        let live = self.reserve(ex, 0)?;
        let value = arg_value_cached(ex, self.args.get(index), live)?;
        self.values.push(value);
        self.check()?;
        self.reserve(ex, 0)?;
        Ok(())
    }
    fn recursive(&self, ex: &mut Execution, index: usize) -> CallResult<bool> {
        let value = &self.values[index];
        let recursive = if matches!(value, Value::Undefined) {
            false
        } else {
            if let Value::String(s) = value {
                ex.charge(s.len() as u64 / 64 + 1)?;
            }
            bsl_vm::ops::to_bool(ex, value)?
        };
        self.check()?;
        self.reserve(ex, 0)?;
        Ok(recursive)
    }
    pub(super) fn selected_column(
        &self,
        ex: &mut Execution,
        key: &Value,
    ) -> CallResult<Rc<ColumnRecord>> {
        let live = self.reserve(ex, 0)?;
        if matches!(key, Value::String(_) | Value::Number(_)) {
            return column(&self.rows.state, ex, key, live);
        }
        let text = crate::present::present_for_collection(ex, key, live)?;
        self.check()?;
        let units = text.len() as u64;
        ex.charge(units / 32 + 1)?;
        self.reserve(ex, units.saturating_mul(32).saturating_add(2048))?;
        Err(HostError::key(msg::PROP_NOT_FOUND)
            .with_arg(text.to_string_lossy())
            .into())
    }
    fn walker(&mut self, ex: &mut Execution, recursive: bool) -> CallResult<Walk> {
        let bound = self.rows.state.arena.borrow().len();
        ex.charge(bound as u64)?;
        self.scratch = self
            .scratch
            .saturating_add((bound as u64).saturating_mul(std::mem::size_of::<usize>() as u64));
        self.reserve(ex, 0)?;
        let mut pending = vec_with_capacity(ex, bound as u64)?;
        self.rows.with_ids(|ids| {
            ex.charge(ids.len() as u64)?;
            pending.extend(ids.iter().rev().copied());
            Ok::<_, bsl_vm::Exception>(())
        })?;
        Ok(Walk {
            pending,
            recursive,
            bound,
        })
    }
    pub(super) fn cell(&self, ex: &mut Execution, row: &Node, column: u32) -> CallResult<Value> {
        let live = self.reserve(ex, 0)?;
        let cells = row.cells.borrow();
        ex.charge(cells.0.len() as u64)?;
        match cells.0.iter().find(|(id, _)| *id == column) {
            Some((_, value)) => clone_cell(ex, value, live.saturating_add(value_bytes(value))),
            None => Ok(Value::Undefined),
        }
    }
    fn equal(&self, ex: &mut Execution, value: &Value, needle: &Value) -> CallResult<bool> {
        let live = self.reserve(ex, value_bytes(value).saturating_add(value_bytes(needle)))?;
        guard_equal(ex, value, needle, live)?;
        let equal = same_value(value, needle);
        self.check()?;
        self.reserve(ex, value_bytes(value).saturating_add(value_bytes(needle)))?;
        Ok(equal)
    }
}
struct Walk {
    pending: Vec<usize>,
    recursive: bool,
    bound: usize,
}
impl Walk {
    fn next(&mut self, ex: &mut Execution, state: &TreeState) -> CallResult<Option<Rc<Node>>> {
        let Some(id) = self.pending.pop() else {
            return Ok(None);
        };
        ex.charge(1)?;
        let node = state.node(id).ok_or_else(changed)?;
        if self.recursive {
            let children = node.children.borrow();
            ex.charge(children.len() as u64)?;
            if self.pending.len().saturating_add(children.len()) > self.pending.capacity() {
                return Err(changed().into());
            }
            self.pending.extend(children.iter().rev().copied());
        }
        Ok(Some(node))
    }
}
fn column(
    state: &TreeState,
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
    let _guard = crate::present::CollectionPresentationGuard::enter(live);
    state.columns.cell_column(ex, key)
}
fn columns(
    state: &TreeState,
    ex: &mut Execution,
    selector: &Value,
    live: u64,
) -> CallResult<Vec<Rc<ColumnRecord>>> {
    let name = match selector {
        Value::Undefined => return state.columns.records(ex, live),
        Value::String(s) if s.is_empty() => return state.columns.records(ex, live),
        Value::String(s) => s,
        _ => return Err(type_mismatch(2).into()),
    };
    ex.charge(name.len() as u64 / 32 + 1)?;
    let count = name
        .units()
        .iter()
        .filter(|u| **u == u16::from(b','))
        .count() as u64
        + 1;
    let live =
        live.saturating_add(count.saturating_mul(std::mem::size_of::<Rc<ColumnRecord>>() as u64));
    ex.reserve(live)?;
    let mut result = vec_with_capacity(ex, count)?;
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
        result.push(column(
            state,
            ex,
            &Value::String(Str::from_units(units)),
            live,
        )?);
    }
    Ok(result)
}
impl TreeRows {
    pub(super) fn find(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let mut q = Query::new(self, ex, args, 3, false)?;
        for i in 0..3 {
            q.extract(ex, i)?;
        }
        let recursive = q.recursive(ex, 2)?;
        let live = q.reserve(ex, 0)?;
        let columns = columns(&self.state, ex, &q.values[1], live)?;
        q.scratch = q.scratch.saturating_add(
            (columns.capacity() as u64)
                .saturating_mul(std::mem::size_of::<Rc<ColumnRecord>>() as u64),
        );
        q.check()?;
        let mut walk = q.walker(ex, recursive)?;
        while let Some(node) = walk.next(ex, &self.state)? {
            for column in &columns {
                let value = q.cell(ex, &node, column.id)?;
                if q.equal(ex, &value, &q.values[0])? {
                    q.reserve(ex, HANDLE_BYTES.saturating_add(value_bytes(&value)))?;
                    return self.state.row(ex, node);
                }
            }
        }
        q.check()?;
        q.reserve(ex, 0)?;
        Ok(Value::Undefined)
    }
    pub(super) fn find_rows(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let mut q = Query::new(self, ex, args, 2, true)?;
        q.extract(ex, 0)?;
        let source = Structure::of(&q.values[0])
            .filter(|s| !s.is_fixed())
            .ok_or_else(|| type_mismatch(1))?;
        let base = q.reserve(ex, 0)?;
        let (pairs, snapshot_live) = source.guarded_pairs(ex, base)?;
        q.scratch = q.scratch.saturating_add(snapshot_live.saturating_sub(base));
        q.extract(ex, 1)?;
        let recursive = q.recursive(ex, 1)?;
        q.scratch = q.scratch.saturating_add(
            (pairs.len() as u64).saturating_mul(std::mem::size_of::<Rc<ColumnRecord>>() as u64),
        );
        q.reserve(ex, 0)?;
        let mut columns = vec_with_capacity(ex, pairs.len() as u64)?;
        for (key, _) in &pairs {
            let live = q.reserve(ex, 0)?;
            columns.push(column(&self.state, ex, &Value::String(key.clone()), live)?);
        }
        q.check()?;
        let mut walk = q.walker(ex, recursive)?;
        q.scratch = q
            .scratch
            .saturating_add(
                (walk.bound as u64)
                    .saturating_mul(std::mem::size_of::<Value>() as u64 + HANDLE_BYTES),
            )
            .saturating_add(ARRAY_BYTES);
        q.reserve(ex, 0)?;
        let mut result = vec_with_capacity(ex, walk.bound as u64)?;
        while let Some(node) = walk.next(ex, &self.state)? {
            let mut matches = true;
            for (column, (_, needle)) in columns.iter().zip(&pairs) {
                let value = q.cell(ex, &node, column.id)?;
                if !q.equal(ex, &value, needle)? {
                    matches = false;
                    break;
                }
            }
            if matches {
                q.reserve(ex, 0)?;
                result.push(self.state.row(ex, node)?);
            }
        }
        q.check()?;
        q.reserve(ex, 0)?;
        Ok(Array::new_value(result, false))
    }
    pub(super) fn total(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let mut q = Query::new(self, ex, args, 2, false)?;
        q.extract(ex, 0)?;
        q.extract(ex, 1)?;
        let recursive = q.recursive(ex, 1)?;
        let column = q.selected_column(ex, &q.values[0])?;
        q.check()?;
        let mut walk = q.walker(ex, recursive)?;
        let mut sum = Number::from_i64(0);
        while let Some(node) = walk.next(ex, &self.state)? {
            let live = q.reserve(ex, (sum.backing_capacity_limbs() as u64).saturating_mul(4))?;
            let cells = node.cells.borrow();
            ex.charge(cells.0.len() as u64)?;
            if let Some((_, Value::Number(n))) = cells.0.iter().find(|(id, _)| *id == column.id) {
                let limbs = sum
                    .size_hint_limbs()
                    .saturating_add(n.size_hint_limbs())
                    .saturating_add(2);
                ex.charge(limbs / 64 + 1)?;
                ex.reserve(
                    live.saturating_add((n.backing_capacity_limbs() as u64).saturating_mul(4))
                        .saturating_add(limbs.saturating_mul(32)),
                )?;
                sum = sum.add(n);
            }
        }
        q.check()?;
        q.reserve(ex, (sum.backing_capacity_limbs() as u64).saturating_mul(4))?;
        Ok(Value::Number(sum))
    }
}
