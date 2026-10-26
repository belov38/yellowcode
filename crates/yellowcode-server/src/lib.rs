#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

pub mod process;
mod query;
pub mod runtime;
pub mod service;
pub mod users;

pub use query::{QueryConfig, QueryProcess};
