use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread::JoinHandle;

use bsl_prim::Collation;
use ibschema::storage::StorageMap;
use ibschema::store::ReadTxn;
use query_ir::{ColumnInfo, ExecError, StmtL};
use query_syntax::INLINE_DEPTH;

use crate::ops::Cx;
use crate::plan::{PlanOptions, build, plan_with};
use crate::proxy::stack_size;
use crate::source::{ExecEnv, Row};
use crate::store::StoreSource;
use memory_budget::{Category, MemoryBudget, RequestMemory, Reservation};

struct StreamRow {
    row: Row,
    _paid: Option<Reservation>,
}

const AHEAD: usize = 64;

#[derive(Clone)]
pub struct ExecEnvOwned {
    pub collation: Arc<dyn Collation>,
    pub native: Option<NativeOwned>,
    pub cancel: Arc<AtomicBool>,
    pub mem_limit: u64,
    pub max_number_limbs: u64,
}

#[derive(Clone)]
pub struct NativeOwned {
    pub locale: &'static std::ffi::CStr,
    pub budget: memory_budget::MemoryBudget,
}

impl ExecEnvOwned {
    pub fn env(&self) -> ExecEnv<'_> {
        ExecEnv {
            collation: self.collation.as_ref(),
            native: self.native.as_ref().map(|n| crate::coll::NativeCollation {
                locale: n.locale,
                budget: &n.budget,
            }),
            cancel: &self.cancel,
            stop: None,
            mem_limit: self.mem_limit,
            max_number_limbs: self.max_number_limbs,
            memory: None,
            lease: None,
            evictor: None,
        }
    }
}

pub struct RowStream<'a> {
    columns: Vec<ColumnInfo>,
    rx: Option<Receiver<Result<StreamRow, ExecError>>>,
    worker: Option<JoinHandle<()>>,
    cancel: Arc<AtomicBool>,
    ended: bool,
    _owned: PhantomData<&'a ()>,
    current_payment: Option<Reservation>,
    _paid: Option<Reservation>,
}

impl RowStream<'_> {
    pub fn take_current_payment(&mut self) -> Option<Reservation> {
        self.current_payment.take()
    }

    pub fn columns(&self) -> &[ColumnInfo] {
        &self.columns
    }
}

impl Iterator for RowStream<'_> {
    type Item = Result<Row, ExecError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.current_payment = None;
        if self.ended {
            return None;
        }
        let item = self.rx.as_ref().and_then(|rx| rx.recv().ok());
        if !matches!(item, Some(Ok(_))) {
            self.ended = true;
        }
        item.map(|item| {
            item.map(|row| {
                self.current_payment = row._paid;
                row.row
            })
        })
    }
}

impl Drop for RowStream<'_> {
    fn drop(&mut self) {
        if !self.ended {
            self.cancel.store(true, Ordering::Relaxed);
        }
        self.rx = None;
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

fn run(
    stmt: &StmtL,
    txn: &dyn ReadTxn,
    map: &StorageMap,
    env: &ExecEnvOwned,
    stop: &AtomicBool,
    tx: &SyncSender<Result<StreamRow, ExecError>>,
    memory: Option<&MemoryBudget>,
) -> Result<(), ExecError> {
    if !stmt.present.is_empty() {
        return Err(ExecError::Later {
            what: "ПРЕДСТАВЛЕНИЕ в потоке строк",
            stage: "Q3",
        });
    }
    if stmt.auto_number.is_some() {
        return Err(ExecError::Later {
            what: "АВТОНОМЕРЗАПИСИ() в потоке строк",
            stage: "Q3",
        });
    }
    let src = StoreSource::new(txn, map);
    crate::register::check_slice_use(stmt, &src, None)?;
    let mut env = env.env();
    let request = memory.map(|budget| RequestMemory {
        budget: budget.clone(),
        row_limit: env.mem_limit,
    });
    let lease = memory
        .map(|budget| budget.try_reserve(Category::Execution, 0))
        .transpose()
        .map_err(ExecError::Memory)?;
    env.memory = request.as_ref();
    env.lease = lease.as_ref();
    env.stop = Some(stop);
    let plan = plan_with(stmt, &src, PlanOptions::default())?;
    let managed = crate::coll::open(&env)?;
    let mut op = build(&plan);
    let mut cx = Cx::new(&env, &src);
    cx.coll = crate::coll::route(&env, managed.as_ref());
    cx.week_start = stmt.week_start;
    let width = stmt.columns.len();
    if !stmt.nested.is_empty() {
        let mut rows = Vec::new();
        while let Some(mut row) = op.next(&mut cx)? {
            row.truncate(width);
            let release = crate::ops::rows_push_growth(&rows, &cx, rows.capacity())?;
            cx.mem.grow(crate::ops::row_cap_bytes(&row))?;
            rows.push(row);
            cx.mem.shrink(release);
        }
        op.close(&mut cx);
        drop(op);
        crate::ops::nested::stitch(stmt, &mut rows, &mut cx, None, PlanOptions::default())?;
        for row in rows {
            if !send_row(tx, row, memory)? {
                break;
            }
        }
        return Ok(());
    }
    while let Some(mut row) = op.next(&mut cx)? {
        row.truncate(width);
        if !send_row(tx, row, memory)? {
            op.close(&mut cx);
            break;
        }
    }
    Ok(())
}

fn send_row(
    tx: &SyncSender<Result<StreamRow, ExecError>>,
    row: Row,
    memory: Option<&MemoryBudget>,
) -> Result<bool, ExecError> {
    let paid = memory
        .map(|budget| budget.try_reserve(Category::Execution, crate::ops::row_cap_bytes(&row)))
        .transpose()
        .map_err(ExecError::Memory)?;
    Ok(tx.send(Ok(StreamRow { row, _paid: paid })).is_ok())
}

pub fn stream(
    stmt: Arc<StmtL>,
    src: Box<dyn ReadTxn + Send>,
    map: Arc<StorageMap>,
    env: ExecEnvOwned,
) -> Result<RowStream<'static>, ExecError> {
    stream_inner(stmt, src, map, env, None)
}

pub fn stream_admitted(
    stmt: Arc<StmtL>,
    src: Box<dyn ReadTxn + Send>,
    map: Arc<StorageMap>,
    env: ExecEnvOwned,
    memory: MemoryBudget,
) -> Result<RowStream<'static>, ExecError> {
    stream_inner(stmt, src, map, env, Some(memory))
}

fn stream_inner(
    stmt: Arc<StmtL>,
    src: Box<dyn ReadTxn + Send>,
    map: Arc<StorageMap>,
    env: ExecEnvOwned,
    memory: Option<MemoryBudget>,
) -> Result<RowStream<'static>, ExecError> {
    if stmt.totals.is_some() {
        return Err(ExecError::Later {
            what: "ИТОГИ в потоковой выборке",
            stage: "Q4",
        });
    }
    let paid = memory
        .as_ref()
        .map(|budget| {
            budget.try_reserve(
                Category::Execution,
                crate::model::columns_model(&stmt.columns)
                    .saturating_add(std::mem::size_of::<RowStream<'_>>() as u64),
            )
        })
        .transpose()
        .map_err(ExecError::Memory)?;
    let (tx, rx) =
        sync_channel::<Result<StreamRow, ExecError>>(if memory.is_some() { 0 } else { AHEAD });
    let columns = stmt.columns.clone();
    let own = Arc::new(AtomicBool::new(false));
    let worker_own = own.clone();
    let mut builder = std::thread::Builder::new().name("query-stream".to_string());
    if stmt.max_depth > INLINE_DEPTH {
        builder = builder.stack_size(stack_size(stmt.max_depth));
    }
    let worker = builder
        .spawn(move || {
            let done = catch_unwind(AssertUnwindSafe(|| {
                ibschema::store::with_key_cancel(&env.cancel, || {
                    run(&stmt, &*src, &map, &env, &worker_own, &tx, memory.as_ref())
                })
            }));
            let last = match done {
                Ok(Ok(())) => None,
                Ok(Err(e)) => Some(e),
                Err(_) => Some(ExecError::Fatal(
                    "stream: the executor thread panicked".to_string(),
                )),
            };
            if let Some(e) = last {
                let _ = tx.send(Err(e));
            }
        })
        .map_err(|e| ExecError::Fatal(format!("stream: cannot start the executor thread: {e}")))?;
    Ok(RowStream {
        columns,
        rx: Some(rx),
        worker: Some(worker),
        cancel: own,
        ended: false,
        _owned: PhantomData,
        current_payment: None,
        _paid: paid,
    })
}
