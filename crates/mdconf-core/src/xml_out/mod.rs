#[doc(hidden)]
pub mod emit;
#[doc(hidden)]
pub mod semantic;

pub(crate) use md_xmlout::codec::values::{typed_elem, value_elem};
#[allow(unused_imports)]
#[doc(hidden)]
pub use md_xmlout::codec::values::{write_prop, write_type};
pub use md_xmlout::paths;
pub use md_xmlout::{
    DUMP_VERSION, DumpOptions, DumpReport, DumpSink, NS_DECL, Part, PartStatus, XmlElem, XmlWriter,
    document, esc, esc_attr, object_files, object_xml, recalculation_xml, subobject_xml,
};
