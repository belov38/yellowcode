#![deny(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

mod events;
mod facts;
mod gaps;
mod hasher;
mod literals;
mod memory;
mod notice;
mod panic;
mod paths;
mod redact;
mod render;
mod ring;
mod scan;
mod sink;
mod tempdb;
mod trace;
mod utc;
mod value;
mod vocabulary;

pub use events::{
    BakHeader, CommandShape, ConfigStats, InterruptKind, Outcome, Refusal, RunShape, StepRecord,
};
pub use facts::ServerFacts;
pub use hasher::Salt;
pub use literals::LiteralKind;
pub use memory::peak_resident_bytes;
pub use notice::{failure_written, not_written, trace_written};
pub use panic::install_panic_hook;
pub use paths::{PathKind, classify as classify_path};
pub use redact::{Redaction, Redactor};
pub use ring::{Ring, RingLimits};
pub use scan::{
    HEAD_MESSAGES, MAX_MESSAGES, MAX_TEXT_CHARS, MessageSource, RawMessage, ScanSummary, StderrScan,
};
pub use sink::{Destination, WriteFailure, io_kind_tag};
pub use tempdb::TempDbName;
pub use trace::{Finished, Limits, Rendered, Scope, Trace};
pub use value::{Event, Hashed, Redacted, Value};
