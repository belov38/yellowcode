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

#[doc(hidden)]
pub mod codec;
mod error;
pub mod paths;
mod sink;
mod writer;

pub use codec::objects::{
    object_files, object_xml, recalculation_xml, subobject_xml, try_object_xml,
    try_recalculation_xml, try_subobject_xml,
};
pub use error::{Error, Result};
pub use md_model::XmlElem;
pub use sink::{DumpOptions, DumpReport, DumpSink, Part, PartStatus};
pub use writer::*;

#[doc(hidden)]
pub mod namespace_scope;
pub mod xml_origin;
