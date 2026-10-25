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

pub mod binary;
mod guid;
mod node;
#[cfg(feature = "testing")]
#[doc(hidden)]
pub mod testing;
mod text;

pub use guid::Guid;
pub use node::Node;
pub use text::{
    Doc, Error, ErrorKind, MAX_DEPTH, ManagedParseError, Newline, base64_atom, is_json_entry,
    parse, parse_bytes, parse_bytes_budgeted, parse_doc, parse_doc_budgeted, parse_multi,
    roundtrips, to_compact, to_text, to_text_with, write_doc, write_doc_budgeted,
};
