#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]
#![forbid(unsafe_code)]

pub mod b64;
pub mod case;
pub mod codec;
pub mod error;
pub mod hash;
pub mod hex;
pub mod json;
pub mod lines;
pub mod numfn;
pub mod sort;
pub mod strings;
pub mod template;
pub mod textio;
pub mod trim;
pub mod url;
pub mod xdto;
pub mod xml;

pub use error::{LibError, MESSAGE_KEYS, msg};
