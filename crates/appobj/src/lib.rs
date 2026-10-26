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

pub mod app;
pub mod error;
pub mod errors;
mod host;
pub mod managers;
mod meta;
pub mod model;
pub mod modules;
pub mod names;
pub mod objects;
mod predefined;
pub mod present;
mod queryhost;
mod read;
mod readtab;
mod rights;
mod select;
mod serial;
pub mod session;
mod session_info;
pub mod types;
pub mod values;

pub use app::*;
pub use error::{AppError, Result};
pub use queryhost::CallQueryProvider;
pub use rights::{Right, RightTarget};
pub use session::*;
