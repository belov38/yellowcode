#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

pub mod available;
mod canon;
pub mod error;
mod expand;
mod from_ast;
pub mod literal;
pub mod meaning;
pub mod model;
mod names;
mod open;
pub mod ops;
mod pretty;
mod refuse;
mod shape;
mod spell;
mod to_ast;
pub mod view;

pub use error::{OpenError, Shape};
pub use from_ast::parse;
pub use model::*;
pub use open::{Ctx, Opened, open};
pub use pretty::pretty;
pub use to_ast::{Conventions, to_ast};

mod totals;

mod periods;
