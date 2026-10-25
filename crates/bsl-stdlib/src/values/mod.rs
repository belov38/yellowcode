mod binary;
pub(crate) mod period;
pub mod placeholder;
mod uuid;
pub(crate) mod xml_type;

pub(crate) use binary::{binary_data_arc, binary_data_bytes_direct, new_binary_data_arc};
pub use binary::{binary_data_bytes, new_binary_data};
pub use uuid::{new_uuid, parse_uuid, uuid_bytes, uuid_string};
pub(crate) use xml_type::{
    from_xml_type, new_xml_data_type, type_from_xml, xml_type, xml_type_of, xml_type_of_value,
};

use crate::collections::types;
use crate::serial::codec::ValueCodec;
use bsl_vm::{Registry, TypeImpl, Value};
use platform_catalog::TypeIdx;
use std::sync::Arc;

pub(crate) fn codecs() -> Vec<(TypeIdx, Arc<dyn ValueCodec>)> {
    let mut v = vec![
        (
            types().binary,
            Arc::new(binary::BinaryCodec) as Arc<dyn ValueCodec>,
        ),
        (
            types().uuid,
            Arc::new(uuid::UuidCodec) as Arc<dyn ValueCodec>,
        ),
    ];
    v.extend(period::codecs());
    v
}

pub(crate) fn register(reg: &mut Registry) {
    reg.type_impl(
        types().binary,
        TypeImpl {
            ctor: Some(Arc::new(binary::ctor)),
        },
    );
    reg.type_impl(
        types().uuid,
        TypeImpl {
            ctor: Some(Arc::new(uuid::ctor)),
        },
    );
    xml_type::register(reg);
    period::register(reg);
}

pub(crate) fn is_empty_value(v: &Value) -> bool {
    uuid_bytes(v).is_some_and(|b| b == [0; 16])
        || binary_data_bytes(v).is_some_and(<[u8]>::is_empty)
}
