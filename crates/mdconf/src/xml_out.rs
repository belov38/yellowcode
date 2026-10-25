mod dump;

pub use dump::dump;
pub use mdconf_core::xml_out::{
    DUMP_VERSION, DumpOptions, DumpReport, DumpSink, NS_DECL, Part, PartStatus, XmlElem, XmlWriter,
    document, esc, esc_attr, object_files, object_xml, paths, recalculation_xml, subobject_xml,
};
