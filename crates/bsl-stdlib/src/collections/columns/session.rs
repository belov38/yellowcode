use super::*;
use bsl_vm::{Exception, SessionExport, SessionImports, SessionObject};
use std::sync::Arc;

impl bsl_vm::SessionBacking for Schema {
    fn session_edges(&self, visit: &mut dyn bsl_vm::SessionEdges) -> bool {
        self.entries
            .borrow()
            .iter()
            .all(|record| visit.backing(record.clone()))
    }
    fn session_clear(&self) {
        self.entries.borrow_mut().clear();
    }
}
impl bsl_vm::SessionBacking for ColumnRecord {
    fn session_edges(&self, visit: &mut dyn bsl_vm::SessionEdges) -> bool {
        visit.value(&self.data.borrow().value_type)
    }
    fn session_clear(&self) {
        self.data.borrow_mut().value_type = Value::Undefined;
    }
}

pub(in crate::collections) struct SchemaState {
    collection: TypeIdx,
    column: TypeIdx,
    next: u32,
    revision: u64,
    hash: i32,
}
pub(in crate::collections) struct RecordState {
    id: u32,
    hash: i32,
    width: i64,
    folded_bytes: u64,
}
struct Snapshot {
    schema: Arc<SchemaState>,
    record: Option<Arc<RecordState>>,
}

pub(in crate::collections) fn record_source(
    columns: &Columns,
    record: Rc<ColumnRecord>,
) -> Rc<dyn HostObject> {
    Rc::new(Column {
        schema: columns.schema.clone(),
        record,
    })
}
pub(in crate::collections) fn capture_record(
    record: &Rc<ColumnRecord>,
    out: &mut dyn SessionExport,
) -> Result<Arc<RecordState>, Exception> {
    let data = record.data.borrow();
    out.backing(
        std::any::TypeId::of::<RecordState>(),
        Rc::as_ptr(record) as usize,
        &mut |out| {
            out.reserve((bsl_vm::session_shared_bytes::<RecordState>() as usize) as u64)?;
            Ok(Arc::new(RecordState {
                id: record.id,
                hash: record.hash,
                width: data.width,
                folded_bytes: (data.name.len() as u64).saturating_mul(12),
            }))
        },
    )?
    .downcast::<RecordState>()
    .map_err(|_| out.memory_error())
}
impl RecordState {
    pub(in crate::collections) fn create(
        self: &Arc<Self>,
        ex: &mut Execution,
        imports: &mut SessionImports,
    ) -> Result<Rc<ColumnRecord>, Exception> {
        imports.backing(ex, Arc::as_ptr(self) as usize, |_| {
            Ok(Rc::new(ColumnRecord {
                id: self.id,
                hash: self.hash,
                data: RefCell::new(ColumnData {
                    name: Str::new(),
                    folded: String::new(),
                    title: Str::new(),
                    width: self.width,
                    value_type: Value::Undefined,
                }),
            }))
        })
    }
    pub(in crate::collections) fn retained(
        self: &Arc<Self>,
        visit: &mut dyn bsl_vm::RetainedMemory,
    ) -> Result<(), Exception> {
        visit
            .allocation(
                Arc::as_ptr(self) as usize,
                (bsl_vm::session_shared_bytes::<Self>() as usize) as u64,
            )
            .map(|_| ())
    }
}

pub(in crate::collections) fn capture(
    columns: &Columns,
    out: &mut dyn SessionExport,
) -> Result<Arc<SchemaState>, Exception> {
    let schema = &columns.schema;
    let value = out.backing(
        std::any::TypeId::of::<SchemaState>(),
        Rc::as_ptr(schema) as usize,
        &mut |out| {
            out.reserve((bsl_vm::session_shared_bytes::<SchemaState>() as usize) as u64)?;
            Ok(Arc::new(SchemaState {
                collection: schema.collection_type,
                column: schema.column_type,
                next: schema.next_id.get(),
                revision: schema.revision.get(),
                hash: schema.hash,
            }))
        },
    )?;
    value
        .downcast::<SchemaState>()
        .map_err(|_| out.memory_error())
}
impl SchemaState {
    pub(in crate::collections) fn create(
        self: &Arc<Self>,
        ex: &mut Execution,
        imports: &mut SessionImports,
    ) -> Result<Rc<Columns>, Exception> {
        imports.backing(ex, Arc::as_ptr(self) as usize, |_| {
            Ok(Rc::new(Columns {
                schema: Rc::new(Schema {
                    entries: RefCell::new(Vec::new()),
                    next_id: Cell::new(self.next),
                    revision: Cell::new(self.revision),
                    collection_type: self.collection,
                    column_type: self.column,
                    hash: self.hash,
                    rows: RefCell::new(None),
                    indexes: RefCell::new(None),
                    rows_allocation: Cell::new(0),
                    indexes_allocation: Cell::new(0),
                }),
            }))
        })
    }
    pub(in crate::collections) fn retained(
        self: &Arc<Self>,
        visit: &mut dyn bsl_vm::RetainedMemory,
    ) -> Result<(), Exception> {
        visit
            .allocation(
                Arc::as_ptr(self) as usize,
                (bsl_vm::session_shared_bytes::<Self>() as usize) as u64,
            )
            .map(|_| ())
    }
}
pub(super) fn columns(
    columns: &Columns,
    out: &mut dyn SessionExport,
) -> Result<Box<dyn SessionObject>, Exception> {
    out.reserve(std::mem::size_of::<Snapshot>() as u64)?;
    let schema = capture(columns, out)?;
    for record in columns.schema.entries.borrow().iter() {
        out.reserve((bsl_vm::session_shared_bytes::<Column>() as usize) as u64)?;
        out.resource(
            Rc::as_ptr(record) as usize,
            Rc::new(Column {
                schema: columns.schema.clone(),
                record: record.clone(),
            }),
        )?;
    }
    Ok(Box::new(Snapshot {
        schema,
        record: None,
    }))
}
pub(super) fn column(
    column: &Column,
    out: &mut dyn SessionExport,
) -> Result<Box<dyn SessionObject>, Exception> {
    out.reserve(
        (std::mem::size_of::<Snapshot>() + bsl_vm::session_shared_bytes::<Columns>() as usize)
            as u64,
    )?;
    let owner = Rc::new(Columns {
        schema: column.schema.clone(),
    });
    let schema = capture(&owner, out)?;
    out.resource(Rc::as_ptr(&column.schema) as usize, owner)?;
    let record = &column.record;
    let data = record.data.borrow();
    let state = capture_record(record, out)?;
    out.value(&Value::String(data.name.clone()))?;
    out.value(&Value::String(data.title.clone()))?;
    out.value(&data.value_type)?;
    Ok(Box::new(Snapshot {
        schema,
        record: Some(state),
    }))
}
impl SessionObject for Snapshot {
    fn retained_memory(&self, visit: &mut dyn bsl_vm::RetainedMemory) -> Result<(), Exception> {
        self.schema.retained(visit)?;
        if let Some(record) = &self.record {
            visit.allocation(
                Arc::as_ptr(record) as usize,
                (bsl_vm::session_shared_bytes::<RecordState>() as usize) as u64,
            )?;
        }
        Ok(())
    }
    fn create(&self, ex: &mut Execution, imports: &mut SessionImports) -> Result<Value, Exception> {
        let columns = self.schema.create(ex, imports)?;
        if let Some(record) = &self.record {
            let record = record.create(ex, imports)?;
            Ok(Value::Object(Rc::new(Column {
                schema: columns.schema.clone(),
                record,
            })))
        } else {
            Ok(Value::Object(columns))
        }
    }
    fn fill(
        &self,
        ex: &mut Execution,
        shell: &Value,
        children: Vec<Value>,
    ) -> Result<(), Exception> {
        let object = shell.as_object().ok_or_else(|| ex.out_of_memory())?;
        if let Some(columns) = object.as_any().downcast_ref::<Columns>() {
            let mut entries = columns.schema.entries.borrow_mut();
            entries.clear();
            entries
                .try_reserve_exact(children.len())
                .map_err(|_| ex.out_of_memory())?;
            for value in children {
                let record = value
                    .as_object()
                    .and_then(|o| o.as_any().downcast_ref::<Column>())
                    .ok_or_else(|| ex.out_of_memory())?;
                entries.push(record.record.clone());
            }
        } else if let Some(column) = object.as_any().downcast_ref::<Column>() {
            let mut children = children.into_iter();
            children.next();
            let (Some(Value::String(name)), Some(Value::String(title)), Some(value_type)) =
                (children.next(), children.next(), children.next())
            else {
                return Err(ex.out_of_memory());
            };
            let mut data = column.record.data.borrow_mut();
            data.folded = fold_name(&name.to_string_lossy());
            data.name = name;
            data.title = title;
            data.value_type = value_type;
        }
        Ok(())
    }
    fn clear(&self, shell: &Value) {
        if let Some(columns) = shell
            .as_object()
            .and_then(|o| o.as_any().downcast_ref::<Columns>())
        {
            columns.schema.entries.borrow_mut().clear();
        }
        if let Some(column) = shell
            .as_object()
            .and_then(|o| o.as_any().downcast_ref::<Column>())
        {
            column.record.data.borrow_mut().value_type = Value::Undefined;
        }
    }
    fn restore_bytes(&self, children: usize) -> u64 {
        1024 + children as u64 * 128 + self.record.as_ref().map_or(0, |record| record.folded_bytes)
    }
}
