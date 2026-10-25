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

pub mod forms;
pub mod startup;

pub(crate) use md_graphics::graph;
#[allow(unused_imports)]
pub(crate) use mdconf_core::codec::base::*;
#[allow(unused_imports)]
pub(crate) use mdconf_core::*;

#[doc(hidden)]
pub mod xml_in;
