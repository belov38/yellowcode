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

mod literals;
mod session;
mod view;

pub use literals::{Kind, Literal, MAX_LITERAL, MAX_MODULE, find_all_literals, find_literals};
pub use query_builder::ops::Op;
pub use query_builder::ops::QueryRef;
pub use query_ir::QuerySchema;
pub use query_syntax::ParseMode;
pub use session::{ErrorOut, MAX_UNDO, Session};
pub use view::{
    AvailableOut, FieldOut, GroupOut, MAX_ROWS, MAX_TEXT, ParamOut, SourceFieldsOut, StubOut,
    TableOut, View,
};
