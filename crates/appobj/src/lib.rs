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

mod access_right;
pub mod app;
mod current_language;
pub mod error;
pub mod errors;
mod extensions;
mod functional_option;
mod hierarchy;
mod host;
mod kind_managers;
pub mod managers;
mod meta;
pub mod model;
pub mod modules;
pub mod names;
pub mod objects;
mod platform_values;
mod predefined;
pub mod present;
mod queryhost;
mod read;
mod read_budget;
mod read_scope;
mod readtab;
mod recordset;
mod rights;
mod select;
mod serial;
pub mod session;
mod session_info;
mod settings_storage;
mod style;
pub mod types;
mod users;
pub mod values;
mod xml_strings;

pub use app::*;
pub use error::{AppError, Result};
pub use queryhost::CallQueryProvider;
pub use read_scope::ReadOnlyScope;
pub use rights::{Right, RightTarget};
pub use session::*;
