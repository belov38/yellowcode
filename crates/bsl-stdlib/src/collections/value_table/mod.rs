mod accounting;
mod session;
pub(super) use accounting::object as retained_object;
mod bridge;
mod column_data;
mod copy;
mod group;
mod indexes;
pub(super) mod row;
mod search;
mod sort;
use super::{
    columns::{ColumnRecord, Columns, PreparedSchemaChange, SchemaChange, SchemaRows},
    creation_number, method_name, not_implemented, position, property_name, release,
    reserve_values, type_name,
    value_list::{arg_i64_cached, arg_value_cached, guarded_cast, scalar_clone_bytes},
    vec_with_capacity,
};
use crate::{
    bind::{msg, type_mismatch, value_mismatch},
    typedesc::{TypeDescription, idx},
};
use bsl_prim::Str;
use bsl_vm::{Arg, CallResult, Execution, HostError, HostObject, MemberId, TypeId, Value};
use row::{Cells, RowRecord, TableRow};
use std::{
    any::Any,
    cell::{Cell, RefCell},
    rc::Rc,
};

pub struct ValueTable {
    state: Rc<TableState>,
}
struct TableState {
    columns: Rc<Columns>,
    indexes: Rc<indexes::Indexes>,
    rows: RefCell<Vec<Rc<RowRecord>>>,
    revision: Cell<u64>,
    next_id: Cell<u64>,
    records_bytes: Cell<u64>,
    producer_capacity_extra: Cell<Option<(u64, u64, u64)>>,
    hash: i32,
}
fn changed() -> HostError {
    HostError::not_implemented("ТаблицаЗначений", "Рекурсивная операция")
}
fn value_bytes(value: &Value) -> u64 {
    match value {
        Value::String(s) => (s.len() as u64).saturating_mul(2),
        _ => crate::values::binary_data_bytes(value)
            .map_or_else(|| scalar_clone_bytes(value), |bytes| bytes.len() as u64),
    }
}
fn clone_cell(ex: &mut Execution, value: &Value, live: u64) -> CallResult<Value> {
    ex.reserve(live.saturating_add(scalar_clone_bytes(value)))?;
    if let Value::Number(n) = value {
        ex.charge(n.size_hint_limbs() / 64)?;
    }
    Ok(value.clone())
}
fn default_cell(ex: &mut Execution, column: &ColumnRecord, live: u64) -> CallResult<Value> {
    let td = column.value_type();
    adjust_cell(ex, &td, Value::Undefined, live)
}
fn adjust_cell(ex: &mut Execution, td: &Value, value: Value, live: u64) -> CallResult<Value> {
    let Some(td) = td
        .as_object()
        .and_then(|o| o.as_any().downcast_ref::<TypeDescription>())
    else {
        return Err(type_mismatch(1).into());
    };
    Ok(guarded_cast(ex, td, value, live)?)
}
impl TableState {
    fn visit_values(
        &self,
        visit: &mut super::retire::Visitor<'_>,
    ) -> Result<(), bsl_vm::Exception> {
        for row in self.rows.borrow().iter() {
            for (_, value) in &row.cells.borrow().0 {
                visit(value)?;
            }
        }
        Ok(())
    }
    fn retire_into(&self, own: Option<&Rc<RowRecord>>, out: &mut Vec<Value>) {
        let rows = std::mem::take(&mut *self.rows.borrow_mut());
        self.records_bytes.set(0);
        for record in rows {
            record.member.set(false);
            let owners = Rc::strong_count(&record);
            if owners == 1 || (owners == 2 && own.is_some_and(|o| Rc::ptr_eq(o, &record))) {
                record.cells.borrow_mut().retire_into(out);
            }
        }
    }

    fn len(&self) -> usize {
        self.rows.borrow().len()
    }
    fn bump(&self) {
        self.revision.set(self.revision.get().wrapping_add(1));
    }
    fn check_revision(&self, revision: u64, schema: u64) -> CallResult<()> {
        self.check(revision)?;
        self.columns.check(schema)
    }
    fn bytes(&self) -> u64 {
        (std::mem::size_of::<Self>()
            + std::mem::size_of::<ValueTable>()
            + 4 * std::mem::size_of::<usize>()) as u64
            + (self.rows.borrow().capacity() as u64)
                .saturating_mul(std::mem::size_of::<Rc<RowRecord>>() as u64)
            + self.records_bytes.get()
    }
    fn live(&self, ex: &mut Execution) -> CallResult<u64> {
        Ok(self.bytes().saturating_add(self.columns.owned_bytes(ex)?))
    }
    fn handle(self: &Rc<Self>, ex: &mut Execution, record: Rc<RowRecord>) -> CallResult<Value> {
        ex.reserve((std::mem::size_of::<TableRow>() + 2 * std::mem::size_of::<usize>()) as u64)?;
        Ok(Value::Object(Rc::new(TableRow {
            state: self.clone(),
            record,
        })))
    }
    fn table(self: &Rc<Self>, ex: &mut Execution) -> CallResult<Value> {
        ex.reserve((std::mem::size_of::<ValueTable>() + 2 * std::mem::size_of::<usize>()) as u64)?;
        Ok(Value::Object(Rc::new(ValueTable {
            state: self.clone(),
        })))
    }
    fn at(self: &Rc<Self>, ex: &mut Execution, i: usize) -> CallResult<Value> {
        let record = self
            .rows
            .borrow()
            .get(i)
            .cloned()
            .ok_or_else(|| HostError::key(msg::INDEX_OFFBOUND))?;
        self.handle(ex, record)
    }
    fn numeric(&self, ex: &mut Execution, arg: Option<&Arg>, insert: bool) -> CallResult<usize> {
        let n = arg_i64_cached(ex, arg, 0)?;
        usize::try_from(n)
            .ok()
            .filter(|i| *i < self.len() || (insert && *i == self.len()))
            .ok_or_else(|| HostError::key(msg::RANGE).into())
    }
    fn row_index(&self, ex: &mut Execution, value: &Value) -> CallResult<Option<usize>> {
        let row = value
            .as_object()
            .and_then(|o| o.as_any().downcast_ref::<TableRow>())
            .ok_or_else(|| type_mismatch(1))?;
        if !std::ptr::eq(self, &*row.state) || !row.record.member.get() {
            return Ok(None);
        }
        ex.charge(self.len() as u64)?;
        Ok(self
            .rows
            .borrow()
            .iter()
            .position(|r| Rc::ptr_eq(r, &row.record)))
    }
    fn selector(&self, ex: &mut Execution, value: &Value) -> CallResult<usize> {
        if let Value::Number(n) = value {
            return position(n)
                .filter(|i| *i < self.len())
                .ok_or_else(|| HostError::key(msg::RANGE).into());
        }
        self.row_index(ex, value)?
            .ok_or_else(|| value_mismatch(1).into())
    }
    fn add(
        self: &Rc<Self>,
        ex: &mut Execution,
        args: &mut [Arg],
        insert: bool,
    ) -> CallResult<Value> {
        let rev = self.revision.get();
        let schema = self.columns.revision();
        let i = if insert {
            self.numeric(ex, args.first(), true)?
        } else {
            self.len()
        };
        self.insert_row(ex, i, rev, schema, None, None)
    }
    fn insert_row(
        self: &Rc<Self>,
        ex: &mut Execution,
        i: usize,
        rev: u64,
        schema: u64,
        source: Option<&[Value]>,
        producer_extra: Option<u64>,
    ) -> CallResult<Value> {
        let id = self.next_id.get();
        let next = id.checked_add(1).ok_or_else(|| ex.out_of_memory())?;
        let mut base = self.live(ex)?.saturating_add(producer_extra.unwrap_or(0));
        if let Some(source) = source {
            ex.charge(source.len() as u64)?;
            base = base.saturating_add(
                (source.len() as u64).saturating_mul(std::mem::size_of::<Value>() as u64),
            );
            ex.reserve(base)?;
            for value in source {
                base = base.saturating_add(value_bytes(value));
                ex.reserve(base)?;
            }
        }
        let columns = self.columns.records(ex, base)?;
        let n = columns.len();
        reserve_values(ex, self.len() as u64 + 1)?;
        let mut live = base
            .saturating_add(
                (n as u64).saturating_mul(std::mem::size_of::<Rc<ColumnRecord>>() as u64),
            )
            .saturating_add((n as u64).saturating_mul(std::mem::size_of::<(u32, Value)>() as u64))
            .saturating_add(
                (std::mem::size_of::<RowRecord>()
                    + std::mem::size_of::<TableRow>()
                    + 4 * std::mem::size_of::<usize>()) as u64,
            )
            .saturating_add(
                ((self.len() + 1) as u64)
                    .saturating_mul(std::mem::size_of::<Rc<RowRecord>>() as u64),
            );
        ex.reserve(live)?;
        ex.charge(n as u64 + (self.len() - i) as u64 + 1)?;
        let mut cells = Cells(vec_with_capacity(ex, n as u64)?);
        for (index, column) in columns.iter().enumerate() {
            let value = if let Some(input) = source.and_then(|values| values.get(index)) {
                let input = clone_cell(ex, input, live)?;
                adjust_cell(ex, &column.value_type(), input, live)?
            } else {
                default_cell(ex, column, live)?
            };
            live = live.saturating_add(value_bytes(&value));
            if producer_extra.is_some() {
                live = live.saturating_add(bridge::capacity_extra(&value));
            }
            ex.reserve(live)?;
            cells.0.push((column.id, value));
            self.check_revision(rev, schema)?;
        }
        let record = Rc::new(RowRecord {
            id,
            hash: creation_number(),
            member: Cell::new(true),
            cells: RefCell::new(cells),
            bytes: Cell::new(0),
        });
        record.bytes.set(record.calculate_bytes());
        let handle = self.handle(ex, record.clone())?;
        self.check_revision(rev, schema)?;
        let mut rows = self.rows.borrow_mut();
        rows.try_reserve_exact(1).map_err(|_| ex.out_of_memory())?;
        rows.insert(i, record.clone());
        self.records_bytes
            .set(self.records_bytes.get().saturating_add(record.bytes.get()));
        self.next_id.set(next);
        self.bump();
        Ok(handle)
    }
    fn detach_all(&self) {
        let rows = std::mem::take(&mut *self.rows.borrow_mut());
        self.records_bytes.set(0);
        self.bump();
        for row in &rows {
            row.member.set(false);
        }
        for row in rows {
            if Rc::strong_count(&row) == 1 {
                release(row.take_values());
            }
        }
    }
    fn take_values(&self, own: Option<&Rc<RowRecord>>) -> Vec<Value> {
        let rows = std::mem::take(&mut *self.rows.borrow_mut());
        let mut out = Vec::new();
        self.records_bytes.set(0);
        for record in rows {
            record.member.set(false);
            let owners = Rc::strong_count(&record);
            if owners == 1 || (owners == 2 && own.is_some_and(|o| Rc::ptr_eq(o, &record))) {
                out.extend(record.take_values());
            }
        }
        out
    }
}
struct PreparedRows {
    state: Rc<TableState>,
    revision: u64,
    schema: u64,
    cells: Vec<(Rc<RowRecord>, Cells)>,
    clear: bool,
    retired: Vec<Value>,
}
impl PreparedSchemaChange for PreparedRows {
    fn check(&self) -> CallResult<()> {
        self.state.check_revision(self.revision, self.schema)
    }
    fn commit(mut self: Box<Self>) {
        if self.clear {
            self.state.retire_into(None, &mut self.retired);
            self.state.bump();
        } else {
            let mut bytes = self.state.records_bytes.get();
            for (record, cells) in &mut self.cells {
                let before = record.bytes.get();
                std::mem::swap(&mut *record.cells.borrow_mut(), cells);
                record.bytes.set(record.calculate_bytes());
                bytes = bytes
                    .saturating_sub(before)
                    .saturating_add(record.bytes.get());
            }
            self.state.records_bytes.set(bytes);
            self.state.bump();
            for (_, cells) in &mut self.cells {
                cells.retire_into(&mut self.retired);
            }
        }
        super::retire::run(std::mem::take(&mut self.retired));
    }
}
impl SchemaRows for TableState {
    fn revision(&self) -> u64 {
        self.revision.get()
    }
    fn check(&self, revision: u64) -> CallResult<()> {
        if self.revision.get() != revision {
            return Err(changed().into());
        }
        Ok(())
    }
    fn owned_bytes(&self) -> u64 {
        self.bytes()
    }
    fn prepare(
        self: Rc<Self>,
        ex: &mut Execution,
        change: SchemaChange,
        live: u64,
    ) -> CallResult<Box<dyn PreparedSchemaChange>> {
        let revision = self.revision.get();
        let schema = self.columns.revision();
        let mut live = live
            .saturating_add(self.bytes())
            .saturating_add(std::mem::size_of::<PreparedRows>() as u64)
            .saturating_add(
                (self.len() as u64)
                    .saturating_mul(std::mem::size_of::<(Rc<RowRecord>, Cells)>() as u64),
            );
        ex.reserve(live)?;
        ex.charge(self.len() as u64)?;
        let mut prepared = PreparedRows {
            state: self.clone(),
            revision,
            schema,
            cells: vec_with_capacity(ex, self.len() as u64)?,
            clear: matches!(change, SchemaChange::Clear),
            retired: Vec::new(),
        };
        if !prepared.clear {
            live = live.saturating_add(
                (self.len() as u64).saturating_mul(std::mem::size_of::<Rc<RowRecord>>() as u64),
            );
            ex.reserve(live)?;
            let mut rows = vec_with_capacity(ex, self.len() as u64)?;
            rows.extend(self.rows.borrow().iter().cloned());
            for record in rows {
                let n = record.cells.borrow().0.len();
                let cap = n + usize::from(matches!(change, SchemaChange::Insert { .. }));
                live =
                    live.saturating_add(
                        (cap as u64).saturating_mul(std::mem::size_of::<(u32, Value)>() as u64),
                    );
                ex.reserve(live)?;
                ex.charge(n as u64 + 1)?;
                let mut cells = Cells(vec_with_capacity(ex, cap as u64)?);
                for (id, value) in &record.cells.borrow().0 {
                    if matches!(change, SchemaChange::Delete { id: deleted } if *id == deleted) {
                        continue;
                    }
                    live = live.saturating_add(scalar_clone_bytes(value));
                    cells.0.push((*id, clone_cell(ex, value, live)?));
                }
                if let SchemaChange::Insert { id, ref value_type } = change {
                    let value = adjust_cell(ex, value_type, Value::Undefined, live)?;
                    live = live.saturating_add(value_bytes(&value));
                    ex.reserve(live)?;
                    cells.0.push((id, value));
                }
                prepared.cells.push((record, cells));
                self.check_revision(revision, schema)?;
            }
        }
        prepared.retired = super::retire::prepare(ex, live, |visit| self.visit_values(visit))?;
        Ok(Box::new(prepared))
    }
}
impl Drop for TableState {
    fn drop(&mut self) {
        release(self.take_values(None));
    }
}
impl ValueTable {
    pub(super) fn retirement_work(&self) -> u64 {
        (self.state.len() as u64).saturating_add(self.state.columns.len() as u64)
    }
    pub(super) fn visit_values(
        &self,
        visit: &mut super::retire::Visitor<'_>,
    ) -> Result<(), bsl_vm::Exception> {
        self.state.visit_values(visit)
    }
    pub(super) fn retire_into(&self, out: &mut Vec<Value>) {
        if Rc::strong_count(&self.state) == 1 {
            self.state.retire_into(None, out);
        }
    }

    pub(crate) fn take_values(&self) -> Vec<Value> {
        if Rc::strong_count(&self.state) == 1 {
            self.state.take_values(None)
        } else {
            Vec::new()
        }
    }
}
impl Drop for ValueTable {
    fn drop(&mut self) {
        release(self.take_values());
    }
}
pub(super) fn ctor(ex: &mut Execution, _: &mut [Arg]) -> CallResult<Value> {
    ex.reserve(
        (std::mem::size_of::<TableState>()
            + std::mem::size_of::<ValueTable>()
            + 4 * std::mem::size_of::<usize>()) as u64
            + Columns::allocation_bytes()
            + indexes::Indexes::allocation_bytes(),
    )?;
    let columns = Columns::new(
        ex,
        idx("ValueTableColumnCollection"),
        idx("ValueTableColumn"),
    )?;
    let live = (std::mem::size_of::<TableState>()
        + std::mem::size_of::<ValueTable>()
        + 4 * std::mem::size_of::<usize>()) as u64
        + Columns::allocation_bytes();
    let indexes = indexes::Indexes::new(ex, columns.clone(), live)?;
    let state = Rc::new(TableState {
        columns,
        indexes,
        rows: RefCell::new(Vec::new()),
        revision: Cell::new(0),
        next_id: Cell::new(0),
        records_bytes: Cell::new(0),
        producer_capacity_extra: Cell::new(None),
        hash: creation_number(),
    });
    let observer: Rc<dyn SchemaRows> = state.clone();
    state.columns.attach_rows(Rc::downgrade(&observer));
    state.table(ex)
}
impl HostObject for ValueTable {
    fn to_shared(&self) -> Option<bsl_vm::SharedValue> {
        bsl_vm::SharedValue::from_object(self)
    }

    fn session_edges(&self, visit: &mut dyn bsl_vm::SessionEdges) -> bool {
        visit.backing(self.state.clone())
    }
    fn session_clear(&self) {}
    fn session_transfer_supported(&self) -> bool {
        true
    }
    fn session_transfer(
        &self,
        out: &mut dyn bsl_vm::SessionExport,
    ) -> Result<Option<Box<dyn bsl_vm::SessionObject>>, bsl_vm::Exception> {
        session::capture(&self.state, None, out).map(Some)
    }
    fn retained_memory(
        &self,
        visit: &mut dyn bsl_vm::RetainedMemory,
    ) -> Result<bool, bsl_vm::Exception> {
        crate::collections::accounting::project(self, visit)
    }
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(idx("ValueTable"))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        match property_name(idx("ValueTable"), m) {
            Some("Columns") => Ok(Value::Object(self.state.columns.clone())),
            Some("Indexes") => Ok(Value::Object(self.state.indexes.clone())),
            _ => Err(not_implemented(idx("ValueTable"), m).into()),
        }
    }
    fn set(&self, _: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        Err(not_implemented(idx("ValueTable"), m).into())
    }
    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        let state = &self.state;
        match method_name(idx("ValueTable"), m) {
            Some("Count") => return Ok(Value::from(state.len() as i64)),
            Some("Sort") => return state.sort_rows(ex, args),
            Some("GroupBy") => return state.group(ex, args),
            Some("Total") => return state.total(ex, args),
            Some("Find") => return state.find(ex, args),
            Some("FindRows") => return state.find_rows(ex, args),
            Some("Copy") => return state.copy_table(ex, args, false),
            Some("CopyColumns") => return state.copy_table(ex, args, true),
            Some("UnloadColumn") => return state.unload_column(ex, args),
            Some("LoadColumn") => return state.load_column(ex, args),
            Some("FillValues") => return state.fill_values(ex, args),
            Some("Add") => return state.add(ex, args, false),
            Some("Insert") => return state.add(ex, args, true),
            Some("Get") => {
                let i = state.numeric(ex, args.first(), false)?;
                return state.at(ex, i);
            }
            Some("IndexOf") => {
                let value = arg_value_cached(ex, args.first(), 0)?;
                return Ok(Value::from(
                    state.row_index(ex, &value)?.map_or(-1, |i| i as i64),
                ));
            }
            Some("Clear") => {
                ex.charge(state.len() as u64)?;
                state.detach_all();
            }
            Some("Delete") | Some("Move") => {
                let rev = state.revision.get();
                let schema = state.columns.revision();
                let value = arg_value_cached(ex, args.first(), 0)?;
                let offset = if method_name(idx("ValueTable"), m) == Some("Move") {
                    Some(arg_i64_cached(ex, args.get(1), scalar_clone_bytes(&value))?)
                } else {
                    None
                };
                state.check_revision(rev, schema)?;
                let i = state.selector(ex, &value)?;
                let dest = offset
                    .map(|n| {
                        (i as i64)
                            .checked_add(n)
                            .and_then(|n| usize::try_from(n).ok())
                            .filter(|n| *n < state.len())
                            .ok_or_else(|| value_mismatch(2))
                    })
                    .transpose()?;
                ex.charge(state.len() as u64)?;
                let record = {
                    let mut rows = state.rows.borrow_mut();
                    let record = rows.remove(i);
                    if let Some(dest) = dest {
                        rows.insert(dest, record.clone());
                    }
                    record
                };
                if dest.is_none() {
                    record.member.set(false);
                    state
                        .records_bytes
                        .set(state.records_bytes.get().saturating_sub(record.bytes.get()));
                }
                state.bump();
                if dest.is_none() && Rc::strong_count(&record) == 1 {
                    release(record.take_values());
                }
            }
            _ => return Err(not_implemented(idx("ValueTable"), m).into()),
        }
        Ok(Value::Undefined)
    }
    fn index_get(&self, ex: &mut Execution, key: &Value) -> CallResult<Value> {
        let Value::Number(n) = key else {
            return Err(super::columns::missing_field(ex, key)?.into());
        };
        let i = position(n).ok_or_else(|| HostError::key(msg::INDEX_OFFBOUND))?;
        self.state.at(ex, i)
    }
    fn index_set(&self, _: &mut Execution, _: &Value, _: Value) -> CallResult<()> {
        Err(HostError::key(msg::INDEXED_READONLY).into())
    }
    fn iter_len(&self, _: &mut Execution) -> Option<usize> {
        Some(self.state.len())
    }
    fn iter_get(&self, ex: &mut Execution, i: usize) -> CallResult<Value> {
        self.state.at(ex, i)
    }
    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<Str>> {
        Ok(Some(type_name(ex, idx("ValueTable"))))
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
