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

mod batch_memory;
mod detail_ties;
mod exec_host;
mod host;
mod managed_store;
mod open_model;
mod params;
mod repr;
mod run;
mod table;
mod walks;

pub use batch_memory::{BatchMemory, SchemaBounds};
pub use exec_host::TestHost;
pub use host::{TestBase, base, project_root, try_base, try_base_in};
pub use managed_store::{ManagedStoredOwnedRows, ManagedStoredRows, ManagedStoredTable};
pub use params::{CaseParams, ParamError, parse_params};
pub use repr::{
    measure_managed_repr, measure_native_repr, repr, repr_budgeted, repr_lang, repr_lang_budgeted,
    repr_managed_budgeted, repr_managed_lang_budgeted, write_managed_repr, write_native_repr,
};
pub use run::{
    CaseOutcome, ExplainRun, RunFailure, RunMode, StmtRun, Verdict, case_by_id, cases, cases_in,
    check, check_case, explain_text, has_walk_or_unload, known_deviations, outcome_json, run_case,
    run_case_movements, run_case_on, run_text, run_text_cancel, run_text_cancel_budgeted,
    run_text_with_plan,
};
pub use table::{
    ConditionKind, RecordField, StoredRows, StoredScanned, StoredTable, record_field_kind,
};
pub use walks::{record_type_text, walk_error};

pub use onecd::ReaderStats;
