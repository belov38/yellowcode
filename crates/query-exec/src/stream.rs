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

const AHEAD: usize = 64;

#[derive(Clone)]
pub struct ExecEnvOwned {
    pub collation: Arc<dyn Collation>,
    pub cancel: Arc<AtomicBool>,
    pub mem_limit: u64,
    pub max_number_limbs: u64,
}

impl ExecEnvOwned {
    pub fn env(&self) -> ExecEnv<'_> {
        ExecEnv {
            collation: self.collation.as_ref(),
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
    rx: Option<Receiver<Result<Row, ExecError>>>,
    worker: Option<JoinHandle<()>>,
    cancel: Arc<AtomicBool>,
    ended: bool,
    _owned: PhantomData<&'a ()>,
}

impl RowStream<'_> {
    pub fn columns(&self) -> &[ColumnInfo] {
        &self.columns
    }
}

impl Iterator for RowStream<'_> {
    type Item = Result<Row, ExecError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.ended {
            return None;
        }
        let item = self.rx.as_ref().and_then(|rx| rx.recv().ok());
        if !matches!(item, Some(Ok(_))) {
            self.ended = true;
        }
        item
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
    tx: &SyncSender<Result<Row, ExecError>>,
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
    env.stop = Some(stop);
    let plan = plan_with(stmt, &src, PlanOptions::default())?;
    let mut op = build(&plan);
    let mut cx = Cx::new(&env, &src);
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
            if tx.send(Ok(row)).is_err() {
                break;
            }
        }
        return Ok(());
    }
    while let Some(mut row) = op.next(&mut cx)? {
        row.truncate(width);
        if tx.send(Ok(row)).is_err() {
            op.close(&mut cx);
            break;
        }
    }
    Ok(())
}

pub fn stream(
    stmt: Arc<StmtL>,
    src: Box<dyn ReadTxn + Send>,
    map: Arc<StorageMap>,
    env: ExecEnvOwned,
) -> Result<RowStream<'static>, ExecError> {
    if stmt.totals.is_some() {
        return Err(ExecError::Later {
            what: "ИТОГИ в потоковой выборке",
            stage: "Q4",
        });
    }
    let (tx, rx) = sync_channel::<Result<Row, ExecError>>(AHEAD);
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
                run(&stmt, &*src, &map, &env, &worker_own, &tx)
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
    })
}
