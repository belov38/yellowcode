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

pub mod dict;
mod error;
pub mod export;
mod model;
pub mod reader;
pub mod record;
mod settings;
mod sink;
#[cfg(feature = "testdata")]
pub mod testdata;
mod writer;

pub use error::Error;
pub use model::{
    BslDate, Event, Level, Meta, MetaRef, SeparatorRef, SeparatorValue, TxStatus, TxnToken,
    UserRef, Uuid16, data, fmt_time,
};
pub use settings::{LogSettings, SplitPeriod, level_code};
pub use sink::{EventSink, FileSink, MemorySink, NullSink};
pub use writer::{Period, Writer, WriterOptions, lgp_name};
