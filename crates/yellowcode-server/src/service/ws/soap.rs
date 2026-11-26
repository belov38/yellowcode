use super::{WsOperation, WsService};
use crate::service::wire::Unimplemented;
use bsl_lib::xml::{XmlAttr, XmlError, XmlEvent, XmlReader, decode_xml_bytes, xml_invalid_pos};

pub const SOAP11: &str = "http://schemas.xmlsoap.org/soap/envelope/";
pub const SOAP12: &str = "http://www.w3.org/2003/05/soap-envelope";
const XSI: &str = "http://www.w3.org/2001/XMLSchema-instance";
const XMLNS: &str = "http://www.w3.org/2000/xmlns/";
pub const CONTENT_TYPE: &str = "text/xml;charset=utf-8";
pub const CONTENT_TYPE_12: &str = "application/soap+xml;charset=utf-8";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Version {
    V11,
    V12,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Param {
    pub ns: String,
    pub name: String,
    pub nil: bool,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Envelope {
    pub version: Version,
    pub ns: String,
    pub operation: String,
    pub params: Vec<Param>,
}

const NOT_ENVELOPE: Unimplemented =
    Unimplemented("a request body that is not a SOAP envelope with one Body");
const NOT_XML: Unimplemented = Unimplemented("a SOAP request body that is not well-formed XML");
const XML_ERROR: Unimplemented = Unimplemented(
    "a SOAP request body that is not well-formed XML, other than an undefined element prefix cut short as the first element of the Body",
);
const OTHER_NODE: Unimplemented =
    Unimplemented("a comment, instruction or DOCTYPE in a SOAP envelope");
const ATTRIBUTE: Unimplemented =
    Unimplemented("an attribute other than a namespace declaration and xsi:nil in a SOAP call");

pub fn check_content_type(headers: &[(String, String)]) -> Result<Version, Unimplemented> {
    let Some(value) = headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case("Content-Type"))
        .map(|(_, v)| v.as_str())
    else {
        return Err(Unimplemented("a SOAP request without Content-Type"));
    };
    let mut parts = value.split(';');
    let media = parts.next().unwrap_or_default().trim();
    let version = if media.eq_ignore_ascii_case("text/xml") {
        Version::V11
    } else if media.eq_ignore_ascii_case("application/soap+xml") {
        Version::V12
    } else {
        return Err(Unimplemented(
            "a SOAP request with a Content-Type other than text/xml and application/soap+xml",
        ));
    };
    for part in parts {
        let (name, value) = part.split_once('=').unwrap_or((part, ""));
        if !name.trim().eq_ignore_ascii_case("charset") {
            return Err(Unimplemented(
                "a parameter of the Content-Type of a SOAP request other than charset",
            ));
        }
        if !value.trim().eq_ignore_ascii_case("utf-8") {
            return Err(Unimplemented(
                "a charset of a SOAP request other than utf-8",
            ));
        }
    }
    Ok(version)
}

pub fn content_type(version: Version) -> &'static str {
    match version {
        Version::V11 => CONTENT_TYPE,
        Version::V12 => CONTENT_TYPE_12,
    }
}

enum Failure {
    Refused(Unimplemented),
    Xml(XmlError),
    InBody(XmlError, Version),
}

impl From<Unimplemented> for Failure {
    fn from(refusal: Unimplemented) -> Self {
        Failure::Refused(refusal)
    }
}

impl From<XmlError> for Failure {
    fn from(error: XmlError) -> Self {
        Failure::Xml(error)
    }
}

fn next(r: &mut XmlReader) -> Result<Option<XmlEvent>, Failure> {
    loop {
        match r.next()? {
            Some(XmlEvent::Whitespace(_) | XmlEvent::Declaration { .. }) => continue,
            Some(XmlEvent::Comment(_) | XmlEvent::Pi { .. } | XmlEvent::Doctype(_)) => {
                return Err(OTHER_NODE.into());
            }
            other => return Ok(other),
        }
    }
}

fn soap_ns(ns: &str) -> Option<Version> {
    match ns {
        SOAP11 => Some(Version::V11),
        SOAP12 => Some(Version::V12),
        _ => None,
    }
}

fn plain_attrs(attrs: &[XmlAttr]) -> impl Iterator<Item = &XmlAttr> {
    attrs.iter().filter(|a| a.ns != XMLNS)
}

fn nil_of(attrs: &[XmlAttr]) -> Result<bool, Unimplemented> {
    let mut nil = false;
    for a in plain_attrs(attrs) {
        if a.ns != XSI || a.local != "nil" {
            return Err(ATTRIBUTE);
        }
        nil = match a.value.trim() {
            "true" | "1" => true,
            "false" | "0" => false,
            _ => return Err(ATTRIBUTE),
        };
    }
    Ok(nil)
}

fn param(r: &mut XmlReader, ns: String, name: String, attrs: &[XmlAttr]) -> Result<Param, Failure> {
    let nil = nil_of(attrs)?;
    let mut text = String::new();
    loop {
        match r.next()? {
            Some(XmlEvent::Text(t) | XmlEvent::Whitespace(t) | XmlEvent::CData(t)) => {
                text.push_str(&t)
            }
            Some(XmlEvent::End { .. }) => break,
            Some(XmlEvent::Start { .. }) => {
                return Err(Unimplemented("a parameter value with child elements").into());
            }
            _ => return Err(OTHER_NODE.into()),
        }
    }
    if nil && !text.trim().is_empty() {
        return Err(Unimplemented("a parameter with xsi:nil and a value").into());
    }
    Ok(Param {
        ns,
        name,
        nil,
        text,
    })
}

pub fn parse(body: &[u8]) -> Result<Result<Envelope, (Version, String)>, Unimplemented> {
    let text = decode_xml_bytes(body).map_err(|_| NOT_XML)?;
    let end = end_position(&text);
    match read(XmlReader::new(text)) {
        Ok(envelope) => Ok(Ok(envelope)),
        Err(Failure::Refused(refusal)) => Err(refusal),
        Err(Failure::Xml(_)) => Err(XML_ERROR),
        Err(Failure::InBody(error, version)) => {
            xml_fault_text(&error, end).map(|text| Err((version, text)))
        }
    }
}

fn end_position(text: &str) -> (u32, u32) {
    let mut line = 1;
    let mut col = 1;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\n' || (c == '\r' && chars.peek() != Some(&'\n')) {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

fn xml_fault_text(error: &XmlError, end: (u32, u32)) -> Result<String, Unimplemented> {
    let words: Vec<&str> = error.message.split(' ').collect();
    let element_prefix = matches!(
        words.as_slice(),
        ["Namespace", "prefix", _, "on", _, "is", "not", "defined"]
    );
    if error.fatal || !element_prefix || (error.line, error.col) != end {
        return Err(XML_ERROR);
    }
    use platform_catalog::messages::{Lang, text_key};
    let (Some(parse), Some(severity)) = (
        text_key("xml2:IDS_XMLREADER_ERR_PARSE", Lang::Ru),
        text_key("xml2:IDS_XML_ERROR", Lang::Ru),
    ) else {
        return Err(XML_ERROR);
    };
    let description = format!(
        "{parse}:  - [{},{}]\n{severity}\n{}\n",
        error.line, error.col, error.message
    );
    Ok(exception_text(&bsl_vm::ErrorInfo::new(
        description.as_str().into(),
        Default::default(),
    )))
}

fn read(mut r: XmlReader) -> Result<Envelope, Failure> {
    let Some(XmlEvent::Start {
        local, ns, attrs, ..
    }) = next(&mut r)?
    else {
        return Err(NOT_ENVELOPE.into());
    };
    let version = match soap_ns(&ns) {
        Some(v) if local == "Envelope" => v,
        _ => return Err(NOT_ENVELOPE.into()),
    };
    if plain_attrs(&attrs).next().is_some() {
        return Err(ATTRIBUTE.into());
    }
    let envelope_ns = ns;
    match next(&mut r)? {
        Some(XmlEvent::Start { local, ns, .. }) if ns == envelope_ns && local == "Header" => {
            return Err(Unimplemented("a SOAP Header").into());
        }
        Some(XmlEvent::Start {
            local, ns, attrs, ..
        }) if ns == envelope_ns && local == "Body" => {
            if plain_attrs(&attrs).next().is_some() {
                return Err(ATTRIBUTE.into());
            }
        }
        _ => return Err(NOT_ENVELOPE.into()),
    }
    let first = next(&mut r).map_err(|failure| match failure {
        Failure::Xml(error) => Failure::InBody(error, version),
        other => other,
    })?;
    let (operation, op_ns) = match first {
        Some(XmlEvent::Start {
            local, ns, attrs, ..
        }) => {
            if plain_attrs(&attrs).next().is_some() {
                return Err(ATTRIBUTE.into());
            }
            (local, ns)
        }
        Some(XmlEvent::End { .. }) => return Err(Unimplemented("an empty SOAP Body").into()),
        _ => return Err(NOT_ENVELOPE.into()),
    };
    let mut params = Vec::new();
    loop {
        match next(&mut r)? {
            Some(XmlEvent::End { .. }) => break,
            Some(XmlEvent::Start {
                local, ns, attrs, ..
            }) => {
                params.push(param(&mut r, ns, local, &attrs)?);
            }
            _ => {
                return Err(Unimplemented("text in the operation element of a SOAP call").into());
            }
        }
    }
    for _ in 0..2 {
        if !matches!(next(&mut r)?, Some(XmlEvent::End { .. })) {
            return Err(NOT_ENVELOPE.into());
        }
    }
    if next(&mut r)?.is_some() {
        return Err(NOT_ENVELOPE.into());
    }
    Ok(Envelope {
        version,
        ns: op_ns,
        operation,
        params,
    })
}

#[derive(Debug, PartialEq, Eq)]
pub enum Bound<'a> {
    Call(&'a WsOperation, Vec<Param>),
    Fault(&'static str),
    TooFew(&'a WsOperation),
}

pub const UNKNOWN_OPERATION: &str = "Неверный формат. ";
const UNKNOWN_ERROR: &str = "Неизвестная ошибка. ";
const BECAUSE: &str = "\nпо причине:\n";

fn link(info: &bsl_vm::ErrorInfo) -> String {
    if info.module_name.is_empty() && info.line == 0 {
        info.description.to_string_lossy()
    } else {
        info.description_text(platform_catalog::messages::Lang::Ru)
    }
}

pub fn exception_text(info: &bsl_vm::ErrorInfo) -> String {
    let mut out = String::from(UNKNOWN_ERROR);
    out.push_str(&link(info));
    let mut cur = Some(info);
    while let Some(e) = cur {
        out.push_str(BECAUSE);
        out.push_str(&link(e));
        cur = e.cause.as_deref();
    }
    out
}

pub fn admit_exception(info: &bsl_vm::ErrorInfo) -> Result<(), Unimplemented> {
    match (info.module_name.is_empty(), info.line == 0) {
        (true, true) => {
            return Err(Unimplemented(
                "the SOAP Fault of an error of the call of the procedure of an operation",
            ));
        }
        (false, false) => {}
        _ => {
            return Err(Unimplemented(
                "the SOAP Fault of an exception without a module or a line",
            ));
        }
    }
    let mut cur = Some(info);
    let mut outer = true;
    while let Some(e) = cur {
        if !outer && (!e.module_name.is_empty() || e.line != 0) {
            return Err(Unimplemented(
                "the SOAP Fault of an exception with a cause that has a position",
            ));
        }
        let additional = matches!(&e.additional, bsl_vm::Value::String(s) if !s.is_empty());
        if !e.code.is_empty() || additional {
            return Err(Unimplemented(
                "the SOAP Fault of an exception with Код or ДополнительнаяИнформация",
            ));
        }
        outer = false;
        cur = e.cause.as_deref();
    }
    Ok(())
}

pub fn too_few_parameters() -> String {
    exception_text(&bsl_vm::ErrorInfo::new(
        "Недостаточно параметров операции".into(),
        Default::default(),
    ))
}

pub fn bind(service: &WsService, envelope: Envelope) -> Result<Bound<'_>, Unimplemented> {
    let version = envelope.version;
    let bound = bind_any(service, envelope)?;
    if version == Version::V12 && !matches!(bound, Bound::Call(..)) {
        return Err(Unimplemented(
            "a SOAP 1.2 Fault other than the Fault of an exception",
        ));
    }
    Ok(bound)
}

fn bind_any(service: &WsService, envelope: Envelope) -> Result<Bound<'_>, Unimplemented> {
    if envelope.ns != service.namespace {
        return Err(Unimplemented(
            "an operation element outside the namespace of the web service",
        ));
    }
    let Some(op) = service
        .operations
        .iter()
        .find(|o| o.name == envelope.operation)
    else {
        return Ok(Bound::Fault(UNKNOWN_OPERATION));
    };
    let in_order = envelope.params.len() <= op.parameters.len()
        && envelope
            .params
            .iter()
            .zip(&op.parameters)
            .all(|(sent, p)| sent.ns == service.namespace && sent.name == p.name);
    if !in_order {
        return Err(Unimplemented(
            "SOAP parameters other than those of the operation in their order",
        ));
    }
    if envelope.params.len() < op.parameters.len() {
        if envelope.params.is_empty()
            || op
                .parameters
                .iter()
                .skip(envelope.params.len())
                .any(|p| p.nillable)
        {
            return Err(Unimplemented(
                "missing SOAP parameters that are Nillable, or no parameter sent",
            ));
        }
        return Ok(Bound::TooFew(op));
    }
    Ok(Bound::Call(op, envelope.params))
}

fn escape(out: &mut String, s: &str, attr: bool) -> Result<(), Unimplemented> {
    let units: Vec<u16> = s.encode_utf16().collect();
    if xml_invalid_pos(&units, false) != 0 {
        return Err(Unimplemented(
            "a character not allowed in XML in a SOAP answer",
        ));
    }
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' if attr => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    Ok(())
}

const HEAD: &str =
    "<soap:Envelope xmlns:soap=\"http://schemas.xmlsoap.org/soap/envelope/\">\r\n\t<soap:Body>\r\n";
const HEAD12: &str =
    "<soap:Envelope xmlns:soap=\"http://www.w3.org/2003/05/soap-envelope\">\r\n\t<soap:Body>\r\n";
const TAIL: &str = "\t</soap:Body>\r\n</soap:Envelope>";

pub fn response(
    service: &WsService,
    op: &WsOperation,
    value: &str,
) -> Result<Vec<u8>, Unimplemented> {
    let mut out = String::from(HEAD);
    let mut name = String::new();
    escape(&mut name, &op.name, false)?;
    out.push_str("\t\t<m:");
    out.push_str(&name);
    out.push_str("Response xmlns:m=\"");
    escape(&mut out, &service.namespace, true)?;
    out.push_str("\">\r\n\t\t\t<m:return xmlns:xs=\"http://www.w3.org/2001/XMLSchema\"\r\n\t\t\t\t\txmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\">");
    escape(&mut out, value, false)?;
    out.push_str("</m:return>\r\n\t\t</m:");
    out.push_str(&name);
    out.push_str("Response>\r\n");
    out.push_str(TAIL);
    Ok(out.into_bytes())
}

pub fn rights_fault(service: &WsService, op: &WsOperation) -> Result<Vec<u8>, Unimplemented> {
    fault(&format!(
        "Нарушение прав доступа к операции Web-сервиса:  {{{}}}:{}:{}()",
        service.namespace, service.name, op.name
    ))
}

pub fn fault(text: &str) -> Result<Vec<u8>, Unimplemented> {
    let mut out = String::from(HEAD);
    out.push_str(
        "\t\t<soap:Fault>\r\n\t\t\t<faultcode>soap:Client</faultcode>\r\n\t\t\t<faultstring>",
    );
    escape(&mut out, text, false)?;
    out.push_str("</faultstring>\r\n\t\t\t<detail/>\r\n\t\t</soap:Fault>\r\n");
    out.push_str(TAIL);
    Ok(out.into_bytes())
}

pub fn fault12(text: &str) -> Result<Vec<u8>, Unimplemented> {
    let mut out = String::from(HEAD12);
    out.push_str("\t\t<soap:Fault>\r\n\t\t\t<soap:Code>\r\n\t\t\t\t<soap:Value>soap:Sender</soap:Value>\r\n\t\t\t</soap:Code>\r\n\t\t\t<soap:Reason>\r\n\t\t\t\t<soap:Text xml:lang=\"ru_RU\">");
    escape(&mut out, text, false)?;
    out.push_str("</soap:Text>\r\n\t\t\t</soap:Reason>\r\n\t\t</soap:Fault>\r\n");
    out.push_str(TAIL);
    Ok(out.into_bytes())
}

pub fn exception_fault(version: Version, text: &str) -> Result<Vec<u8>, Unimplemented> {
    match version {
        Version::V11 => fault(text),
        Version::V12 => fault12(text),
    }
}
