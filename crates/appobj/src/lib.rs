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
mod aggregates;
pub mod app;
mod bgjobs;
mod changes;
mod codes;
mod commit_guard;
mod current_language;
mod data_lock;
mod delete;
pub mod error;
pub mod errors;
mod event_log;
mod events;
mod exchange_params;
mod extensions;
mod fo_version;
mod functional_option;
mod hierarchy;
mod history;
mod host;
mod journal;
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
mod picture_lib;
mod platform_values;
mod posting;
mod predefined;
pub mod present;
mod queryhost;
mod read;
mod read_budget;
mod read_scope;
mod readtab;
mod recordset;
mod regread;
mod rights;
mod sched;
mod select;
mod serial;
pub mod session;
mod session_info;
mod settings_storage;
mod style;
#[doc(hidden)]
pub mod totals;
pub mod txn;
mod txn_globals;
pub mod types;
mod user_message;
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
#[doc(hidden)]
pub use regread::QueryingMethod;
pub use rights::{Right, RightTarget};
pub use session::*;
