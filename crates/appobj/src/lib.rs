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
mod codes;
mod current_language;
mod data_lock;
pub mod error;
pub mod errors;
mod events;
mod exchange_params;
mod extensions;
mod functional_option;
mod hierarchy;
mod host;
mod kind_managers;
pub mod locks;
pub mod managers;
mod meta;
pub mod model;
pub mod modules;
pub mod names;
mod numerator;
pub mod objects;
mod objtab;
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
pub mod txn;
mod txn_globals;
pub mod types;
mod users;
pub mod values;
mod write;
mod write_checks;
mod write_hooks;
mod xml_strings;

pub use app::*;
pub use error::{AppError, Result};
pub use queryhost::CallQueryProvider;
pub use read_scope::{DataArea, ReadOnlyScope};
pub use rights::{Right, RightTarget};
pub use session::*;
