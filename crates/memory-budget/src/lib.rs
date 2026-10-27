#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]
#![deny(unsafe_op_in_unsafe_fn)]

pub mod ledger;
mod lock;
pub use ledger::*;
pub use lock::{Guard as LockGuard, Lock};

pub mod boxed;
pub use boxed::BudgetedBox;
pub mod buffer;
pub mod discovery;
pub mod policy;
#[doc(hidden)]
pub mod profile;
pub use buffer::{BudgetError, BudgetedText, BudgetedVec, BudgetedWriter};
pub mod shared;
pub use shared::{ErasedShared, Shared};
pub mod text_output;
pub use text_output::{TextCount, TextOutput};
pub mod charged;
pub use charged::Charged;
pub mod prepaid;
pub use prepaid::{Grant, Scratch, extend_within, resize_within};
pub mod context;
pub mod control;
pub use context::{
    RequestBudget, RequestMemory, ResourceCause, ResourceFailure, ResourcePhase, StructureLimit,
};
pub mod sealed_heap;
pub mod worker;
pub use worker::{OWNER_WORKER_STACK_BYTES, WorkerContext, with_current_worker};

#[derive(Debug)]
pub struct ResponseLimit;
impl std::fmt::Display for ResponseLimit {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        output.write_str("MEMORY_LIMIT: response")
    }
}
impl std::error::Error for ResponseLimit {}

#[derive(Debug)]
pub struct OutputClosed;
impl std::fmt::Display for OutputClosed {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        output.write_str("output pipe closed")
    }
}
impl std::error::Error for OutputClosed {}
#[derive(Debug)]
pub struct OutputTimedOut;
impl std::fmt::Display for OutputTimedOut {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        output.write_str("output write timed out")
    }
}
impl std::error::Error for OutputTimedOut {}
#[derive(Debug)]
pub struct OutputFailed;
impl std::fmt::Display for OutputFailed {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        output.write_str("output write failed")
    }
}
impl std::error::Error for OutputFailed {}
#[derive(Debug)]
pub struct OutputDisconnected;
impl std::fmt::Display for OutputDisconnected {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        output.write_str("output connection lost")
    }
}
impl std::error::Error for OutputDisconnected {}
