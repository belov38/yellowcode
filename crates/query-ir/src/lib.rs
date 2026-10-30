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

pub type CompileError = query_syntax::QueryError;

mod managed;
pub mod worker_analysis;
pub use worker_analysis::{
    WorkerAnalysis, WorkerArgument, WorkerDiagnostic, WorkerFailure, WorkerSources, WorkerStatement,
};
pub mod worker_build;
pub use worker_build::{WorkerBuild, WorkerBuildFailure, WorkerBuilt};
pub mod types;
pub use types::*;
pub mod fold;
pub mod schema;
pub use schema::{
    Complement, FieldRole, Only, PeriodField, Periodicity, QField, QFieldId, QTable, QTableId,
    QuerySchema, RefInfo, SchemaError, Side, StdRole, StorageBinding, StorageLookup, TableKind,
    TabularKey, ValuePath, VtKind, VtParam, VtParamKind,
};
pub mod convert;
pub mod managed_value;
pub mod value;
pub use value::{NestedResult, QBytes, QShared, QStr, QValue, RefId};
pub mod params;
pub use params::{ManagedParamTypes, NoParams, ParamConst, ParamInfo, ParamShape, ParamTypes};
pub mod temp;
pub use temp::{ManagedTempSchemas, NoTempTables, TempColumn, TempMap, TempSchema, TempSchemas};
pub mod ir;
pub mod sema;
pub mod tir;
pub mod totals;
pub use sema::{
    AnalyzeMode, AnalyzeOptions, ColumnInfo, CompiledStmt, TempOut, analyze_batch_static,
    analyze_stmt, find_parameters, query_column_types,
};
pub mod compile;
pub use compile::{CompileRefusal, compile_batch, compile_batch_public};
pub mod bind;
pub use bind::{
    BindCtx, BindHost, ParamValue, ParamValues, ValueRows, ValueSet, bind, ref_id, uuid_of,
};
pub mod lower;
pub use lower::{
    AggL, ArithDec, Dec, HierSeeds, HierTable, NestedL, NestedRowsL, Opd, PExpr, PresentTarget,
    RelL, SizeKind, StmtL,
};
pub mod access;
pub mod build;
pub use access::{AccessPredicate, compile_access_condition};
pub mod exec_error;
pub use exec_error::{ExecError, NativeError, RuntimeError};
pub mod host;
pub use host::{
    Access, AccessPolicy, FieldSet, HostError, PredefinedName, PresentationInput, SeparatorFilter,
    SessionValues, SourceTable, TablePart, TxnId, UserRef,
};

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
        hit_n(1);
    }

    #[inline]
    pub fn hit_n(n: usize) {
        WORK.with(|w| w.set(w.get().wrapping_add(n as u64)));
    }
}
