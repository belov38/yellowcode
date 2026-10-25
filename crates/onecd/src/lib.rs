#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

pub mod collate;
pub mod db;
pub mod error;
pub mod format;
pub mod index;
pub mod pager;
pub mod table;

pub(crate) mod blobstore;
pub(crate) mod catalog;
pub(crate) mod freelist;
pub(crate) mod fsinfo;
pub(crate) mod ifile;
pub(crate) mod lock;
mod page_numbers;
mod read_buffer;
pub(crate) mod verify;

#[cfg(any(test, feature = "testing"))]
#[doc(hidden)]
pub mod testing;

#[cfg(all(test, feature = "icu"))]
mod corpus_index;

#[cfg(feature = "icu")]
pub use collate::IcuCollate;
pub use collate::{Collate, Strength};
pub use db::{
    CreateOptions, Database, DbInfo, Locking, Mode, OpenOptions, ReadTxn, RowId, SyncMode, WriteTxn,
};
pub use error::{Constraint, Error, Location, LockedBy, NotFound, Result, Unsupported};
pub use format::descr::{FieldDescr, FieldType, IndexDescr, IndexField, TableDescr};
pub use format::layout::{FieldSlot, RecordLayout};
pub use format::record::{decode_row, encode_row};
pub use format::value::{BlobRef, BlobVal, DateTime, Numeric, RawDate, Row, Value};
pub use format::{Format, FormatInfo};
pub use index::{Direction, IndexEntry, IndexRead};
#[cfg(any(test, feature = "testing"))]
#[doc(hidden)]
pub use pager::FailingStorage;
pub use pager::{FileStorage, MemStorage, PageBuf, PageKind, PageRead, ReaderStats, Storage};
pub use table::{BlobReader, BlobWriter, RowRead, RowScratch, TableRead, TableWrite};
pub use verify::{VerifyReport, Violation};
