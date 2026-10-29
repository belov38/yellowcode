mod bridge;
mod column_data;
mod copy;
mod row;
mod rows;
mod schema;
mod search;
use super::{
    columns::{ColumnRecord, Columns, PreparedSchemaChange, SchemaChange, SchemaRows},
    creation_number, method_name, not_implemented, position, property_name, release, type_name,
    value_list::{arg_i64_cached, arg_value_cached, guarded_cast, scalar_clone_bytes},
    vec_with_capacity,
};
use crate::{
    bind::{msg, type_mismatch, value_mismatch},
    typedesc::{TypeDescription, idx},
};
use bsl_prim::Str;
use bsl_vm::{Arg, CallResult, Execution, HostError, HostObject, MemberId, TypeId, Value};
use row::TreeRow;
use rows::TreeRows;
use std::{
    any::Any,
    cell::{Cell, RefCell},
    rc::Rc,
};

pub(crate) struct ValueTree {
    state: Rc<TreeState>,
}
struct Cells(Vec<(u32, Value)>);
impl Cells {
    fn drain_into(&mut self, out: &mut Vec<Value>, prepared: bool) {
        for (_, value) in self.0.drain(..) {
            push(out, value, prepared);
        }
    }
}
impl Drop for Cells {
    fn drop(&mut self) {
        let mut out = Vec::new();
        self.drain_into(&mut out, false);
        release(out);
    }
}
struct Node {
    id: usize,
    parent: Option<usize>,
    level: i64,
    active: Cell<bool>,
    children: RefCell<Vec<usize>>,
    cells: RefCell<Cells>,
    bytes: Cell<u64>,
    hash: i32,
    rows_hash: i32,
}
impl Node {
    fn writable(&self) -> CallResult<()> {
        if self.active.get() {
            Ok(())
        } else {
            Err(HostError::key("core85:IDS_ERR_DELETED_ROW").into())
        }
    }
    fn bytes(&self) -> u64 {
        let cells = self.cells.borrow();
        (std::mem::size_of::<Self>() + 2 * std::mem::size_of::<usize>()) as u64
            + (self.children.borrow().capacity() as u64)
                .saturating_mul(std::mem::size_of::<usize>() as u64)
            + (cells.0.capacity() as u64).saturating_mul(std::mem::size_of::<(u32, Value)>() as u64)
            + cells
                .0
                .iter()
                .fold(0u64, |n, (_, v)| n.saturating_add(value_bytes(v)))
    }
}
struct TreeState {
    columns: Rc<Columns>,
    arena: RefCell<Vec<Option<Rc<Node>>>>,
    roots: RefCell<Vec<usize>>,
    revision: Cell<u64>,
    records_bytes: Cell<u64>,
    hash: i32,
    rows_hash: i32,
    producer_text_slack: Cell<Option<(u64, u64, u64)>>,
}
fn changed() -> HostError {
    HostError::not_implemented("ДеревоЗначений", "Рекурсивная операция")
}
fn value_bytes(v: &Value) -> u64 {
    match v {
        Value::Number(n) => (n.backing_capacity_limbs() as u64).saturating_mul(4),
        Value::String(s) => (s.len() as u64).saturating_mul(2),
        _ => crate::values::binary_data_bytes(v).map_or(0, |b| b.len() as u64),
    }
}
fn argument_bytes(ex: &Execution, args: &[Arg]) -> u64 {
    args.iter().fold(0u64, |n, arg| {
        n.saturating_add(match arg {
            Arg::Val(value) => value_bytes(value),
            Arg::Place(place) => place.numeric_backing_bytes(ex),
            Arg::Omitted => 0,
        })
    })
}
fn clone_cell(ex: &mut Execution, v: &Value, live: u64) -> CallResult<Value> {
    ex.reserve(live.saturating_add(scalar_clone_bytes(v)))?;
    if let Value::Number(n) = v {
        ex.charge(n.size_hint_limbs() / 64 + 1)?;
    }
    Ok(v.clone())
}
fn adjust(ex: &mut Execution, td: &Value, v: Value, live: u64) -> CallResult<Value> {
    let td = td
        .as_object()
        .and_then(|o| o.as_any().downcast_ref::<TypeDescription>())
        .ok_or_else(|| type_mismatch(1))?;
    Ok(guarded_cast(ex, td, v, live)?)
}
fn push(out: &mut Vec<Value>, value: Value, prepared: bool) {
    if prepared {
        super::retire::push(out, value);
    } else {
        out.push(value);
    }
}
impl TreeState {
    fn bump(&self) {
        self.revision.set(self.revision.get().wrapping_add(1));
    }
    fn check_source(&self, revision: u64, schema: u64) -> CallResult<()> {
        self.check(revision)?;
        self.columns.check(schema)
    }
    fn bytes(&self) -> u64 {
        (std::mem::size_of::<Self>() + 4 * std::mem::size_of::<usize>()) as u64
            + (self.arena.borrow().capacity() as u64)
                .saturating_mul(std::mem::size_of::<Option<Rc<Node>>>() as u64)
            + (self.roots.borrow().capacity() as u64)
                .saturating_mul(std::mem::size_of::<usize>() as u64)
            + self.records_bytes.get()
    }
    fn live(&self, ex: &mut Execution) -> CallResult<u64> {
        Ok(self.bytes().saturating_add(self.columns.owned_bytes(ex)?))
    }
    fn node(&self, id: usize) -> Option<Rc<Node>> {
        self.arena.borrow().get(id)?.clone()
    }
    fn update_bytes(&self, node: &Node) {
        let before = node.bytes.replace(node.bytes());
        self.records_bytes.set(
            self.records_bytes
                .get()
                .saturating_sub(before)
                .saturating_add(node.bytes.get()),
        );
    }
    fn tree(self: &Rc<Self>, ex: &mut Execution) -> CallResult<Value> {
        ex.reserve((std::mem::size_of::<ValueTree>() + 2 * std::mem::size_of::<usize>()) as u64)?;
        Ok(Value::Object(Rc::new(ValueTree {
            state: self.clone(),
        })))
    }
    fn row(self: &Rc<Self>, ex: &mut Execution, record: Rc<Node>) -> CallResult<Value> {
        ex.reserve((std::mem::size_of::<TreeRow>() + 2 * std::mem::size_of::<usize>()) as u64)?;
        Ok(Value::Object(Rc::new(TreeRow {
            state: self.clone(),
            record,
        })))
    }
    fn rows(self: &Rc<Self>, ex: &mut Execution, parent: Option<Rc<Node>>) -> CallResult<Value> {
        ex.reserve((std::mem::size_of::<TreeRows>() + 2 * std::mem::size_of::<usize>()) as u64)?;
        Ok(Value::Object(Rc::new(TreeRows {
            state: self.clone(),
            parent,
        })))
    }
    fn parent_value(
        self: &Rc<Self>,
        ex: &mut Execution,
        parent: Option<usize>,
    ) -> CallResult<Value> {
        match parent.and_then(|id| self.node(id)) {
            Some(node) => self.row(ex, node),
            None => Ok(Value::Undefined),
        }
    }
    fn visit_values(
        &self,
        visit: &mut super::retire::Visitor<'_>,
    ) -> Result<(), bsl_vm::Exception> {
        for node in self.arena.borrow().iter().flatten() {
            for (_, value) in &node.cells.borrow().0 {
                visit(value)?;
            }
        }
        Ok(())
    }
    fn retire_into(&self, own: Option<&Rc<Node>>, out: &mut Vec<Value>, prepared: bool) {
        let arena = std::mem::take(&mut *self.arena.borrow_mut());
        self.roots.borrow_mut().clear();
        self.records_bytes.set(0);
        for node in arena.into_iter().flatten() {
            node.active.set(false);
            node.children.borrow_mut().clear();
            let count = Rc::strong_count(&node);
            if count == 1 || (count == 2 && own.is_some_and(|o| Rc::ptr_eq(o, &node))) {
                node.cells.borrow_mut().drain_into(out, prepared);
            }
        }
    }
}
impl Drop for TreeState {
    fn drop(&mut self) {
        let mut out = Vec::new();
        self.retire_into(None, &mut out, false);
        release(out);
    }
}
fn wrapper(object: &dyn HostObject) -> Option<(&Rc<TreeState>, Option<&Rc<Node>>)> {
    let any = object.as_any();
    if let Some(o) = any.downcast_ref::<ValueTree>() {
        Some((&o.state, None))
    } else if let Some(o) = any.downcast_ref::<TreeRow>() {
        Some((&o.state, Some(&o.record)))
    } else {
        any.downcast_ref::<TreeRows>()
            .map(|o| (&o.state, o.parent.as_ref()))
    }
}
pub(super) fn retirement_work(object: &dyn HostObject) -> Option<u64> {
    let (state, own) = wrapper(object)?;
    let mut work = (state.arena.borrow().len() as u64)
        .saturating_mul(2)
        .saturating_add(state.columns.len() as u64)
        .saturating_add(state.roots.borrow().len() as u64)
        .saturating_add(state.records_bytes.get() / 8);
    if let Some(node) = own {
        work = work.saturating_add(node.bytes.get() / 8);
    }
    Some(work)
}
pub(super) fn visit_values(
    object: &dyn HostObject,
    visit: &mut super::retire::Visitor<'_>,
) -> Option<Result<(), bsl_vm::Exception>> {
    let (state, own) = wrapper(object)?;
    Some((|| {
        state.visit_values(visit)?;
        if let Some(node) = own {
            for (_, v) in &node.cells.borrow().0 {
                visit(v)?;
            }
        }
        Ok(())
    })())
}
pub(super) fn drain(object: &dyn HostObject, out: &mut Vec<Value>, prepared: bool) -> bool {
    let Some((state, own)) = wrapper(object) else {
        return false;
    };
    if Rc::strong_count(state) == 1 {
        state.retire_into(own, out, prepared);
    }
    if let Some(node) = own
        && Rc::strong_count(node) == 1
    {
        node.cells.borrow_mut().drain_into(out, prepared);
    }
    true
}
fn drop_wrapper(object: &dyn HostObject) {
    let mut out = Vec::new();
    drain(object, &mut out, false);
    release(out);
}
impl Drop for ValueTree {
    fn drop(&mut self) {
        drop_wrapper(self);
    }
}
impl Drop for TreeRow {
    fn drop(&mut self) {
        drop_wrapper(self);
    }
}
impl Drop for TreeRows {
    fn drop(&mut self) {
        drop_wrapper(self);
    }
}
pub(super) fn ctor(ex: &mut Execution, _: &mut [Arg]) -> CallResult<Value> {
    ex.reserve(
        (std::mem::size_of::<TreeState>()
            + std::mem::size_of::<ValueTree>()
            + 6 * std::mem::size_of::<usize>()) as u64
            + Columns::allocation_bytes(),
    )?;
    let columns = Columns::new(ex, idx("ValueTreeColumnCollection"), idx("ValueTreeColumn"))?;
    let state = Rc::new(TreeState {
        columns,
        producer_text_slack: Cell::new(None),
        arena: RefCell::new(Vec::new()),
        roots: RefCell::new(Vec::new()),
        revision: Cell::new(0),
        records_bytes: Cell::new(0),
        hash: creation_number(),
        rows_hash: creation_number(),
    });
    let observer: Rc<dyn SchemaRows> = state.clone();
    state.columns.attach_rows(Rc::downgrade(&observer));
    state.tree(ex)
}
impl HostObject for ValueTree {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(idx("ValueTree"))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        match property_name(idx("ValueTree"), m) {
            Some("Columns") => Ok(Value::Object(self.state.columns.clone())),
            Some("Rows") => self.state.rows(ex, None),
            _ => Err(not_implemented(idx("ValueTree"), m).into()),
        }
    }
    fn set(&self, _: &mut Execution, _: MemberId, _: Value) -> CallResult<()> {
        Err(HostError::key(msg::PROP_NOT_WRITABLE).raw().into())
    }
    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        match method_name(idx("ValueTree"), m) {
            Some("Copy") => self.copy(ex, args),
            _ => Err(not_implemented(idx("ValueTree"), m).into()),
        }
    }
    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<Str>> {
        Ok(Some(type_name(ex, idx("ValueTree"))))
    }
    fn boost_hash(&self) -> Option<i32> {
        Some(self.state.hash)
    }
    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        Some(
            other
                .as_any()
                .downcast_ref::<Self>()
                .is_some_and(|o| Rc::ptr_eq(&self.state, &o.state)),
        )
    }
}
