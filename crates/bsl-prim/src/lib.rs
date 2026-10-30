#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]
#![forbid(unsafe_code)]

mod limbs;

pub mod number;
pub use number::{DivByZero, Number, RoundMode};

pub mod text;
pub use text::{Str, fold_name};

pub mod compat;
pub use compat::{CompatFlags, CompatMode, UnsupportedCompat};

pub mod date;
pub use date::{BslDate, DateParts, days_in_month, is_leap_year};

pub mod collate;
pub mod locale;
#[cfg(feature = "icu")]
pub use collate::IcuCollation;
pub use collate::{Collation, CollationError, Strength};

pub mod boost_hash;
pub mod boostmap;
pub use boostmap::{BoostKey, BoostMap};

mod lowercase;
pub use lowercase::lowercase;
