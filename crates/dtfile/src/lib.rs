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

pub mod binlist;
pub mod container;
pub mod elements;
pub mod error;
pub mod layout;
pub mod reader;
#[doc(hidden)]
pub mod rows;
pub mod writer;

pub use elements::Source;
pub use error::{Error, Result};
pub use reader::{DtReader, Event, TableHead};
pub use writer::DtWriter;
