use super::*;
use crate::collections::columns::session as schema;
use bsl_vm::{Exception, SessionExport, SessionImports, SessionObject};
use std::sync::Arc;

impl bsl_vm::SessionBacking for TreeState {
    fn session_edges(&self, visit: &mut dyn bsl_vm::SessionEdges) -> bool {
        let columns: bsl_vm::ObjRef = self.columns.clone();
        visit.object(&columns)
            && self
                .arena
                .borrow()
                .iter()
                .flatten()
                .all(|record| visit.backing(record.clone()))
    }
    fn session_clear(&self) {
        self.arena.borrow_mut().clear();
    }
}
impl bsl_vm::SessionBacking for Node {
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
    roots: Vec<usize>,
    arena: usize,
    revision: u64,
    hash: i32,
    rows_hash: i32,
}
struct Record {
    id: usize,
    parent: Option<usize>,
    level: i64,
    active: bool,
    children: Vec<usize>,
    fields: Vec<u32>,
    hash: i32,
    rows_hash: i32,
}
struct Snapshot {
    state: Arc<State>,
    record: Option<Arc<Record>>,
    kind: u8,
}
fn state(state: &Rc<TreeState>, out: &mut dyn SessionExport) -> Result<Arc<State>, Exception> {
    out.backing(
        std::any::TypeId::of::<State>(),
        Rc::as_ptr(state) as usize,
        &mut |out| {
            out.reserve(
                (bsl_vm::session_shared_bytes::<State>() as usize
                    + state.roots.borrow().len() * std::mem::size_of::<usize>())
                    as u64,
            )?;
            Ok(Arc::new(State {
                columns: schema::capture(&state.columns, out)?,
                roots: crate::session::copy(&state.roots.borrow())
                    .map_err(|_| out.memory_error())?,
                arena: state.arena.borrow().len(),
                revision: state.revision.get(),
                hash: state.hash,
                rows_hash: state.rows_hash,
            }))
        },
    )?
    .downcast::<State>()
    .map_err(|_| out.memory_error())
}
fn record(record: &Rc<Node>, out: &mut dyn SessionExport) -> Result<Arc<Record>, Exception> {
    out.backing(
        std::any::TypeId::of::<Record>(),
        Rc::as_ptr(record) as usize,
        &mut |out| {
            out.reserve(
                (bsl_vm::session_shared_bytes::<Record>() as usize
                    + record.children.borrow().len() * std::mem::size_of::<usize>()
                    + record.cells.borrow().0.len() * std::mem::size_of::<u32>())
                    as u64,
            )?;
            Ok(Arc::new(Record {
                id: record.id,
                parent: record.parent,
                level: record.level,
                active: record.active.get(),
                children: crate::session::copy(&record.children.borrow())
                    .map_err(|_| out.memory_error())?,
                fields: {
                    let cells = record.cells.borrow();
                    let mut fields = Vec::new();
                    fields
                        .try_reserve_exact(cells.0.len())
                        .map_err(|_| out.memory_error())?;
                    fields.extend(cells.0.iter().map(|(id, _)| *id));
                    fields
                },
                hash: record.hash,
                rows_hash: record.rows_hash,
            }))
        },
    )?
    .downcast::<Record>()
    .map_err(|_| out.memory_error())
}
pub(super) fn capture(
    owner: &Rc<TreeState>,
    retained: Option<&Rc<Node>>,
    kind: u8,
    out: &mut dyn SessionExport,
) -> Result<Box<dyn SessionObject>, Exception> {
    out.reserve((std::mem::size_of::<Snapshot>() + 64) as u64)?;
    let saved = state(owner, out)?;
    let node = retained.map(|r| record(r, out)).transpose()?;
    if kind == 0 {
        out.resource(Rc::as_ptr(&owner.columns) as usize, owner.columns.clone())?;
        for record in owner.arena.borrow().iter().flatten() {
            out.reserve(64)?;
            out.resource(
                Rc::as_ptr(record) as usize,
                Rc::new(TreeRow {
                    state: owner.clone(),
                    record: record.clone(),
                }),
            )?;
        }
    } else {
        out.resource(
            Rc::as_ptr(owner) as usize,
            Rc::new(ValueTree {
                state: owner.clone(),
            }),
        )?;
        if let Some(record) = retained {
            if kind == 1 {
                for (_, value) in &record.cells.borrow().0 {
                    out.value(value)?;
                }
            } else {
                out.resource(
                    Rc::as_ptr(record) as usize,
                    Rc::new(TreeRow {
                        state: owner.clone(),
                        record: record.clone(),
                    }),
                )?;
            }
        }
    }
    Ok(Box::new(Snapshot {
        state: saved,
        record: node,
        kind,
    }))
}
impl State {
    fn create(
        self: &Arc<Self>,
        ex: &mut Execution,
        imports: &mut SessionImports,
    ) -> Result<Rc<TreeState>, Exception> {
        if let Some(value) = imports.get(Arc::as_ptr(self) as usize) {
            return Ok(value);
        }
        let columns = self.columns.create(ex, imports)?;
        imports.backing(ex, Arc::as_ptr(self) as usize, |ex| {
            let mut arena = Vec::new();
            arena
                .try_reserve_exact(self.arena)
                .map_err(|_| ex.out_of_memory())?;
            arena.resize_with(self.arena, || None);
            let state = Rc::new(TreeState {
                columns,
                arena: RefCell::new(arena),
                roots: RefCell::new(
                    crate::session::copy(&self.roots).map_err(|_| ex.out_of_memory())?,
                ),
                revision: Cell::new(self.revision),
                records_bytes: Cell::new(0),
                hash: self.hash,
                rows_hash: self.rows_hash,
                producer_text_slack: Cell::new(None),
            });
            let observer: Rc<dyn SchemaRows> = state.clone();
            state.columns.attach_rows(Rc::downgrade(&observer));
            Ok(state)
        })
    }
}
impl Record {
    fn create(
        self: &Arc<Self>,
        ex: &mut Execution,
        imports: &mut SessionImports,
    ) -> Result<Rc<Node>, Exception> {
        imports.backing(ex, Arc::as_ptr(self) as usize, |ex| {
            Ok(Rc::new(Node {
                id: self.id,
                parent: self.parent,
                level: self.level,
                active: Cell::new(self.active),
                children: RefCell::new(
                    crate::session::copy(&self.children).map_err(|_| ex.out_of_memory())?,
                ),
                cells: RefCell::new(Cells(Vec::new())),
                bytes: Cell::new(0),
                hash: self.hash,
                rows_hash: self.rows_hash,
            }))
        })
    }
}
impl SessionObject for Snapshot {
    fn retained_memory(&self, visit: &mut dyn bsl_vm::RetainedMemory) -> Result<(), Exception> {
        if visit.allocation(
            Arc::as_ptr(&self.state) as usize,
            (bsl_vm::session_shared_bytes::<State>() as usize
                + self.state.roots.capacity() * std::mem::size_of::<usize>()) as u64,
        )? {
            self.state.columns.retained(visit)?;
        }
        if let Some(r) = &self.record {
            visit.allocation(
                Arc::as_ptr(r) as usize,
                (bsl_vm::session_shared_bytes::<Record>() as usize
                    + r.fields.capacity() * std::mem::size_of::<u32>()
                    + r.children.capacity() * std::mem::size_of::<usize>()) as u64,
            )?;
        }
        Ok(())
    }
    fn create(&self, ex: &mut Execution, imports: &mut SessionImports) -> Result<Value, Exception> {
        let state = self.state.create(ex, imports)?;
        let record = self
            .record
            .as_ref()
            .map(|r| r.create(ex, imports))
            .transpose()?;
        Ok(match (self.kind, record) {
            (1, Some(record)) => Value::Object(Rc::new(TreeRow { state, record })),
            (2, parent) => Value::Object(Rc::new(TreeRows { state, parent })),
            _ => Value::Object(Rc::new(ValueTree { state })),
        })
    }
    fn fill(
        &self,
        ex: &mut Execution,
        shell: &Value,
        children: Vec<Value>,
    ) -> Result<(), Exception> {
        let object = shell.as_object().ok_or_else(|| ex.out_of_memory())?;
        if let Some(tree) = object.as_any().downcast_ref::<ValueTree>() {
            let mut arena = tree.state.arena.borrow_mut();
            for value in children.into_iter().skip(1) {
                let row = value
                    .as_object()
                    .and_then(|o| o.as_any().downcast_ref::<TreeRow>())
                    .ok_or_else(|| ex.out_of_memory())?;
                let slot = arena
                    .get_mut(row.record.id)
                    .ok_or_else(|| ex.out_of_memory())?;
                *slot = Some(row.record.clone());
            }
            tree.state
                .records_bytes
                .set(arena.iter().flatten().map(|r| r.bytes.get()).sum());
        } else if let (Some(row), Some(record)) =
            (object.as_any().downcast_ref::<TreeRow>(), &self.record)
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
            row.record.bytes.set(row.record.bytes());
            row.state.records_bytes.set(
                row.state
                    .arena
                    .borrow()
                    .iter()
                    .flatten()
                    .map(|r| r.bytes.get())
                    .sum(),
            );
        }
        Ok(())
    }
    fn clear(&self, shell: &Value) {
        if let Some(object) = shell.as_object() {
            if let Some(tree) = object.as_any().downcast_ref::<ValueTree>() {
                tree.state.arena.borrow_mut().clear();
            }
            if let Some(row) = object.as_any().downcast_ref::<TreeRow>() {
                row.record.cells.borrow_mut().0.clear();
            }
        }
    }
    fn restore_bytes(&self, children: usize) -> u64 {
        2048 + children as u64 * 256
            + (self.state.arena + self.state.roots.len()) as u64 * 16
            + self
                .record
                .as_ref()
                .map_or(0, |r| (r.children.len() + r.fields.len()) as u64 * 64)
    }
}
