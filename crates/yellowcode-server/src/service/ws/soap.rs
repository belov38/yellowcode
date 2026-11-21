use super::{WsOperation, WsService};
use crate::service::wire::Unimplemented;
use bsl_lib::xml::{XmlAttr, XmlEvent, XmlReader, decode_xml_bytes, xml_invalid_pos};

pub const SOAP11: &str = "http://schemas.xmlsoap.org/soap/envelope/";
pub const SOAP12: &str = "http://www.w3.org/2003/05/soap-envelope";
const XSI: &str = "http://www.w3.org/2001/XMLSchema-instance";
const XMLNS: &str = "http://www.w3.org/2000/xmlns/";
pub const CONTENT_TYPE: &str = "text/xml;charset=utf-8";

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
const OTHER_NODE: Unimplemented =
    Unimplemented("a comment, instruction or DOCTYPE in a SOAP envelope");
const ATTRIBUTE: Unimplemented =
    Unimplemented("an attribute other than a namespace declaration and xsi:nil in a SOAP call");

pub fn check_content_type(headers: &[(String, String)]) -> Result<(), Unimplemented> {
    let media = headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case("Content-Type"))
        .map(|(_, v)| v.split(';').next().unwrap_or_default().trim());
    match media {
        Some(m) if m.eq_ignore_ascii_case("text/xml") => Ok(()),
        _ => Err(Unimplemented(
            "a SOAP request without Content-Type text/xml",
        )),
    }
}

fn next(r: &mut XmlReader) -> Result<Option<XmlEvent>, Unimplemented> {
    loop {
        match r.next().map_err(|_| NOT_XML)? {
            Some(XmlEvent::Whitespace(_) | XmlEvent::Declaration { .. }) => continue,
            Some(XmlEvent::Comment(_) | XmlEvent::Pi { .. } | XmlEvent::Doctype(_)) => {
                return Err(OTHER_NODE);
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

fn param(
    r: &mut XmlReader,
    ns: String,
    name: String,
    attrs: &[XmlAttr],
) -> Result<Param, Unimplemented> {
    let nil = nil_of(attrs)?;
    let mut text = String::new();
    loop {
        match r.next().map_err(|_| NOT_XML)? {
            Some(XmlEvent::Text(t) | XmlEvent::Whitespace(t) | XmlEvent::CData(t)) => {
                text.push_str(&t)
            }
            Some(XmlEvent::End { .. }) => break,
            Some(XmlEvent::Start { .. }) => {
                return Err(Unimplemented("a parameter value with child elements"));
            }
            _ => return Err(OTHER_NODE),
        }
    }
    if nil && !text.trim().is_empty() {
        return Err(Unimplemented("a parameter with xsi:nil and a value"));
    }
    Ok(Param {
        ns,
        name,
        nil,
        text,
    })
}

pub fn parse(body: &[u8]) -> Result<Envelope, Unimplemented> {
    let text = decode_xml_bytes(body).map_err(|_| NOT_XML)?;
    let mut r = XmlReader::new(text);
    let Some(XmlEvent::Start {
        local, ns, attrs, ..
    }) = next(&mut r)?
    else {
        return Err(NOT_ENVELOPE);
    };
    let version = match soap_ns(&ns) {
        Some(v) if local == "Envelope" => v,
        _ => return Err(NOT_ENVELOPE),
    };
    if plain_attrs(&attrs).next().is_some() {
        return Err(ATTRIBUTE);
    }
    let envelope_ns = ns;
    match next(&mut r)? {
        Some(XmlEvent::Start { local, ns, .. }) if ns == envelope_ns && local == "Header" => {
            return Err(Unimplemented("a SOAP Header"));
        }
        Some(XmlEvent::Start {
            local, ns, attrs, ..
        }) if ns == envelope_ns && local == "Body" => {
            if plain_attrs(&attrs).next().is_some() {
                return Err(ATTRIBUTE);
            }
        }
        _ => return Err(NOT_ENVELOPE),
    }
    let (operation, op_ns) = match next(&mut r)? {
        Some(XmlEvent::Start {
            local, ns, attrs, ..
        }) => {
            if plain_attrs(&attrs).next().is_some() {
                return Err(ATTRIBUTE);
            }
            (local, ns)
        }
        Some(XmlEvent::End { .. }) => return Err(Unimplemented("an empty SOAP Body")),
        _ => return Err(NOT_ENVELOPE),
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
                return Err(Unimplemented(
                    "text in the operation element of a SOAP call",
                ));
            }
        }
    }
    for _ in 0..2 {
        if !matches!(next(&mut r)?, Some(XmlEvent::End { .. })) {
            return Err(NOT_ENVELOPE);
        }
    }
    if next(&mut r)?.is_some() {
        return Err(NOT_ENVELOPE);
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
pub const TOO_FEW_PARAMETERS: &str = "Неизвестная ошибка. Недостаточно параметров операции\nпо причине:\nНедостаточно параметров операции";

pub fn bind(service: &WsService, envelope: Envelope) -> Result<Bound<'_>, Unimplemented> {
    if envelope.version != Version::V11 {
        return Err(Unimplemented("a SOAP 1.2 call"));
    }
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
