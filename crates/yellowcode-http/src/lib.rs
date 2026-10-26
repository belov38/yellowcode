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

mod conn;
mod guard;
pub mod runtime;
pub mod service;
pub mod transport;
pub mod viewer;

pub use conn::{DRAIN, DeadlineReader, close_gracefully};
pub use guard::{guard, text};
pub use transport::{
    HttpError, MAX_BODY, PAGE_CSP, Request, Response, percent_decode, read_json_request,
    read_request_with, write_response, write_response_with_allow,
};
