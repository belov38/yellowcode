#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]
#![forbid(unsafe_code)]

mod binary;
pub mod bind;
pub mod collections;
mod convert;
pub mod enums;
mod fs;
mod globals;
mod json;
pub mod present;
pub mod serial;
mod sort;
mod symbols;
mod text;
pub mod typedesc;
mod ui;
pub mod values;
mod xdto;
mod xml;

pub use binary::{binary_buffer_bytes, new_binary_buffer, stream_bytes};
pub use collections::{
    array_items, map_pairs, map_put, new_array, new_fixed_map, new_map, new_structure,
    new_value_list, structure_fields, structure_get, value_list_items,
};
pub use fs::MemFs;
pub use globals::data::encoding_from_value;
pub use serial::codec::{InternalReader, InternalWriter, ValueCodec, register_codec};
pub use serial::storage::{new_value_storage, value_storage_bytes, value_storage_from_bytes};
pub use serial::xmlvalue::{from_xml_string, to_xml_string};
pub use typedesc::{
    AllowedLength, AllowedSign, BinaryQualifiers, DateParts, DateQualifiers, NumberQualifiers,
    StringQualifiers, TypeDescription, cast_value, new_type_description, type_description_of,
};
pub use xdto::{
    ReservedXdtoName, ScalarProjection, XdtoCodec, XdtoCtx, XdtoData, register_xdto_codec,
};

use bsl_vm::Registry;
use std::sync::Arc;

pub fn register(reg: &mut Registry) {
    reg.set_presenter(Arc::new(present::present));
    convert::register(reg);
    enums::register(reg);
    symbols::register(reg);
    collections::register(reg);
    values::register(reg);
    typedesc::register(reg);
    binary::register(reg);
    globals::register(reg);
    json::register(reg);
    serial::register(reg);
    fs::register(reg);
    text::register(reg);
    ui::register(reg);
    xml::register(reg);
    xdto::register(reg);
    values::placeholder::register(reg);
}
