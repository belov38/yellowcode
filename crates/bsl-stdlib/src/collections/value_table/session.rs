use super::*;
use crate::collections::columns::session as schema;
use bsl_vm::{Exception, SessionExport, SessionImports, SessionObject};
use std::sync::Arc;

impl bsl_vm::SessionBacking for TableState {
    fn session_edges(&self, visit: &mut dyn bsl_vm::SessionEdges) -> bool {
        let columns: bsl_vm::ObjRef = self.columns.clone();
        let indexes: bsl_vm::ObjRef = self.indexes.clone();
        visit.object(&columns)
            && visit.object(&indexes)
            && self
                .rows
                .borrow()
                .iter()
                .all(|record| visit.backing(record.clone()))
    }
    fn session_clear(&self) {
        self.rows.borrow_mut().clear();
    }
}
impl bsl_vm::SessionBacking for RowRecord {
    fn session_edges(&self, visit: &mut dyn bsl_vm::SessionEdges) -> bool {
        self.cells
            .borrow()
            .0
            .iter()
            .all(|(_, value)| visit.value(value))
    }
    fn session_clear(&self) {
        self.cells.borrow_mut().0.clear();
    }
}

struct State {
    columns: Arc<schema::SchemaState>,
    indexes: Arc<indexes::session::State>,
    revision: u64,
    next: u64,
    hash: i32,
}
struct Record {
    id: u64,
    hash: i32,
    member: bool,
    fields: Vec<u32>,
}
struct Snapshot {
    state: Arc<State>,
    record: Option<Arc<Record>>,
}
fn state(state: &Rc<TableState>, out: &mut dyn SessionExport) -> Result<Arc<State>, Exception> {
    out.backing(
        std::any::TypeId::of::<State>(),
        Rc::as_ptr(state) as usize,
        &mut |out| {
            out.reserve((bsl_vm::session_shared_bytes::<State>() as usize) as u64)?;
            Ok(Arc::new(State {
                columns: schema::capture(&state.columns, out)?,
                indexes: indexes::session::capture(&state.indexes, out)?,
                revision: state.revision.get(),
                next: state.next_id.get(),
                hash: state.hash,
            }))
        },
    )?
    .downcast::<State>()
    .map_err(|_| out.memory_error())
}
pub(super) fn capture(
    owner: &Rc<TableState>,
    retained: Option<&Rc<RowRecord>>,
    out: &mut dyn SessionExport,
) -> Result<Box<dyn SessionObject>, Exception> {
    out.reserve((std::mem::size_of::<Snapshot>() + 64) as u64)?;
    let saved = state(owner, out)?;
    let record = if let Some(record) = retained {
        out.resource(
            Rc::as_ptr(owner) as usize,
            Rc::new(ValueTable {
                state: owner.clone(),
            }),
        )?;
        let cells = record.cells.borrow();
        let saved = out
            .backing(
                std::any::TypeId::of::<Record>(),
                Rc::as_ptr(record) as usize,
                &mut |out| {
                    out.reserve(
                        (bsl_vm::session_shared_bytes::<Record>() as usize
                            + cells.0.len() * std::mem::size_of::<u32>())
                            as u64,
                    )?;
                    Ok(Arc::new(Record {
                        id: record.id,
                        hash: record.hash,
                        member: record.member.get(),
                        fields: {
                            let mut fields = Vec::new();
                            fields
                                .try_reserve_exact(cells.0.len())
                                .map_err(|_| out.memory_error())?;
                            fields.extend(cells.0.iter().map(|(id, _)| *id));
                            fields
                        },
                    }))
                },
            )?
            .downcast::<Record>()
            .map_err(|_| out.memory_error())?;
        for (_, value) in &cells.0 {
            out.value(value)?;
        }
        Some(saved)
    } else {
        out.resource(Rc::as_ptr(&owner.columns) as usize, owner.columns.clone())?;
        out.resource(Rc::as_ptr(&owner.indexes) as usize, owner.indexes.clone())?;
        for record in owner.rows.borrow().iter() {
            out.reserve(64)?;
            out.resource(
                Rc::as_ptr(record) as usize,
                Rc::new(TableRow {
                    state: owner.clone(),
                    record: record.clone(),
                }),
            )?;
        }
        None
    };
    Ok(Box::new(Snapshot {
        state: saved,
        record,
    }))
}
impl State {
    fn create(
        self: &Arc<Self>,
        ex: &mut Execution,
        imports: &mut SessionImports,
    ) -> Result<Rc<TableState>, Exception> {
        if let Some(value) = imports.get(Arc::as_ptr(self) as usize) {
            return Ok(value);
        }
        let columns = self.columns.create(ex, imports)?;
        let indexes = self.indexes.create(ex, imports)?;
        imports.backing(ex, Arc::as_ptr(self) as usize, |_| {
            let state = Rc::new(TableState {
                columns,
                indexes,
                rows: RefCell::new(Vec::new()),
                revision: Cell::new(self.revision),
                next_id: Cell::new(self.next),
                records_bytes: Cell::new(0),
                producer_capacity_extra: Cell::new(None),
                hash: self.hash,
            });
            let observer: Rc<dyn SchemaRows> = state.clone();
            state.columns.attach_rows(Rc::downgrade(&observer));
            Ok(state)
        })
    }
}
impl SessionObject for Snapshot {
    fn retained_memory(&self, visit: &mut dyn bsl_vm::RetainedMemory) -> Result<(), Exception> {
        if visit.allocation(
            Arc::as_ptr(&self.state) as usize,
            (bsl_vm::session_shared_bytes::<State>() as usize) as u64,
        )? {
            self.state.columns.retained(visit)?;
            self.state.indexes.retained(visit)?;
        }
        if let Some(r) = &self.record {
            visit.allocation(
                Arc::as_ptr(r) as usize,
                (bsl_vm::session_shared_bytes::<Record>() as usize
                    + r.fields.capacity() * std::mem::size_of::<u32>()) as u64,
            )?;
        }
        Ok(())
    }
    fn create(&self, ex: &mut Execution, imports: &mut SessionImports) -> Result<Value, Exception> {
        let state = self.state.create(ex, imports)?;
        Ok(if let Some(record) = &self.record {
            let record = imports.backing(ex, Arc::as_ptr(record) as usize, |_| {
                Ok(Rc::new(RowRecord {
                    id: record.id,
                    hash: record.hash,
                    member: Cell::new(record.member),
                    cells: RefCell::new(Cells(Vec::new())),
                    bytes: Cell::new(0),
                }))
            })?;
            Value::Object(Rc::new(TableRow { state, record }))
        } else {
            Value::Object(Rc::new(ValueTable { state }))
        })
    }
    fn fill(
        &self,
        ex: &mut Execution,
        shell: &Value,
        children: Vec<Value>,
    ) -> Result<(), Exception> {
        let object = shell.as_object().ok_or_else(|| ex.out_of_memory())?;
        if let Some(table) = object.as_any().downcast_ref::<ValueTable>() {
            let mut rows = table.state.rows.borrow_mut();
            rows.clear();
            rows.try_reserve_exact(children.len().saturating_sub(2))
                .map_err(|_| ex.out_of_memory())?;
            for value in children.into_iter().skip(2) {
                let row = value
                    .as_object()
                    .and_then(|o| o.as_any().downcast_ref::<TableRow>())
                    .ok_or_else(|| ex.out_of_memory())?;
                rows.push(row.record.clone());
            }
            table
                .state
                .records_bytes
                .set(rows.iter().map(|r| r.bytes.get()).sum());
        } else if let (Some(row), Some(record)) =
            (object.as_any().downcast_ref::<TableRow>(), &self.record)
        {
            let mut cells = row.record.cells.borrow_mut();
            cells.0.clear();
            cells
                .0
                .try_reserve_exact(record.fields.len())
                .map_err(|_| ex.out_of_memory())?;
            cells.0.extend(
                record
                    .fields
                    .iter()
                    .copied()
                    .zip(children.into_iter().skip(1)),
            );
            drop(cells);
            row.record.bytes.set(row.record.calculate_bytes());
            row.state
                .records_bytes
                .set(row.state.rows.borrow().iter().map(|r| r.bytes.get()).sum());
        }
        Ok(())
    }
    fn clear(&self, shell: &Value) {
        if let Some(object) = shell.as_object() {
            if let Some(table) = object.as_any().downcast_ref::<ValueTable>() {
                table.state.rows.borrow_mut().clear();
            }
            if let Some(row) = object.as_any().downcast_ref::<TableRow>() {
                drop(row.record.take_values());
            }
        }
    }
    fn restore_bytes(&self, children: usize) -> u64 {
        2048 + children as u64 * 256
    }
}
