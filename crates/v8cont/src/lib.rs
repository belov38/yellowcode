#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]
#![forbid(unsafe_code)]

pub mod dir;
mod error;
mod format;
mod layout;
mod managed;
mod read;
mod source;
#[cfg(feature = "testing")]
pub mod testing;
mod time;
mod write;

pub use error::{Error, Result};
pub use format::{FileHeader, Format, PageHeader, entry_header_bytes};
pub use layout::{Captured, ChainLayout, Layout, Patch, write_exact};
pub use managed::{ManagedContainer, ManagedReadError};
pub use read::{ChainReader, Container, EntryInfo, PageRec, detect, is_container};
pub use source::ReadAt;
pub use time::V8Time;
pub use write::{
    DataSource, EntrySpec, FORMAT16_PLACEHOLDER, NewEntry, StorageVer, VecData, WriteOptions,
    build_container, write_container,
};
