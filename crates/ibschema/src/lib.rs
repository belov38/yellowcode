#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

pub mod columns;
pub mod dbnames;
pub mod error;
pub mod field_view;
pub mod fields;
pub mod logical;
mod managed;
pub mod names;
pub mod rules;
pub mod storage;
pub mod store;
pub mod totals;
pub mod values;

pub use error::{Error, Result};
pub use fields::FieldValue;
pub use onecd::{DateTime, Numeric};
pub use store::Row;

pub(crate) fn canonical_i64(s: &str) -> Option<i64> {
    let digits = s.strip_prefix('-').unwrap_or(s);
    let canonical = match digits.as_bytes() {
        [b'0'] => digits.len() == s.len(),
        [first, rest @ ..] => (b'1'..=b'9').contains(first) && rest.iter().all(u8::is_ascii_digit),
        [] => false,
    };
    if canonical { s.parse().ok() } else { None }
}
