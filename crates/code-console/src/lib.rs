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

mod console;
mod job;
mod report;

pub use console::{BUSY, CodeConsole, ConsoleInfo, INTERNAL, SessionMode};
pub use job::{CodeJobState, CodeJobStatus, CodeJobs, KEEP, MAX_TEXT_BYTES};
pub use report::{CodeError, MessageOut, Outcome, RunReport, StackOut};

use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

pub(crate) fn millis(t0: Instant) -> u64 {
    u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX)
}
