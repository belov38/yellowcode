use super::*;
use crate::collections::columns::session as schema;
use bsl_vm::{Exception, SessionExport, SessionImports, SessionObject};
use std::sync::Arc;

impl bsl_vm::SessionBacking for IndexState {
    fn session_edges(&self, visit: &mut dyn bsl_vm::SessionEdges) -> bool {
        let columns: bsl_vm::ObjRef = self.columns.clone();
        visit.object(&columns)
            && self
                .entries
                .borrow()
                .iter()
                .all(|record| visit.backing(record.clone()))
    }
    fn session_clear(&self) {
        self.entries.borrow_mut().clear();
    }
}
impl bsl_vm::SessionBacking for Definition {
    fn session_edges(&self, visit: &mut dyn bsl_vm::SessionEdges) -> bool {
        self.columns
            .iter()
            .all(|column| visit.backing(column.clone()))
    }
}

struct Record {
    id: u64,
    hash: i32,
    member: bool,
    columns: Vec<Arc<schema::RecordState>>,
}
pub(in crate::collections::value_table) struct State {
    columns: Arc<schema::SchemaState>,
    entries: Vec<Arc<Record>>,
    revision: u64,
    next: u64,
    hash: i32,
}
struct Snapshot {
    state: Arc<State>,
    record: Option<Arc<Record>>,
}
fn record(record: &Rc<Definition>, out: &mut dyn SessionExport) -> Result<Arc<Record>, Exception> {
    out.backing(
        std::any::TypeId::of::<Record>(),
        Rc::as_ptr(record) as usize,
        &mut |out| {
            out.reserve(
                (bsl_vm::session_shared_bytes::<Record>() as usize
                    + record.columns.len() * std::mem::size_of::<usize>()) as u64,
            )?;
            let mut columns = Vec::new();
            columns
                .try_reserve_exact(record.columns.len())
                .map_err(|_| out.memory_error())?;
            for column in &record.columns {
                columns.push(schema::capture_record(column, out)?);
            }
            Ok(Arc::new(Record {
                id: record.id,
                hash: record.hash,
                member: record.member.get(),
                columns,
            }))
        },
    )?
    .downcast::<Record>()
    .map_err(|_| out.memory_error())
}
pub(in crate::collections::value_table) fn capture(
    indexes: &Indexes,
    out: &mut dyn SessionExport,
) -> Result<Arc<State>, Exception> {
    out.backing(
        std::any::TypeId::of::<State>(),
        Rc::as_ptr(&indexes.state) as usize,
        &mut |out| {
            let state = &indexes.state;
            let entries = state.entries.borrow();
            out.reserve(
                (bsl_vm::session_shared_bytes::<State>() as usize
                    + entries.len() * std::mem::size_of::<usize>()) as u64,
            )?;
            let mut saved = Vec::new();
            saved
                .try_reserve_exact(entries.len())
                .map_err(|_| out.memory_error())?;
            for entry in entries.iter() {
                saved.push(record(entry, out)?);
            }
            Ok(Arc::new(State {
                columns: schema::capture(&state.columns, out)?,
                entries: saved,
                revision: state.revision.get(),
                next: state.next_id.get(),
                hash: state.hash,
            }))
        },
    )?
    .downcast::<State>()
    .map_err(|_| out.memory_error())
}
impl Record {
    fn create(
        self: &Arc<Self>,
        ex: &mut Execution,
        imports: &mut SessionImports,
    ) -> Result<Rc<Definition>, Exception> {
        if let Some(value) = imports.get(Arc::as_ptr(self) as usize) {
            return Ok(value);
        }
        let mut columns = Vec::new();
        columns
            .try_reserve_exact(self.columns.len())
            .map_err(|_| ex.out_of_memory())?;
        for column in &self.columns {
            columns.push(column.create(ex, imports)?);
        }
        imports.backing(ex, Arc::as_ptr(self) as usize, |_| {
            Ok(Rc::new(Definition {
                id: self.id,
                hash: self.hash,
                member: Cell::new(self.member),
                columns,
            }))
        })
    }
    fn retained(self: &Arc<Self>, visit: &mut dyn bsl_vm::RetainedMemory) -> Result<(), Exception> {
        if visit.allocation(
            Arc::as_ptr(self) as usize,
            (bsl_vm::session_shared_bytes::<Self>() as usize
                + self.columns.capacity() * std::mem::size_of::<usize>()) as u64,
        )? {
            for column in &self.columns {
                column.retained(visit)?;
            }
        }
        Ok(())
    }
}
impl State {
    pub(in crate::collections::value_table) fn create(
        self: &Arc<Self>,
        ex: &mut Execution,
        imports: &mut SessionImports,
    ) -> Result<Rc<Indexes>, Exception> {
        if let Some(value) = imports.get(Arc::as_ptr(self) as usize) {
            return Ok(value);
        }
        let columns = self.columns.create(ex, imports)?;
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(self.entries.len())
            .map_err(|_| ex.out_of_memory())?;
        for record in &self.entries {
            entries.push(record.create(ex, imports)?);
        }
        imports.backing(ex, Arc::as_ptr(self) as usize, |_| {
            let bytes = entries.iter().fold(
                Indexes::allocation_bytes() + vector::<Rc<Definition>>(entries.capacity()),
                |sum, record| sum + record_bytes(record),
            );
            let state = Rc::new(IndexState {
                columns,
                entries: RefCell::new(entries),
                revision: Cell::new(self.revision),
                next_id: Cell::new(self.next),
                bytes: Cell::new(bytes),
                hash: self.hash,
            });
            let observer: Rc<dyn SchemaIndexes> = state.clone();
            state.columns.attach_indexes(Rc::downgrade(&observer));
            Ok(Rc::new(Indexes { state }))
        })
    }
    pub(in crate::collections::value_table) fn retained(
        self: &Arc<Self>,
        visit: &mut dyn bsl_vm::RetainedMemory,
    ) -> Result<(), Exception> {
        if visit.allocation(
            Arc::as_ptr(self) as usize,
            (bsl_vm::session_shared_bytes::<Self>() as usize
                + self.entries.capacity() * std::mem::size_of::<usize>()) as u64,
        )? {
            self.columns.retained(visit)?;
            for record in &self.entries {
                record.retained(visit)?;
            }
        }
        Ok(())
    }
}
pub(super) fn capture_object(
    indexes: &Indexes,
    retained: Option<&Rc<Definition>>,
    out: &mut dyn SessionExport,
) -> Result<Box<dyn SessionObject>, Exception> {
    out.reserve(std::mem::size_of::<Snapshot>() as u64)?;
    out.resource(
        Rc::as_ptr(&indexes.state.columns) as usize,
        indexes.state.columns.clone(),
    )?;
    for record in indexes.state.entries.borrow().iter().chain(retained) {
        for column in &record.columns {
            out.reserve(64)?;
            out.resource(
                Rc::as_ptr(column) as usize,
                schema::record_source(&indexes.state.columns, column.clone()),
            )?;
        }
    }
    Ok(Box::new(Snapshot {
        state: capture(indexes, out)?,
        record: retained.map(|r| record(r, out)).transpose()?,
    }))
}
impl SessionObject for Snapshot {
    fn retained_memory(&self, visit: &mut dyn bsl_vm::RetainedMemory) -> Result<(), Exception> {
        self.state.retained(visit)?;
        if let Some(r) = &self.record {
            r.retained(visit)?;
        }
        Ok(())
    }
    fn create(&self, ex: &mut Execution, imports: &mut SessionImports) -> Result<Value, Exception> {
        let indexes = self.state.create(ex, imports)?;
        Ok(if let Some(record) = &self.record {
            Value::Object(Rc::new(CollectionIndex {
                state: indexes.state.clone(),
                record: record.create(ex, imports)?,
            }))
        } else {
            Value::Object(indexes)
        })
    }
    fn fill(&self, _: &mut Execution, _: &Value, _: Vec<Value>) -> Result<(), Exception> {
        Ok(())
    }
    fn clear(&self, _: &Value) {}
    fn restore_bytes(&self, children: usize) -> u64 {
        1024 + children as u64 * 256 + self.state.entries.len() as u64 * 256
    }
}
