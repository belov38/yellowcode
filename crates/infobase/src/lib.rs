#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

pub mod colval;
pub mod config;
pub mod create;
pub mod data;
pub mod dump;
pub mod error;
pub mod folder;
pub mod maybe_charged;
pub mod open;
pub mod params;
pub mod physical;
pub mod raw;
pub mod restore;
pub mod seed;
pub mod service;
pub mod store1cd;

pub use error::{Error, Result};
pub use maybe_charged::MaybeCharged;
pub use open::{Infobase, OpenMode, OpenOptions, SchemaCheck, StartupOwners, Warning};

pub use dump::{DumpReport, UsersLayout};
