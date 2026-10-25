#![deny(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

pub mod heap;
pub mod output;
pub mod plain;
pub mod platform;
pub mod service;
pub mod users;
pub mod viewer;

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;

pub const VERSION: &str = env!("YELLOWCODE_VERSION");
