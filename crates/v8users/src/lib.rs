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

pub mod card;
pub mod data;
mod error;
mod layout;
pub use layout::MAX_DATA_BYTES;
pub mod password;
pub mod roles;
pub mod secret;
#[cfg(feature = "testdata")]
pub mod testdata;
pub mod user;

pub use bsl_prim::BslDate;
pub use error::Error;

pub type Uuid16 = brace::Guid;
pub use data::new_key;
