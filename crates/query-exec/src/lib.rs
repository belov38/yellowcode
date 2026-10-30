#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

pub mod coll;
pub mod eval;
pub mod key;
pub mod managed;
pub mod mem;
pub mod model;
pub mod num;
pub mod observe;
pub mod ops;
pub mod plan;
pub mod register;

pub mod access;
pub mod batch;
pub mod host;
mod proxy;
pub mod result;
pub mod source;
pub mod store;
pub mod stream;
pub mod temp;
pub mod totals;
pub mod walk;

mod exec;

pub use access::{AccessCtx, RowView, eval_access};
pub use batch::{
    BatchError, BatchMem, CompileModel, PUBLIC_MAX_COLUMNS, ParamSource, RunMode, StmtOutcome,
    StmtRun, count_column, explain_last, find_parameters, run_batch, run_batch_budgeted,
    run_batch_detailed,
};
pub use coll::{ExecCollation, NativeCollation};
pub use eval::text_of as value_text;
pub use exec::{execute, execute_temps, execute_temps_observed, execute_with, result_model};
pub use host::ExecHost;
pub use key::{Key, key_of};
pub use mem::MemSource;
pub use ops::present::not_found;
pub use query_ir::{AccessPolicy, SessionValues, TxnId};
pub use query_ir::{ExecError, RuntimeError};
pub use result::QueryResult;
pub use source::{DataSource, ExecEnv, IndexPrefixes, Row, RowIter, SKIP_MAX_AREAS};
pub use store::{StoreSource, execute_store};
pub use stream::{ExecEnvOwned, NativeOwned, RowStream, stream};
pub use temp::{TempIndex, TempTable, TempTableId, TempTables};

#[doc(hidden)]
pub mod probe {
    use std::cell::Cell;

    thread_local! {
        static WORK: Cell<u64> = const { Cell::new(0) };
    }

    pub fn reset() {
        WORK.with(|w| w.set(0));
    }

    pub fn get() -> u64 {
        WORK.with(Cell::get)
    }

    #[inline]
    pub fn hit() {
        WORK.with(|w| w.set(w.get().wrapping_add(1)));
    }
}
