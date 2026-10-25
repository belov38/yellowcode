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

mod cell;
pub mod data;
mod explain;
mod job;
pub mod managed;
pub mod managed_batch;
pub mod managed_results;
pub mod managed_rows;
mod open;
mod params;
mod refs;
mod run;
mod table;

pub use cell::{Cell, CellKind, Column, RefOut};
pub use data::{
    Card, DataError, DataFilters, DataIndexes, DataPage, DataPageRequest, DataTable, DroppedTable,
    PeriodAccess, TabularPart, default_limit,
};
pub use explain::{EXPLAIN_BUSY, EXPLAIN_TIMEOUT, Explain, ExplainFail, NOTE_IR_L, NOTE_LAST_ONLY};
pub use job::{
    Job, JobState, JobStatus, Jobs, MAX_RUNNING, Page, RETAIN_BUDGET, StateName, TOO_MANY_RUNNING,
};
pub use md_model::LoadProfile;
pub use open::{Console, Info};
pub use params::{Choice, MAX_CHOICES, ParamInfoOut, ParamKind};
pub use refs::{MAX_REFS_PER_KIND, NESTED_LATER, NestedOut, Presented, PresentedCell, Source};
pub use run::{
    ErrorOut, Lang, MAX_BYTES, MAX_ROWS, Mode, ParamIn, Phase, Report, Rows, RunRefusal,
    RunRequest, Stmt,
};
pub use table::{
    AvailableIndex, FILTER_SCAN_ROWS, IndexField, IndexOrder, IndexRange, RowFilter, TableAccess,
    TableIndexRequest, TablePage, TableReadError,
};
