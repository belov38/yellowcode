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
mod managed_objects;
pub use managed_objects::{
    managed_configuration_xml, managed_object_xml, managed_recalculation_xml, managed_subobject_xml,
};
mod managed_writer;
pub use managed_writer::ManagedXmlWriter;
mod managed_cmdi;
pub use managed_cmdi::{
    managed_client_interface_xml, managed_command_interface_xml, managed_home_page_xml,
};
mod managed_style;
pub use managed_style::managed_style_xml;
mod managed_misc;
pub use managed_misc::{managed_content_xml, managed_schedule_xml};
mod managed_predefined;
pub use managed_predefined::managed_predefined_xml;
mod managed_rights;
pub use managed_rights::managed_rights_xml;
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

mod managed_picture;
pub use managed_picture::managed_picture_xml;

mod managed_help;
pub use managed_help::{managed_help_page, managed_help_xml};

mod managed_configdump;
pub use managed_configdump::{ManagedDumpNames, managed_configdump_xml};
