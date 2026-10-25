#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]
#![forbid(unsafe_code)]

pub mod error;
pub use error::{LibError, MESSAGE_KEYS, msg};
pub mod calendar;
pub mod data;
pub mod format;
pub mod grammar;
pub mod locale;
pub mod nstr;
pub mod parse;
pub mod pattern;
pub mod period;
pub mod plural;
pub mod present;
pub mod tz;
pub mod words;
