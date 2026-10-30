mod accounting;
pub(super) mod session;
use super::value_list::{arg_i64_cached, arg_value_cached, scalar_clone_bytes};
use super::{
    creation_number, is_name, method_name, not_implemented, position, property_name,
    reserve_values, type_name,
};
use crate::bind::{is_given, msg, type_mismatch, value_mismatch};
use crate::typedesc::{TypeDescription, new_type_description};
pub(super) use accounting::object as retained_object;
use bsl_prim::{Str, fold_name};
use bsl_vm::{
    Arg, CallResult, Execution, HostError, HostObject, MemberId, MemberKind, TypeId, TypeIdx, Value,
};
use std::{
    any::Any,
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
};

pub(super) const COLUMN_BASE: u32 = 0x10000;
const BAD_NAME: &str = "core85:IDS_VT_COLUMNCODE_ERROR";

struct ColumnData {
    name: Str,
    folded: String,
    title: Str,
    width: i64,
    value_type: Value,
}
pub(super) struct ColumnRecord {
    pub(super) id: u32,
    hash: i32,
    data: RefCell<ColumnData>,
}
struct Schema {
    entries: RefCell<Vec<Rc<ColumnRecord>>>,
    next_id: Cell<u32>,
    revision: Cell<u64>,
    collection_type: TypeIdx,
    column_type: TypeIdx,
    hash: i32,
    rows: RefCell<Option<Weak<dyn SchemaRows>>>,
    indexes: RefCell<Option<Weak<dyn SchemaIndexes>>>,
    rows_allocation: Cell<u64>,
    indexes_allocation: Cell<u64>,
}

pub(crate) struct Columns {
    schema: Rc<Schema>,
}
pub(super) struct Column {
    schema: Rc<Schema>,
    record: Rc<ColumnRecord>,
}

impl Column {
    pub(super) fn prepare_fill(
        &self,
        ex: &mut Execution,
        member: MemberId,
        value: Value,
        retained: u64,
    ) -> CallResult<(Value, [u64; 2], String)> {
        let rows = self.schema.rows.borrow().as_ref().and_then(Weak::upgrade);
        let stamp = [
            self.schema.revision.get(),
            rows.as_ref().map_or(0, |rows| rows.revision()),
        ];
        let mut folded = String::new();
        let value = match property_name(self.schema.column_type, member) {
            Some("Name") => {
                let name = text(ex, value, retained)?;
                folded = fold(
                    ex,
                    &name,
                    retained.saturating_add((name.len() as u64).saturating_mul(2)),
                )?;
                if !is_name(name.units()) {
                    return Err(HostError::key(BAD_NAME).into());
                }
                ex.charge(self.schema.len() as u64)?;
                if self.schema.at_id(self.record.id).is_some()
                    && self.schema.find(ex, &folded)?.is_some_and(|index| {
                        !Rc::ptr_eq(&self.schema.entries.borrow()[index], &self.record)
                    })
                {
                    return Err(HostError::key(BAD_NAME).into());
                }
                Value::String(name)
            }
            Some("Title") => Value::String(text(ex, value, retained)?),
            Some("Width") => Value::from(arg_i64_cached(ex, Some(&Arg::Val(value)), retained)?),
            Some("ValueType") => return Err(HostError::key(msg::PROP_NOT_WRITABLE).raw().into()),
            _ => return Err(not_implemented(self.schema.column_type, member).into()),
        };
        self.check_fill(member, stamp)?;
        Ok((value, stamp, folded))
    }

    pub(super) fn check_fill(&self, _member: MemberId, stamp: [u64; 2]) -> CallResult<()> {
        self.schema.check(stamp[0])?;
        if let Some(rows) = self.schema.rows.borrow().as_ref().and_then(Weak::upgrade) {
            rows.check(stamp[1])?;
        }
        Ok(())
    }

    pub(super) fn publish_fill(&self, member: MemberId, value: &mut Value, folded: &mut String) {
        let mut data = self.record.data.borrow_mut();
        match (property_name(self.schema.column_type, member), value) {
            (Some("Name"), Value::String(name)) => {
                std::mem::swap(&mut data.name, name);
                std::mem::swap(&mut data.folded, folded);
            }
            (Some("Title"), Value::String(title)) => std::mem::swap(&mut data.title, title),
            (Some("Width"), Value::Number(width)) => {
                if let Some(width) = width.to_i64() {
                    data.width = width;
                } else {
                    unreachable!("unprepared column width")
                }
            }
            _ => unreachable!("unprepared column property"),
        }
        self.schema.bump();
    }
}

pub(super) struct PreparedColumns {
    schema: Rc<Schema>,
    revision: u64,
    entries: Vec<Rc<ColumnRecord>>,
    indexes: Option<Box<dyn PreparedSchemaChange>>,
}
impl PreparedColumns {
    pub(super) fn check(&self) -> CallResult<()> {
        self.schema.check(self.revision)?;
        if let Some(indexes) = &self.indexes {
            indexes.check()?;
        }
        Ok(())
    }
    pub(super) fn commit(self) {
        *self.schema.entries.borrow_mut() = self.entries;
        self.schema.bump();
        if let Some(indexes) = self.indexes {
            indexes.commit();
        }
    }
}

pub(super) enum SchemaChange {
    Insert { id: u32, value_type: Value },
    Delete { id: u32 },
    Clear,
}
pub(super) trait PreparedSchemaChange {
    fn check(&self) -> CallResult<()>;
    fn commit(self: Box<Self>);
}
pub(super) trait SchemaIndexes {
    fn owned_bytes(&self) -> u64;
    fn revision(&self) -> u64;
    fn check(&self, revision: u64) -> CallResult<()>;
    fn prepare_remove(
        self: Rc<Self>,
        ex: &mut Execution,
        removed: &[u32],
        live: u64,
    ) -> CallResult<(Box<dyn PreparedSchemaChange>, u64)>;
}
pub(super) trait SchemaRows {
    fn revision(&self) -> u64;
    fn check(&self, revision: u64) -> CallResult<()>;
    fn owned_bytes(&self) -> u64;
    fn prepare(
        self: Rc<Self>,
        ex: &mut Execution,
        change: SchemaChange,
        live: u64,
    ) -> CallResult<Box<dyn PreparedSchemaChange>>;
}

fn changed(t: TypeIdx) -> HostError {
    HostError::not_implemented(super::type_name_ru(t), "Рекурсивная операция")
}
fn fold(ex: &mut Execution, name: &Str, live: u64) -> CallResult<String> {
    ex.reserve(live.saturating_add((name.len() as u64).saturating_mul(8)))?;
    ex.charge(name.len() as u64 / 32 + 1)?;
    Ok(fold_name(&name.to_string_lossy()))
}
fn text(ex: &mut Execution, value: Value, live: u64) -> CallResult<Str> {
    let text = match value {
        Value::String(s) => s,
        Value::Undefined | Value::Null => Str::new(),
        other => crate::present::present_for_collection(
            ex,
            &other,
            live.saturating_add(scalar_clone_bytes(&other)),
        )?,
    };
    ex.charge(text.len() as u64 / 32 + 1)?;
    ex.reserve(live.saturating_add((text.len() as u64).saturating_mul(2)))?;
    Ok(text)
}
fn argument_text(ex: &mut Execution, arg: Option<&Arg>, live: u64) -> CallResult<Str> {
    let value = arg_value_cached(ex, arg, live)?;
    text(ex, value, live)
}

pub(super) fn missing_field(ex: &mut Execution, key: &Value) -> CallResult<HostError> {
    let text = crate::present::present_for_collection(ex, key, scalar_clone_bytes(key))?;
    let len = text.len() as u64;
    ex.charge(len / 32 + 1)?;
    ex.reserve(
        crate::present::collection_presentation_live()
            .saturating_add(len.saturating_mul(32).saturating_add(2048)),
    )?;
    Ok(HostError::key(msg::PROP_NOT_FOUND).with_arg(text.to_string_lossy()))
}

fn type_bytes(td: &TypeDescription) -> u64 {
    (std::mem::size_of::<TypeDescription>() + 2 * std::mem::size_of::<usize>()) as u64
        + (td.types().len() as u64).saturating_mul(std::mem::size_of::<TypeId>() as u64)
}
impl ColumnRecord {
    pub(super) fn current_name(&self, ex: &mut Execution, live: u64) -> CallResult<Value> {
        let data = self.data.borrow();
        ex.charge(data.name.len() as u64 / 32 + 1)?;
        ex.reserve(live.saturating_add((data.name.len() as u64).saturating_mul(2)))?;
        Ok(Value::String(data.name.clone()))
    }

    pub(super) fn value_type(&self) -> Value {
        self.data.borrow().value_type.clone()
    }

    pub(super) fn text_slack_bytes(&self) -> u64 {
        let data = self.data.borrow();
        super::retained::text_extra(&data.name)
            .saturating_add(super::retained::text_extra(&data.title))
    }

    pub(super) fn owned_bytes(&self) -> u64 {
        let data = self.data.borrow();
        let td = data
            .value_type
            .as_object()
            .and_then(|o| o.as_any().downcast_ref::<TypeDescription>());
        (std::mem::size_of::<Self>() + 2 * std::mem::size_of::<usize>()) as u64
            + ((data.name.len() as u64).saturating_add(data.title.len() as u64)).saturating_mul(2)
            + data.folded.capacity() as u64
            + td.map_or(0, type_bytes)
    }
}
impl Schema {
    pub(super) fn owned_bytes(&self, ex: &mut Execution) -> CallResult<u64> {
        ex.charge(self.len() as u64)?;
        let entries = self.entries.borrow();
        let base = Columns::allocation_bytes().saturating_add(
            (entries.capacity() as u64)
                .saturating_mul(std::mem::size_of::<Rc<ColumnRecord>>() as u64),
        );
        let indexes = self.indexes.borrow().as_ref().and_then(Weak::upgrade);
        Ok(entries
            .iter()
            .fold(base, |n, r| n.saturating_add(r.owned_bytes()))
            .saturating_add(indexes.map_or(0, |i| i.owned_bytes())))
    }
    fn len(&self) -> usize {
        self.entries.borrow().len()
    }
    fn bump(&self) {
        self.revision.set(self.revision.get().wrapping_add(1));
    }
    fn check(&self, revision: u64) -> CallResult<()> {
        if self.revision.get() != revision {
            return Err(changed(self.collection_type).into());
        }
        Ok(())
    }
    fn find(&self, ex: &mut Execution, folded: &str) -> CallResult<Option<usize>> {
        ex.charge(self.len() as u64)?;
        let entries = self.entries.borrow();
        let work = entries.iter().fold(0u64, |n, e| {
            n.saturating_add((e.data.borrow().folded.len().min(folded.len()) as u64) / 32)
        });
        ex.charge(work)?;
        Ok(entries
            .iter()
            .position(|entry| entry.data.borrow().folded == folded))
    }
    fn at_id(&self, id: u32) -> Option<Rc<ColumnRecord>> {
        self.entries.borrow().iter().find(|r| r.id == id).cloned()
    }
    fn handle(self: &Rc<Self>, ex: &mut Execution, record: Rc<ColumnRecord>) -> CallResult<Value> {
        ex.reserve((std::mem::size_of::<Column>() + 2 * std::mem::size_of::<usize>()) as u64)?;
        Ok(Value::Object(Rc::new(Column {
            schema: self.clone(),
            record,
        })))
    }
}
impl Columns {
    pub(in crate::collections) fn property_names(
        &self,
        ex: &mut Execution,
        retained: u64,
    ) -> CallResult<Vec<Str>> {
        let entries = self.schema.entries.borrow();
        ex.charge(entries.len() as u64)?;
        ex.reserve(retained.saturating_add(
            (entries.len() as u64).saturating_mul(std::mem::size_of::<Str>() as u64),
        ))?;
        let mut names = super::vec_with_capacity(ex, entries.len() as u64)?;
        ex.reserve(retained.saturating_add(
            (names.capacity() as u64).saturating_mul(std::mem::size_of::<Str>() as u64),
        ))?;
        names.extend(
            entries
                .iter()
                .map(|column| column.data.borrow().name.clone()),
        );
        Ok(names)
    }
    pub(super) fn prepare_keep(
        &self,
        ex: &mut Execution,
        keep: &[Rc<ColumnRecord>],
        live: u64,
    ) -> CallResult<(PreparedColumns, u64)> {
        let revision = self.schema.revision.get();
        ex.charge(
            (self.schema.len() as u64)
                .saturating_mul(keep.len() as u64)
                .saturating_add(keep.len() as u64),
        )?;
        ex.reserve(live.saturating_add(
            (keep.len() as u64).saturating_mul(std::mem::size_of::<Rc<ColumnRecord>>() as u64),
        ))?;
        ex.charge(self.schema.len() as u64)?;
        let live = live
            .saturating_add(
                (self.schema.len() as u64).saturating_mul(std::mem::size_of::<u32>() as u64),
            )
            .saturating_add(
                (keep.len() as u64).saturating_mul(std::mem::size_of::<Rc<ColumnRecord>>() as u64),
            )
            .saturating_add(std::mem::size_of::<PreparedColumns>() as u64);
        ex.reserve(live)?;
        let mut removed = super::vec_with_capacity(ex, self.schema.len() as u64)?;
        let current = self.schema.entries.borrow();
        let mut entries = super::vec_with_capacity(ex, keep.len() as u64)?;
        for record in current.iter() {
            if keep.iter().any(|k| Rc::ptr_eq(record, k)) {
                entries.push(record.clone());
            } else {
                removed.push(record.id);
            }
        }
        if entries.len() != keep.len() {
            return Err(changed(self.schema.collection_type).into());
        }
        drop(current);
        let (indexes, live) = self.prepare_indexes(ex, &removed, live)?;
        Ok((
            PreparedColumns {
                schema: self.schema.clone(),
                revision,
                entries,
                indexes,
            },
            live,
        ))
    }
    pub(super) fn named_record(
        &self,
        ex: &mut Execution,
        name: &Str,
        live: u64,
    ) -> CallResult<Option<Rc<ColumnRecord>>> {
        let folded = fold(ex, name, live)?;
        Ok(self
            .schema
            .find(ex, &folded)?
            .map(|i| self.schema.entries.borrow()[i].clone()))
    }
    pub(super) fn index_storage_changed(&self) {
        self.schema.bump();
    }
    pub(super) fn attach_indexes(&self, indexes: Weak<dyn SchemaIndexes>) {
        self.schema.indexes_allocation.set(
            indexes
                .upgrade()
                .map_or(u64::MAX, |owner| super::retained::rc_bytes(owner.as_ref())),
        );
        *self.schema.indexes.borrow_mut() = Some(indexes);
    }
    fn indexes(&self) -> Option<Rc<dyn SchemaIndexes>> {
        self.schema
            .indexes
            .borrow()
            .as_ref()
            .and_then(Weak::upgrade)
    }
    fn prepare_indexes(
        &self,
        ex: &mut Execution,
        removed: &[u32],
        live: u64,
    ) -> CallResult<(Option<Box<dyn PreparedSchemaChange>>, u64)> {
        match self.indexes() {
            Some(indexes) => {
                let (change, live) = indexes.prepare_remove(ex, removed, live)?;
                Ok((Some(change), live))
            }
            None => Ok((None, live)),
        }
    }
    pub(super) fn attach_rows(&self, rows: Weak<dyn SchemaRows>) {
        self.schema.rows_allocation.set(
            rows.upgrade()
                .map_or(u64::MAX, |owner| super::retained::rc_bytes(owner.as_ref())),
        );
        *self.schema.rows.borrow_mut() = Some(rows);
    }
    fn rows(&self) -> Option<Rc<dyn SchemaRows>> {
        self.schema.rows.borrow().as_ref().and_then(Weak::upgrade)
    }
    pub(super) fn revision(&self) -> u64 {
        self.schema.revision.get()
    }
    pub(super) fn check(&self, revision: u64) -> CallResult<()> {
        self.schema.check(revision)
    }
    pub(super) fn len(&self) -> usize {
        self.schema.len()
    }
    pub(super) fn owned_bytes(&self, ex: &mut Execution) -> CallResult<u64> {
        self.schema.owned_bytes(ex)
    }
    pub(super) fn record(&self, id: u32) -> Option<Rc<ColumnRecord>> {
        self.schema.at_id(id)
    }
    pub(super) fn records(
        &self,
        ex: &mut Execution,
        live: u64,
    ) -> CallResult<Vec<Rc<ColumnRecord>>> {
        ex.charge(self.len() as u64)?;
        ex.reserve(live.saturating_add(
            (self.len() as u64).saturating_mul(std::mem::size_of::<Rc<ColumnRecord>>() as u64),
        ))?;
        let mut out = super::vec_with_capacity(ex, self.len() as u64)?;
        out.extend(self.schema.entries.borrow().iter().cloned());
        Ok(out)
    }
    pub(super) fn copy_selected(
        &self,
        ex: &mut Execution,
        records: &[Rc<ColumnRecord>],
        live: u64,
    ) -> CallResult<(Rc<Self>, u64)> {
        let next_id = u32::try_from(records.len())
            .ok()
            .filter(|n| *n < u32::MAX - COLUMN_BASE)
            .ok_or_else(|| ex.out_of_memory())?;
        ex.charge(records.len() as u64)?;
        let mut live = live
            .saturating_add(Self::allocation_bytes())
            .saturating_add(
                (records.len() as u64)
                    .saturating_mul(std::mem::size_of::<Rc<ColumnRecord>>() as u64),
            );
        for record in records {
            let data = record.data.borrow();
            let descriptor = data
                .value_type
                .as_object()
                .and_then(|o| o.as_any().downcast_ref::<TypeDescription>());
            ex.charge(
                (data.name.len() as u64)
                    .saturating_add(data.title.len() as u64)
                    .saturating_add(data.folded.len() as u64)
                    / 32
                    + 1,
            )?;
            ex.charge(descriptor.map_or(0, |td| td.types().len() as u64 / 64))?;
            live = live.saturating_add(record.owned_bytes());
        }
        ex.reserve(live)?;
        let mut entries = super::vec_with_capacity(ex, records.len() as u64)?;
        for (id, record) in records.iter().enumerate() {
            let data = record.data.borrow();
            entries.push(Rc::new(ColumnRecord {
                id: id as u32,
                hash: creation_number(),
                data: RefCell::new(ColumnData {
                    name: data.name.clone(),
                    folded: data.folded.clone(),
                    title: data.title.clone(),
                    width: data.width,
                    value_type: data.value_type.clone(),
                }),
            }));
        }
        Ok((
            Rc::new(Self {
                schema: Rc::new(Schema {
                    entries: RefCell::new(entries),
                    next_id: Cell::new(next_id),
                    revision: Cell::new(0),
                    collection_type: self.schema.collection_type,
                    column_type: self.schema.column_type,
                    hash: creation_number(),
                    rows: RefCell::new(None),
                    indexes: RefCell::new(None),
                    rows_allocation: Cell::new(0),
                    indexes_allocation: Cell::new(0),
                }),
            }),
            live,
        ))
    }
    pub(super) fn cell_column(
        &self,
        ex: &mut Execution,
        key: &Value,
    ) -> CallResult<Rc<ColumnRecord>> {
        let i = match key {
            Value::Number(n) => position(n)
                .filter(|i| *i < self.len())
                .ok_or_else(|| HostError::key(msg::INDEX_OFFBOUND))?,
            Value::String(name) => {
                let folded = fold(ex, name, (name.len() as u64).saturating_mul(2))?;
                let Some(i) = self.schema.find(ex, &folded)? else {
                    return Err(missing_field(ex, key)?.into());
                };
                i
            }
            _ => return Err(missing_field(ex, key)?.into()),
        };
        Ok(self.schema.entries.borrow()[i].clone())
    }

    pub(crate) const fn allocation_bytes() -> u64 {
        (std::mem::size_of::<Schema>()
            + std::mem::size_of::<Self>()
            + 4 * std::mem::size_of::<usize>()) as u64
    }
    pub(crate) fn new(
        ex: &mut Execution,
        collection_type: TypeIdx,
        column_type: TypeIdx,
    ) -> CallResult<Rc<Self>> {
        ex.reserve(Self::allocation_bytes())?;
        Ok(Rc::new(Self {
            schema: Rc::new(Schema {
                entries: RefCell::new(Vec::new()),
                next_id: Cell::new(0),
                revision: Cell::new(0),
                collection_type,
                column_type,
                hash: creation_number(),
                rows: RefCell::new(None),
                indexes: RefCell::new(None),
                rows_allocation: Cell::new(0),
                indexes_allocation: Cell::new(0),
            }),
        }))
    }
    fn at(&self, ex: &mut Execution, i: usize) -> CallResult<Value> {
        let record = self
            .schema
            .entries
            .borrow()
            .get(i)
            .cloned()
            .ok_or_else(|| HostError::key(msg::INDEX_OFFBOUND))?;
        self.schema.handle(ex, record)
    }
    fn numeric(&self, ex: &mut Execution, arg: Option<&Arg>, insert: bool) -> CallResult<usize> {
        let n = arg_i64_cached(ex, arg, 0)?;
        match usize::try_from(n).ok() {
            Some(i) if i < self.schema.len() || (insert && i == self.schema.len()) => Ok(i),
            _ => Err(HostError::key(msg::RANGE).into()),
        }
    }
    fn index_of(&self, ex: &mut Execution, value: &Value) -> CallResult<Option<usize>> {
        let column = value
            .as_object()
            .and_then(|o| o.as_any().downcast_ref::<Column>())
            .ok_or_else(|| type_mismatch(1))?;
        ex.charge(self.schema.len() as u64)?;
        if !Rc::ptr_eq(&self.schema, &column.schema) {
            return Ok(None);
        }
        Ok(self
            .schema
            .entries
            .borrow()
            .iter()
            .position(|r| Rc::ptr_eq(r, &column.record)))
    }
    fn selector(&self, ex: &mut Execution, value: &Value, names: bool) -> CallResult<usize> {
        match value {
            Value::Number(n) => position(n)
                .filter(|i| *i < self.schema.len())
                .ok_or_else(|| HostError::key(msg::RANGE).into()),
            Value::String(name) if names => {
                let folded = fold(ex, name, (name.len() as u64).saturating_mul(2))?;
                self.schema
                    .find(ex, &folded)?
                    .ok_or_else(|| value_mismatch(1).into())
            }
            _ => self
                .index_of(ex, value)?
                .ok_or_else(|| value_mismatch(1).into()),
        }
    }
    fn add(&self, ex: &mut Execution, args: &mut [Arg], insert: bool) -> CallResult<Value> {
        let revision = self.schema.revision.get();
        let rows = self.rows();
        let row_revision = rows.as_ref().map(|r| r.revision());
        let indexes = self.indexes();
        let index_revision = indexes.as_ref().map(|i| i.revision());
        let i = if insert {
            self.numeric(ex, args.first(), true)?
        } else {
            self.schema.len()
        };
        let args = if insert {
            args.get(1..).unwrap_or_default()
        } else {
            args
        };
        let mut id = self.schema.next_id.get();
        let base = self
            .schema
            .owned_bytes(ex)?
            .saturating_add(rows.as_ref().map_or(0, |r| r.owned_bytes()));
        let name_value = arg_value_cached(ex, args.first(), base)?;
        let auto = matches!(name_value, Value::Undefined);
        let name = if auto {
            Str::new()
        } else {
            text(ex, name_value, base)?
        };
        let mut live = base.saturating_add((name.len() as u64).saturating_mul(2));
        let td = arg_value_cached(ex, args.get(1), live)?;
        let descriptor = if matches!(td, Value::Undefined) {
            None
        } else {
            Some(
                td.as_object()
                    .and_then(|o| o.as_any().downcast_ref::<TypeDescription>())
                    .ok_or_else(|| type_mismatch(if insert { 3 } else { 2 }))?,
            )
        };
        let bytes = descriptor.map_or(
            (std::mem::size_of::<TypeDescription>() + 2 * std::mem::size_of::<usize>()) as u64,
            type_bytes,
        );
        ex.reserve(live.saturating_add(bytes))?;
        ex.charge(descriptor.map_or(0, |td| td.types().len() as u64 / 64))?;
        let value_type = new_type_description(match descriptor {
            Some(td) => td.clone(),
            None => TypeDescription::new(&[], None, None, None, None, ex.runtime()),
        });
        live = live.saturating_add(bytes);
        let title = argument_text(ex, args.get(2), live)?;
        live = live.saturating_add((title.len() as u64).saturating_mul(2));
        let width = if is_given(args.get(3)) {
            arg_i64_cached(ex, args.get(3), live)?
        } else {
            0
        };
        if let (Some(indexes), Some(revision)) = (&indexes, index_revision) {
            indexes.check(revision)?;
        }
        self.schema.check(revision)?;
        let name = if auto {
            loop {
                ex.reserve(live.saturating_add(256))?;
                let prefix = if ex.script_variant() == bsl_vm::Lang::Ru {
                    "Колонка"
                } else {
                    "Column"
                };
                let name = Str::from(format!("{prefix}{}", u64::from(id) + 1));
                let folded = fold(ex, &name, live.saturating_add(64))?;
                if self.schema.find(ex, &folded)?.is_none() {
                    break name;
                }
                id = id
                    .checked_add(1)
                    .filter(|n| *n < u32::MAX - COLUMN_BASE)
                    .ok_or_else(|| ex.out_of_memory())?;
            }
        } else {
            name
        };
        ex.charge(name.len() as u64 / 32 + 1)?;
        if !is_name(name.units()) {
            return Err(HostError::key(BAD_NAME).into());
        }
        let folded = fold(ex, &name, live)?;
        if self.schema.find(ex, &folded)?.is_some() {
            return Err(HostError::key(BAD_NAME).into());
        }
        let next_id = id
            .checked_add(1)
            .filter(|n| *n < u32::MAX - COLUMN_BASE)
            .ok_or_else(|| ex.out_of_memory())?;
        let len = self.schema.len();
        reserve_values(ex, len as u64 + 1)?;
        let bytes = live
            .saturating_add(folded.capacity() as u64)
            .saturating_add(
                ((len as u64) + 1).saturating_mul(std::mem::size_of::<Rc<ColumnRecord>>() as u64),
            )
            .saturating_add(
                (std::mem::size_of::<ColumnRecord>()
                    + std::mem::size_of::<Column>()
                    + 4 * std::mem::size_of::<usize>()) as u64,
            );
        ex.reserve(bytes)?;
        ex.charge((len - i) as u64 + 1)?;
        let record = Rc::new(ColumnRecord {
            id,
            hash: creation_number(),
            data: RefCell::new(ColumnData {
                name,
                folded,
                title,
                width,
                value_type,
            }),
        });
        let handle = self.schema.handle(ex, record.clone())?;
        if let (Some(rows), Some(rev)) = (&rows, row_revision) {
            rows.check(rev)?;
        }
        if let (Some(indexes), Some(revision)) = (&indexes, index_revision) {
            indexes.check(revision)?;
        }
        let prepared = rows
            .map(|rows| {
                rows.prepare(
                    ex,
                    SchemaChange::Insert {
                        id,
                        value_type: record.value_type(),
                    },
                    bytes,
                )
            })
            .transpose()?;
        if let (Some(indexes), Some(revision)) = (&indexes, index_revision) {
            indexes.check(revision)?;
        }
        self.schema.check(revision)?;
        if let Some(change) = &prepared {
            change.check()?;
        }
        let mut entries = self.schema.entries.borrow_mut();
        entries
            .try_reserve_exact(1)
            .map_err(|_| ex.out_of_memory())?;
        entries.insert(i, record);
        self.schema.next_id.set(next_id);
        self.schema.bump();
        drop(entries);
        if let Some(change) = prepared {
            change.commit();
        }
        Ok(handle)
    }
}

impl HostObject for Columns {
    fn session_edges(&self, visit: &mut dyn bsl_vm::SessionEdges) -> bool {
        visit.backing(self.schema.clone())
    }
    fn session_transfer_supported(&self) -> bool {
        true
    }
    fn session_transfer(
        &self,
        out: &mut dyn bsl_vm::SessionExport,
    ) -> Result<Option<Box<dyn bsl_vm::SessionObject>>, bsl_vm::Exception> {
        session::columns(self, out).map(Some)
    }
    fn retained_memory(
        &self,
        visit: &mut dyn bsl_vm::RetainedMemory,
    ) -> Result<bool, bsl_vm::Exception> {
        crate::collections::accounting::project(self, visit)
    }
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.schema.collection_type)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        ex.charge(self.schema.len() as u64)?;
        let record =
            m.0.checked_sub(COLUMN_BASE)
                .and_then(|id| self.schema.at_id(id))
                .ok_or_else(|| not_implemented(self.schema.collection_type, m))?;
        self.schema.handle(ex, record)
    }
    fn set(&self, _: &mut Execution, _: MemberId, _: Value) -> CallResult<()> {
        Err(HostError::key(msg::PROP_NOT_WRITABLE).raw().into())
    }
    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        let t = self.schema.collection_type;
        match method_name(t, m) {
            Some("Count") => return Ok(Value::from(self.schema.len() as i64)),
            Some("Add") => return self.add(ex, args, false),
            Some("Insert") => return self.add(ex, args, true),
            Some("Get") => {
                let i = self.numeric(ex, args.first(), false)?;
                return self.at(ex, i);
            }
            Some("Find") => {
                let name = argument_text(ex, args.first(), 0)?;
                let folded = fold(ex, &name, (name.len() as u64).saturating_mul(2))?;
                return match self.schema.find(ex, &folded)? {
                    Some(i) => self.at(ex, i),
                    None => Ok(Value::Undefined),
                };
            }
            Some("IndexOf") => {
                let value = arg_value_cached(ex, args.first(), 0)?;
                return Ok(Value::from(
                    self.index_of(ex, &value)?.map_or(-1, |i| i as i64),
                ));
            }
            Some("Clear") => {
                let revision = self.revision();
                let live = self.owned_bytes(ex)?;
                ex.charge(self.schema.len() as u64)?;
                let live = live.saturating_add(
                    (self.schema.len() as u64).saturating_mul(std::mem::size_of::<u32>() as u64),
                );
                ex.reserve(live)?;
                let mut removed = super::vec_with_capacity(ex, self.schema.len() as u64)?;
                removed.extend(self.schema.entries.borrow().iter().map(|r| r.id));
                let (indexes, live) = self.prepare_indexes(ex, &removed, live)?;
                let prepared = self
                    .rows()
                    .map(|rows| rows.prepare(ex, SchemaChange::Clear, live))
                    .transpose()?;
                ex.charge(self.schema.len() as u64)?;
                self.check(revision)?;
                if let Some(indexes) = &indexes {
                    indexes.check()?;
                }
                if let Some(change) = &prepared {
                    change.check()?;
                }
                self.schema.entries.borrow_mut().clear();
                self.schema.bump();
                if let Some(indexes) = indexes {
                    indexes.commit();
                }
                if let Some(change) = prepared {
                    change.commit();
                }
            }
            Some("Delete") | Some("Move") => {
                let revision = self.schema.revision.get();
                let rows = self.rows();
                let row_revision = rows.as_ref().map(|r| r.revision());
                let value = arg_value_cached(ex, args.first(), 0)?;
                let offset = if method_name(t, m) == Some("Move") {
                    Some(arg_i64_cached(ex, args.get(1), scalar_clone_bytes(&value))?)
                } else {
                    None
                };
                self.schema.check(revision)?;
                let i = self.selector(ex, &value, offset.is_none())?;
                let dest = if let Some(offset) = offset {
                    Some(
                        (i as i64)
                            .checked_add(offset)
                            .and_then(|n| usize::try_from(n).ok())
                            .filter(|n| *n < self.schema.len())
                            .ok_or_else(|| value_mismatch(2))?,
                    )
                } else {
                    None
                };
                ex.charge(self.schema.len() as u64)?;
                if let (Some(rows), Some(rev)) = (&rows, row_revision) {
                    rows.check(rev)?;
                }
                let (indexes, live) = if dest.is_none() {
                    let id = self.schema.entries.borrow()[i].id;
                    let live = self
                        .owned_bytes(ex)?
                        .saturating_add(scalar_clone_bytes(&value));
                    self.prepare_indexes(ex, &[id], live)?
                } else {
                    (None, 0)
                };
                let prepared = if dest.is_none() {
                    let id = self.schema.entries.borrow()[i].id;
                    rows.map(|rows| rows.prepare(ex, SchemaChange::Delete { id }, live))
                        .transpose()?
                } else {
                    None
                };
                self.check(revision)?;
                if let Some(indexes) = &indexes {
                    indexes.check()?;
                }
                if let Some(change) = &prepared {
                    change.check()?;
                }
                let mut entries = self.schema.entries.borrow_mut();
                let record = entries.remove(i);
                if let Some(dest) = dest {
                    entries.insert(dest, record);
                }
                self.schema.bump();
                drop(entries);
                if let Some(indexes) = indexes {
                    indexes.commit();
                }
                if let Some(change) = prepared {
                    change.commit();
                }
            }
            _ => return Err(not_implemented(t, m).into()),
        }
        Ok(Value::Undefined)
    }
    fn find_member(&self, name: &str) -> Option<MemberId> {
        let folded = fold_name(name);
        self.schema
            .entries
            .borrow()
            .iter()
            .find(|r| r.data.borrow().folded == folded)
            .map(|r| MemberId(COLUMN_BASE + r.id))
    }
    fn find_member_guarded(
        &self,
        ex: &mut Execution,
        name: &str,
    ) -> Result<Option<MemberId>, bsl_vm::Exception> {
        let bytes = (name.len() as u64).saturating_mul(8);
        ex.reserve(bytes.saturating_add(256))?;
        let per_entry = (name.len() as u64).saturating_mul(4) / 32 + 2;
        ex.charge(
            (name.len() as u64 / 32 + 1)
                .saturating_add((self.schema.len() as u64).saturating_mul(per_entry)),
        )?;
        Ok(self.find_member(name))
    }
    fn member_kind(&self, m: MemberId) -> Option<MemberKind> {
        self.schema.at_id(m.0.checked_sub(COLUMN_BASE)?)?;
        Some(MemberKind::Property {
            readable: true,
            writable: false,
        })
    }
    fn index_get(&self, ex: &mut Execution, key: &Value) -> CallResult<Value> {
        let i = match key {
            Value::Number(n) => position(n)
                .filter(|i| *i < self.schema.len())
                .ok_or_else(|| HostError::key(msg::INDEX_OFFBOUND))?,
            Value::String(name) => {
                let folded = fold(ex, name, (name.len() as u64).saturating_mul(2))?;
                let Some(i) = self.schema.find(ex, &folded)? else {
                    return Err(missing_field(ex, key)?.into());
                };
                i
            }
            _ => return Err(missing_field(ex, key)?.into()),
        };
        self.at(ex, i)
    }
    fn iter_len(&self, _: &mut Execution) -> Option<usize> {
        Some(self.schema.len())
    }
    fn iter_get(&self, ex: &mut Execution, i: usize) -> CallResult<Value> {
        self.at(ex, i)
    }
    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<Str>> {
        Ok(Some(type_name(ex, self.schema.collection_type)))
    }
    fn boost_hash(&self) -> Option<i32> {
        Some(self.schema.hash)
    }
}
impl HostObject for Column {
    fn session_edges(&self, visit: &mut dyn bsl_vm::SessionEdges) -> bool {
        visit.backing(self.schema.clone()) && visit.backing(self.record.clone())
    }
    fn session_transfer_supported(&self) -> bool {
        true
    }
    fn session_transfer(
        &self,
        out: &mut dyn bsl_vm::SessionExport,
    ) -> Result<Option<Box<dyn bsl_vm::SessionObject>>, bsl_vm::Exception> {
        session::column(self, out).map(Some)
    }
    fn retained_memory(
        &self,
        visit: &mut dyn bsl_vm::RetainedMemory,
    ) -> Result<bool, bsl_vm::Exception> {
        crate::collections::accounting::project(self, visit)
    }
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.schema.column_type)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        let data = self.record.data.borrow();
        match property_name(self.schema.column_type, m) {
            Some("Name") => Ok(Value::String(data.name.clone())),
            Some("Title") => Ok(Value::String(data.title.clone())),
            Some("Width") => Ok(Value::from(data.width)),
            Some("ValueType") => Ok(data.value_type.clone()),
            _ => Err(not_implemented(self.schema.column_type, m).into()),
        }
    }
    fn set(&self, ex: &mut Execution, m: MemberId, value: Value) -> CallResult<()> {
        let revision = self.schema.revision.get();
        let rows = self.schema.rows.borrow().as_ref().and_then(Weak::upgrade);
        let row_revision = rows.as_ref().map(|r| r.revision());
        let check = || -> CallResult<()> {
            self.schema.check(revision)?;
            if let (Some(rows), Some(revision)) = (&rows, row_revision) {
                rows.check(revision)?;
            }
            Ok(())
        };
        let live = self
            .schema
            .owned_bytes(ex)?
            .saturating_add(self.record.owned_bytes())
            .saturating_add(rows.as_ref().map_or(0, |r| r.owned_bytes()));
        match property_name(self.schema.column_type, m) {
            Some("Name") => {
                let name = text(ex, value, live)?;
                let folded = fold(
                    ex,
                    &name,
                    live.saturating_add((name.len() as u64).saturating_mul(2)),
                )?;
                check()?;
                if !is_name(name.units()) {
                    return Err(HostError::key(BAD_NAME).into());
                }
                ex.charge(self.schema.len() as u64)?;
                let current = self.schema.at_id(self.record.id).is_some();
                if current
                    && self.schema.find(ex, &folded)?.is_some_and(|i| {
                        !Rc::ptr_eq(&self.schema.entries.borrow()[i], &self.record)
                    })
                {
                    return Err(HostError::key(BAD_NAME).into());
                }
                let mut data = self.record.data.borrow_mut();
                data.name = name;
                data.folded = folded;
            }
            Some("Title") => {
                let title = text(ex, value, live)?;
                check()?;
                self.record.data.borrow_mut().title = title;
            }
            Some("Width") => {
                let width = arg_i64_cached(ex, Some(&Arg::Val(value)), live)?;
                check()?;
                self.record.data.borrow_mut().width = width;
            }
            Some("ValueType") => return Err(HostError::key(msg::PROP_NOT_WRITABLE).raw().into()),
            _ => return Err(not_implemented(self.schema.column_type, m).into()),
        }
        self.schema.bump();
        Ok(())
    }
    fn call(&self, _: &mut Execution, m: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        Err(not_implemented(self.schema.column_type, m).into())
    }
    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<Str>> {
        Ok(Some(type_name(ex, self.schema.column_type)))
    }
    fn boost_hash(&self) -> Option<i32> {
        Some(self.record.hash)
    }
    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        Some(other.as_any().downcast_ref::<Self>().is_some_and(|other| {
            Rc::ptr_eq(&self.schema, &other.schema) && self.record.id == other.record.id
        }))
    }
}
