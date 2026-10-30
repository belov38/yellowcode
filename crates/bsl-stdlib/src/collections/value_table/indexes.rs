use super::*;
pub(super) mod session;
use crate::collections::columns::{PreparedSchemaChange, SchemaIndexes};
use bsl_prim::Number;

struct Definition {
    id: u64,
    hash: i32,
    member: Cell<bool>,
    columns: Vec<Rc<ColumnRecord>>,
}
struct IndexState {
    columns: Rc<Columns>,
    entries: RefCell<Vec<Rc<Definition>>>,
    revision: Cell<u64>,
    next_id: Cell<u64>,
    bytes: Cell<u64>,
    hash: i32,
}
pub(super) struct Indexes {
    state: Rc<IndexState>,
}
struct CollectionIndex {
    state: Rc<IndexState>,
    record: Rc<Definition>,
}
fn vector<T>(n: usize) -> u64 {
    (n as u64).saturating_mul(std::mem::size_of::<T>() as u64)
}
fn record_bytes(record: &Definition) -> u64 {
    (std::mem::size_of::<Definition>() + 2 * std::mem::size_of::<usize>()) as u64
        + vector::<Rc<ColumnRecord>>(record.columns.capacity())
}
fn payload(value: &Value) -> u64 {
    match value {
        Value::Number(n) => (n.backing_capacity_limbs() as u64).saturating_mul(4),
        _ => value_bytes(value),
    }
}
fn number_work(ex: &mut Execution, value: &Value) -> CallResult<()> {
    if let Value::Number(n) = value {
        ex.charge(n.size_hint_limbs() / 64 + 1)?;
    }
    Ok(())
}
fn changed() -> HostError {
    HostError::not_implemented("ИндексыКоллекции", "Рекурсивная операция")
}
fn integral(n: &Number, len: usize) -> Option<usize> {
    let i = n.trunc_to_i64_saturating();
    (n == &Number::from_i64(i))
        .then_some(i)
        .and_then(|i| usize::try_from(i).ok())
        .filter(|i| *i < len)
}
impl Indexes {
    pub(super) const fn allocation_bytes() -> u64 {
        (std::mem::size_of::<Self>()
            + std::mem::size_of::<IndexState>()
            + 4 * std::mem::size_of::<usize>()) as u64
    }
    pub(super) fn new(ex: &mut Execution, columns: Rc<Columns>, live: u64) -> CallResult<Rc<Self>> {
        ex.reserve(live.saturating_add(Self::allocation_bytes()))?;
        let state = Rc::new(IndexState {
            columns,
            entries: RefCell::new(Vec::new()),
            revision: Cell::new(0),
            next_id: Cell::new(0),
            bytes: Cell::new(Self::allocation_bytes()),
            hash: creation_number(),
        });
        let observer: Rc<dyn SchemaIndexes> = state.clone();
        state.columns.attach_indexes(Rc::downgrade(&observer));
        Ok(Rc::new(Self { state }))
    }
}
impl IndexState {
    fn len(&self) -> usize {
        self.entries.borrow().len()
    }
    fn bump(&self) {
        self.revision.set(self.revision.get().wrapping_add(1));
    }
    fn bump_source(&self) {
        self.bump();
        self.columns.index_storage_changed();
    }
    fn check(&self, revision: u64) -> CallResult<()> {
        if self.revision.get() != revision {
            return Err(changed().into());
        }
        Ok(())
    }
    fn live(&self, ex: &mut Execution, retained: Option<&Definition>) -> CallResult<u64> {
        let mut live = self.columns.owned_bytes(ex)?;
        if let Some(record) = retained {
            ex.charge(record.columns.len() as u64 + 1)?;
            live = live.saturating_add(record_bytes(record)).saturating_add(
                (std::mem::size_of::<CollectionIndex>() + 2 * std::mem::size_of::<usize>()) as u64,
            );
            for column in &record.columns {
                live = live.saturating_add(column.owned_bytes());
            }
        }
        ex.reserve(live)?;
        Ok(live)
    }
    fn argument_storage(&self, ex: &mut Execution, value: &Value) -> CallResult<u64> {
        if let Some(record) = value
            .as_object()
            .and_then(|o| o.as_any().downcast_ref::<CollectionIndex>())
        {
            return record.state.live(ex, Some(&record.record));
        }
        Ok(payload(value))
    }
    fn argument(
        &self,
        ex: &mut Execution,
        arg: Option<&Arg>,
        mut live: u64,
    ) -> CallResult<(Value, u64)> {
        if let Some(Arg::Val(value)) = arg {
            live = live.saturating_add(self.argument_storage(ex, value)?);
        }
        ex.reserve(live)?;
        let value = arg_value_cached(ex, arg, live)?;
        live = live.saturating_add(self.argument_storage(ex, &value)?);
        ex.reserve(live)?;
        Ok((value, live))
    }
    fn handle(
        self: &Rc<Self>,
        ex: &mut Execution,
        record: Rc<Definition>,
        live: u64,
    ) -> CallResult<Value> {
        ex.reserve(live.saturating_add(
            (std::mem::size_of::<CollectionIndex>() + 2 * std::mem::size_of::<usize>()) as u64,
        ))?;
        Ok(Value::Object(Rc::new(CollectionIndex {
            state: self.clone(),
            record,
        })))
    }
    fn at(self: &Rc<Self>, ex: &mut Execution, i: usize) -> CallResult<Value> {
        let live = self.live(ex, None)?;
        ex.charge(1)?;
        let record = self
            .entries
            .borrow()
            .get(i)
            .cloned()
            .ok_or_else(|| HostError::key(msg::INDEX_OFFBOUND))?;
        self.handle(ex, record, live)
    }
    fn add(self: &Rc<Self>, ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
        let revision = self.revision.get();
        let schema = self.columns.revision();
        let live = self.live(ex, None)?;
        let (value, mut live) = self.argument(ex, args.first(), live)?;
        self.check(revision)?;
        self.columns.check(schema)?;
        let Value::String(name) = &value else {
            return Err(type_mismatch(1).into());
        };
        ex.charge(name.len() as u64 / 32 + 1)?;
        if name.is_empty() {
            return Err(HostError::key("core85:IDS_VT_COLUMNCODE_ERROR").into());
        }
        let count = name.units().iter().filter(|u| **u == 44).count() + 1;
        live = live.saturating_add(vector::<Rc<ColumnRecord>>(count));
        ex.reserve(live)?;
        let mut columns: Vec<Rc<ColumnRecord>> = vec_with_capacity(ex, count as u64)?;
        for clause in name.units().split(|u| *u == 44) {
            let blank = |u: &u16| matches!(*u, 9 | 10 | 13 | 32);
            let start = clause
                .iter()
                .position(|u| !blank(u))
                .unwrap_or(clause.len());
            let end = clause
                .iter()
                .rposition(|u| !blank(u))
                .map_or(start, |i| i + 1);
            let clause = &clause[start..end];
            if clause.is_empty() {
                return Err(HostError::key("core85:IDS_VT_COLUMNCODE_ERROR").into());
            }
            ex.reserve(
                live.saturating_add((clause.len() as u64).saturating_mul(42))
                    .saturating_add(2048),
            )?;
            let selector = Str::from_units(clause);
            let record = self
                .columns
                .named_record(
                    ex,
                    &selector,
                    live.saturating_add((selector.len() as u64).saturating_mul(2)),
                )?
                .ok_or_else(|| HostError::key("core85:IDS_VT_COLUMNCODE_ERROR"))?;
            ex.charge(columns.len() as u64 + 1)?;
            if columns.iter().any(|c| c.id == record.id) {
                return Err(value_mismatch(1).into());
            }
            columns.push(record);
        }
        let id = self.next_id.get();
        let next = id.checked_add(1).ok_or_else(|| ex.out_of_memory())?;
        let count = self.len();
        ex.charge(count as u64 + 1)?;
        live = live
            .saturating_add(vector::<Rc<Definition>>(count + 1))
            .saturating_add(
                (std::mem::size_of::<Definition>() + 2 * std::mem::size_of::<usize>()) as u64,
            );
        ex.reserve(live)?;
        let record = Rc::new(Definition {
            id,
            hash: creation_number(),
            member: Cell::new(true),
            columns,
        });
        let mut entries = vec_with_capacity(ex, (count + 1) as u64)?;
        entries.extend(self.entries.borrow().iter().cloned());
        entries.push(record.clone());
        let bytes = Indexes::allocation_bytes()
            .saturating_add(vector::<Rc<Definition>>(entries.capacity()))
            .saturating_add(entries.iter().map(|r| record_bytes(r)).sum::<u64>());
        let handle = self.handle(ex, record, live)?;
        self.check(revision)?;
        self.columns.check(schema)?;
        *self.entries.borrow_mut() = entries;
        self.next_id.set(next);
        self.bytes.set(bytes);
        self.bump_source();
        Ok(handle)
    }
    fn delete(self: &Rc<Self>, ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
        let revision = self.revision.get();
        let schema = self.columns.revision();
        let live = self.live(ex, None)?;
        let (value, live) = self.argument(ex, args.first(), live)?;
        self.check(revision)?;
        self.columns.check(schema)?;
        number_work(ex, &value)?;
        let i = match &value {
            Value::Number(n) => {
                integral(n, self.len()).ok_or_else(|| HostError::key(msg::RANGE))?
            }
            Value::Object(o) => {
                let record = o
                    .as_any()
                    .downcast_ref::<CollectionIndex>()
                    .filter(|r| Rc::ptr_eq(&r.state, self) && r.record.member.get())
                    .ok_or_else(|| value_mismatch(1))?;
                ex.charge(self.len() as u64)?;
                self.entries
                    .borrow()
                    .iter()
                    .position(|r| Rc::ptr_eq(r, &record.record))
                    .ok_or_else(|| value_mismatch(1))?
            }
            _ => return Err(type_mismatch(1).into()),
        };
        ex.reserve(live)?;
        ex.charge(self.len() as u64 + 1)?;
        let record = self.entries.borrow_mut().remove(i);
        record.member.set(false);
        self.bytes
            .set(self.bytes.get().saturating_sub(record_bytes(&record)));
        self.bump_source();
        Ok(Value::Undefined)
    }
    fn clear(&self, ex: &mut Execution) -> CallResult<Value> {
        self.live(ex, None)?;
        ex.charge(self.len() as u64 + 1)?;
        let mut entries = self.entries.borrow_mut();
        for record in entries.iter() {
            record.member.set(false)
        }
        entries.clear();
        self.bytes.set(
            Indexes::allocation_bytes()
                .saturating_add(vector::<Rc<Definition>>(entries.capacity())),
        );
        self.bump_source();
        Ok(Value::Undefined)
    }
}
struct PreparedIndexes {
    state: Rc<IndexState>,
    revision: u64,
    entries: Vec<Rc<Definition>>,
    removed: Vec<Rc<Definition>>,
    bytes: u64,
}
impl PreparedSchemaChange for PreparedIndexes {
    fn check(&self) -> CallResult<()> {
        self.state.check(self.revision)
    }
    fn commit(self: Box<Self>) {
        for record in &self.removed {
            record.member.set(false)
        }
        *self.state.entries.borrow_mut() = self.entries;
        self.state.bytes.set(self.bytes);
        self.state.bump();
    }
}
impl SchemaIndexes for IndexState {
    fn owned_bytes(&self) -> u64 {
        self.bytes.get()
    }
    fn revision(&self) -> u64 {
        self.revision.get()
    }
    fn check(&self, revision: u64) -> CallResult<()> {
        self.check(revision)
    }
    fn prepare_remove(
        self: Rc<Self>,
        ex: &mut Execution,
        removed: &[u32],
        live: u64,
    ) -> CallResult<(Box<dyn PreparedSchemaChange>, u64)> {
        let revision = self.revision.get();
        let n = self.len();
        ex.charge(n as u64 + 1)?;
        {
            let entries = self.entries.borrow();
            for record in entries.iter() {
                ex.charge((record.columns.len() as u64).saturating_mul(removed.len() as u64 + 1))?;
            }
        }
        let live = live
            .saturating_add(
                (std::mem::size_of::<PreparedIndexes>() + 2 * std::mem::size_of::<usize>()) as u64,
            )
            .saturating_add(vector::<Rc<Definition>>(n).saturating_mul(2));
        ex.reserve(live)?;
        let mut entries = vec_with_capacity(ex, n as u64)?;
        let mut invalid = vec_with_capacity(ex, n as u64)?;
        for record in self.entries.borrow().iter() {
            if record.columns.iter().any(|c| removed.contains(&c.id)) {
                invalid.push(record.clone())
            } else {
                entries.push(record.clone())
            }
        }
        let bytes = Indexes::allocation_bytes()
            .saturating_add(vector::<Rc<Definition>>(entries.capacity()))
            .saturating_add(entries.iter().map(|r| record_bytes(r)).sum::<u64>());
        Ok((
            Box::new(PreparedIndexes {
                state: self,
                revision,
                entries,
                removed: invalid,
                bytes,
            }),
            live,
        ))
    }
}
impl HostObject for Indexes {
    fn session_edges(&self, visit: &mut dyn bsl_vm::SessionEdges) -> bool {
        visit.backing(self.state.clone())
    }
    fn session_transfer_supported(&self) -> bool {
        true
    }
    fn session_transfer(
        &self,
        out: &mut dyn bsl_vm::SessionExport,
    ) -> Result<Option<Box<dyn bsl_vm::SessionObject>>, bsl_vm::Exception> {
        session::capture_object(self, None, out).map(Some)
    }
    fn retained_memory(
        &self,
        visit: &mut dyn bsl_vm::RetainedMemory,
    ) -> Result<bool, bsl_vm::Exception> {
        crate::collections::accounting::project(self, visit)
    }
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(idx("CollectionIndexes"))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        Err(not_implemented(idx("CollectionIndexes"), m).into())
    }
    fn set(&self, _: &mut Execution, _: MemberId, _: Value) -> CallResult<()> {
        Err(HostError::key(msg::PROP_NOT_WRITABLE).raw().into())
    }
    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        match method_name(idx("CollectionIndexes"), m) {
            Some("Count") => Ok(Value::from(self.state.len() as i64)),
            Some("Add") => self.state.add(ex, args),
            Some("Delete") => self.state.delete(ex, args),
            Some("Clear") => self.state.clear(ex),
            _ => Err(not_implemented(idx("CollectionIndexes"), m).into()),
        }
    }
    fn iter_len(&self, _: &mut Execution) -> Option<usize> {
        Some(self.state.len())
    }
    fn iter_get(&self, ex: &mut Execution, i: usize) -> CallResult<Value> {
        self.state.at(ex, i)
    }
    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<Str>> {
        Ok(Some(type_name(ex, idx("CollectionIndexes"))))
    }
    fn boost_hash(&self) -> Option<i32> {
        Some(self.state.hash)
    }
}
impl CollectionIndex {
    fn at(&self, ex: &mut Execution, key: &Value) -> CallResult<Value> {
        let live = self
            .state
            .live(ex, Some(&self.record))?
            .saturating_add(payload(key));
        ex.reserve(live)?;
        number_work(ex, key)?;
        let Value::Number(n) = key else {
            return Err(type_mismatch(1).into());
        };
        let i = integral(n, self.record.columns.len())
            .ok_or_else(|| HostError::key(msg::INDEX_OFFBOUND))?;
        self.record.columns[i].current_name(ex, live)
    }
}
impl HostObject for CollectionIndex {
    fn session_edges(&self, visit: &mut dyn bsl_vm::SessionEdges) -> bool {
        visit.backing(self.state.clone()) && visit.backing(self.record.clone())
    }
    fn session_transfer_supported(&self) -> bool {
        true
    }
    fn session_transfer(
        &self,
        out: &mut dyn bsl_vm::SessionExport,
    ) -> Result<Option<Box<dyn bsl_vm::SessionObject>>, bsl_vm::Exception> {
        session::capture_object(
            &Indexes {
                state: self.state.clone(),
            },
            Some(&self.record),
            out,
        )
        .map(Some)
    }
    fn retained_memory(
        &self,
        visit: &mut dyn bsl_vm::RetainedMemory,
    ) -> Result<bool, bsl_vm::Exception> {
        crate::collections::accounting::project(self, visit)
    }
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(idx("CollectionIndex"))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        Err(not_implemented(idx("CollectionIndex"), m).into())
    }
    fn set(&self, _: &mut Execution, _: MemberId, _: Value) -> CallResult<()> {
        Err(HostError::key(msg::PROP_NOT_WRITABLE).raw().into())
    }
    fn call(&self, _: &mut Execution, m: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        Err(not_implemented(idx("CollectionIndex"), m).into())
    }
    fn index_get(&self, ex: &mut Execution, key: &Value) -> CallResult<Value> {
        self.at(ex, key)
    }
    fn iter_len(&self, _: &mut Execution) -> Option<usize> {
        Some(self.record.columns.len())
    }
    fn iter_get(&self, ex: &mut Execution, i: usize) -> CallResult<Value> {
        self.at(ex, &Value::from(i as i64))
    }
    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<Str>> {
        Ok(Some(type_name(ex, idx("CollectionIndex"))))
    }
    fn boost_hash(&self) -> Option<i32> {
        Some(self.record.hash)
    }
    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        Some(
            other.as_any().downcast_ref::<Self>().is_some_and(|o| {
                Rc::ptr_eq(&self.state, &o.state) && self.record.id == o.record.id
            }),
        )
    }
}

impl Indexes {
    pub(in crate::collections) fn retained_rc(
        value: &Rc<Self>,
        a: &mut impl crate::collections::accounting::Charge,
    ) -> crate::collections::accounting::Result<()> {
        a.shared_object(value)
    }
}
pub(super) fn retained_object(
    actual: &dyn Any,
    a: &mut impl crate::collections::accounting::Charge,
) -> Option<crate::collections::accounting::Result<()>> {
    if let Some(indexes) = actual.downcast_ref::<Indexes>() {
        Some(retained_state(&indexes.state, a))
    } else {
        actual.downcast_ref::<CollectionIndex>().map(|index| {
            (|| {
                retained_state(&index.state, a)?;
                retained_definition(&index.record, a)
            })()
        })
    }
}
fn retained_state(
    value: &Rc<IndexState>,
    a: &mut impl crate::collections::accounting::Charge,
) -> crate::collections::accounting::Result<()> {
    if !a.rc(value)? {
        return Ok(());
    }
    Columns::retained_rc(&value.columns, a)?;
    let entries = value
        .entries
        .try_borrow()
        .map_err(|_| crate::collections::RetainedChargeError::Borrowed)?;
    a.vector(&entries)?;
    for record in entries.iter() {
        retained_definition(record, a)?;
    }
    Ok(())
}
fn retained_definition(
    value: &Rc<Definition>,
    a: &mut impl crate::collections::accounting::Charge,
) -> crate::collections::accounting::Result<()> {
    if a.rc(value)? {
        a.vector(&value.columns)?;
        for column in &value.columns {
            ColumnRecord::retained_rc(column, a)?;
        }
    }
    Ok(())
}
