mod dom;
mod reader;
mod schema;
mod writer;

pub(crate) use reader::{XmlReaderState, xml_reader};
pub(crate) use schema::{export_schema_set, schema_set_packages};
pub(crate) use writer::{XmlWriterState, check_chars, xml_writer};

use crate::bind::{key_error, msg, with_other};
use crate::enums::type_by_id;
use bsl_lib::xml::{XmlError, XmlWriteError};
use bsl_vm::{Execution, HostError, Registry};
use platform_catalog::{TypeIdx, messages::text_key};
use std::sync::OnceLock;

pub(crate) const NS_XS: &str = "http://www.w3.org/2001/XMLSchema";
pub(crate) const NS_DATA: &str = "http://v8.1c.ru/data";
pub(crate) const NS_XSI: &str = "http://www.w3.org/2001/XMLSchema-instance";

pub(crate) struct Types {
    pub writer: TypeIdx,
    pub settings: TypeIdx,
    pub reader: TypeIdx,
    pub node_type: TypeIdx,
    pub assignment: TypeIdx,
}

#[allow(
    clippy::panic,
    reason = "the embedded catalog has every type listed here"
)]
pub(crate) fn types() -> &'static Types {
    static T: OnceLock<Types> = OnceLock::new();
    T.get_or_init(|| {
        let t = |id: &str| type_by_id(id).unwrap_or_else(|| panic!("catalog type {id}"));
        Types {
            writer: t("XMLWriter"),
            settings: t("XMLWriterSettings"),
            reader: t("XMLReader"),
            node_type: t("XMLNodeType"),
            assignment: t("XMLTypeAssignment"),
        }
    })
}

pub(crate) fn xml_error(key: &'static str) -> HostError {
    with_other(HostError::key(key), false)
}

pub(crate) fn bad_char_error(pos: usize, text: &str) -> HostError {
    with_other(
        key_error(
            msg::XML_BAD_CHAR,
            vec![pos.to_string()],
            format!("\n{text}"),
        ),
        false,
    )
}

pub(crate) fn shown_text(units: &[u16]) -> String {
    char::decode_utf16(units.iter().copied())
        .map(|c| c.unwrap_or('?'))
        .collect()
}

pub(crate) fn write_error_in(e: XmlWriteError, text: &str) -> HostError {
    match e {
        XmlWriteError::InvalidChar { pos } => bad_char_error(pos.saturating_sub(1), text),
        e => write_error(e),
    }
}

pub(crate) fn write_error(e: XmlWriteError) -> HostError {
    match e {
        XmlWriteError::InvalidChar { pos } => bad_char_error(pos.saturating_sub(1), ""),
        XmlWriteError::Order => xml_error(msg::XML_WRITE_ORDER),
        XmlWriteError::UnboundPrefix => xml_error(msg::XML_NS_MAPPING),
    }
}

pub(crate) fn parse_error(ex: &Execution, e: &XmlError) -> HostError {
    let severity = text_key(
        if e.fatal {
            msg::XML_FATAL
        } else {
            msg::XML_ERROR
        },
        ex.lang(),
    )
    .unwrap_or_default();
    let suffix = format!(":  - [{},{}]\n{}\n{}\n", e.line, e.col, severity, e.message);
    with_other(key_error(msg::XML_PARSE, Vec::new(), suffix), false)
}

pub(crate) fn enum_presentation(
    t: TypeIdx,
    name_en: &str,
    lang: platform_catalog::messages::Lang,
) -> Option<bsl_prim::Str> {
    if t == crate::xdto::types().form {
        let form = match name_en {
            "Attribute" => bsl_lib::xdto::Form::Attribute,
            "Element" => bsl_lib::xdto::Form::Element,
            "Text" => bsl_lib::xdto::Form::Text,
            _ => return None,
        };
        return Some(bsl_prim::Str::from(crate::xdto::form_name(lang, form)));
    }
    if t != types().node_type {
        return None;
    }
    let tail = match name_en {
        "DocumentTypeDefinition" => "DOCUMENTTYPE".to_string(),
        n => n.to_ascii_uppercase(),
    };
    let key = format!("xml2:IDS_XMLNODETYPE_{tail}");
    text_key(&key, lang).map(bsl_prim::Str::from)
}

fn is_name_start(c: char) -> bool {
    matches!(c,
        '_' | 'A'..='Z' | 'a'..='z'
        | '\u{C0}'..='\u{D6}' | '\u{D8}'..='\u{F6}' | '\u{F8}'..='\u{2FF}'
        | '\u{370}'..='\u{37D}' | '\u{37F}'..='\u{1FFF}' | '\u{200C}'..='\u{200D}'
        | '\u{2070}'..='\u{218F}' | '\u{2C00}'..='\u{2FEF}' | '\u{3001}'..='\u{D7FF}'
        | '\u{F900}'..='\u{FDCF}' | '\u{FDF0}'..='\u{FFFD}' | '\u{10000}'..='\u{EFFFF}')
}

fn is_name_char(c: char) -> bool {
    is_name_start(c)
        || matches!(c, '-' | '.' | '0'..='9' | '\u{B7}' | '\u{300}'..='\u{36F}' | '\u{203F}'..='\u{2040}')
}

pub(crate) fn is_ncname(s: &str) -> bool {
    let mut it = s.chars();
    it.next().is_some_and(is_name_start) && it.all(is_name_char)
}

pub(crate) fn split_qname(s: &str) -> Option<(&str, &str)> {
    match s.split_once(':') {
        None => is_ncname(s).then_some(("", s)),
        Some((p, l)) => (is_ncname(p) && is_ncname(l)).then_some((p, l)),
    }
}

pub(crate) fn register(reg: &mut Registry) {
    writer::register(reg);
    reader::register(reg);
    dom::register(reg);
    schema::register(reg);
}
