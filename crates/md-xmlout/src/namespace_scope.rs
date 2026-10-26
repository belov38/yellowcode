use crate::{Error, Result, XmlElem, XmlWriter};
use md_model::namespace_scope::NamespaceEnvelope;
use quick_xml::{Reader, events::Event};

fn refusal(what: &str) -> Error {
    Error::Format {
        entry: "XML namespace scope".into(),
        what: what.into(),
    }
}
pub(crate) fn validate_type(description: &md_model::TypeDescription) -> Result<()> {
    for member in &description.types {
        mdschema::ExpandedTypeName::new(member.name.namespace_uri(), &member.name.local)
            .ok_or_else(|| refusal("invalid Type namespace or local identity"))?;
    }
    Ok(())
}

pub(crate) fn validate_properties(props: &[md_model::Prop], has_envelope: bool) -> Result<()> {
    fn properties(props: &[md_model::Prop], prefix: &str, has_envelope: bool) -> Result<()> {
        for prop in props {
            match &prop.value {
                md_model::Value::Type(description) => validate_type(description)?,
                md_model::Value::Xml(raw) => {
                    if raw.name != format!("{prefix}{}", prop.name) {
                        return Err(refusal("raw property output name differs from original"));
                    }
                    if !has_envelope {
                        let bindings: Vec<_> = crate::NS_DECL
                            .split_ascii_whitespace()
                            .filter_map(|declaration| {
                                let (prefix, uri) =
                                    declaration.strip_prefix("xmlns:")?.split_once("=\"")?;
                                Some((prefix, uri.strip_suffix('"')?))
                            })
                            .collect();
                        validate(raw, &bindings, true)?;
                    }
                }
                md_model::Value::StandardAttributes(attributes) => {
                    for attribute in attributes {
                        properties(&attribute.props, "xr:", has_envelope)?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
    properties(props, "", has_envelope)
}

fn tree(text: &str) -> Result<XmlElem> {
    let mut reader = Reader::from_str(text.trim_start_matches('\u{feff}'));
    let mut stack: Vec<XmlElem> = Vec::new();
    let mut root = None;
    loop {
        let event = reader
            .read_event()
            .map_err(|_| refusal("invalid generated description"))?;
        let empty = matches!(&event, Event::Empty(_));
        match event {
            Event::Start(start) | Event::Empty(start) => {
                let mut element = XmlElem::new(
                    std::str::from_utf8(start.name().as_ref())
                        .map_err(|_| refusal("invalid element name"))?,
                );
                for attr in start.attributes() {
                    let attr = attr.map_err(|_| refusal("invalid generated attribute"))?;
                    element.attrs.push((
                        std::str::from_utf8(attr.key.as_ref())
                            .map_err(|_| refusal("invalid attribute name"))?
                            .to_owned(),
                        attr.decode_and_unescape_value(reader.decoder())
                            .map_err(|_| refusal("invalid generated attribute"))?
                            .into_owned(),
                    ));
                }
                if empty {
                    attach(element, &mut stack, &mut root)?;
                } else {
                    element.expanded = true;
                    stack.push(element);
                }
            }
            Event::End(_) => {
                let mut element = stack
                    .pop()
                    .ok_or_else(|| refusal("unbalanced generated description"))?;
                if !element.children.is_empty() {
                    element.text = None;
                }
                element.expanded = false;
                attach(element, &mut stack, &mut root)?;
            }
            Event::Text(text) => {
                if let Some(element) = stack.last_mut() {
                    let decoded = text
                        .unescape()
                        .map_err(|_| refusal("invalid generated text"))?;
                    element
                        .text
                        .get_or_insert_with(String::new)
                        .push_str(&decoded);
                }
            }
            Event::Eof => break,
            Event::Decl(_) | Event::Comment(_) | Event::PI(_) => {}
            _ => return Err(refusal("unsupported generated XML event")),
        }
    }
    if !stack.is_empty() {
        return Err(refusal("unbalanced generated description"));
    }
    root.ok_or_else(|| refusal("missing generated description"))
}
fn attach(element: XmlElem, stack: &mut [XmlElem], root: &mut Option<XmlElem>) -> Result<()> {
    if let Some(parent) = stack.last_mut() {
        parent.children.push(element);
    } else if root.replace(element).is_some() {
        return Err(refusal("duplicate generated description root"));
    }
    Ok(())
}
fn at_mut<'a>(mut element: &'a mut XmlElem, path: &[usize]) -> Option<&'a mut XmlElem> {
    for index in path {
        element = element.children.get_mut(*index)?;
    }
    Some(element)
}
fn at<'a>(mut element: &'a XmlElem, path: &[usize]) -> Option<&'a XmlElem> {
    for index in path {
        element = element.children.get(*index)?;
    }
    Some(element)
}
fn namespace_attr(name: &str) -> bool {
    name == "xmlns" || name.starts_with("xmlns:")
}

pub fn restore(text: &str, envelope: Option<&NamespaceEnvelope>) -> Result<String> {
    let mut root = tree(text)?;
    if let Some(envelope) = envelope {
        for (index, anchor) in envelope.anchors.iter().enumerate() {
            if envelope
                .anchors
                .iter()
                .take(index)
                .any(|a| a.path == anchor.path)
            {
                return Err(refusal("conflicting namespace envelope anchors"));
            }
            let element =
                at_mut(&mut root, &anchor.path).ok_or_else(|| refusal("namespace anchor moved"))?;
            if element.name != anchor.element
                || anchor
                    .identity
                    .as_ref()
                    .is_some_and(|identity| !element.attrs.contains(identity))
            {
                return Err(refusal("namespace anchor identity changed"));
            }
            if anchor
                .declarations
                .iter()
                .enumerate()
                .any(|(i, (name, _))| {
                    !namespace_attr(name)
                        || anchor
                            .declarations
                            .iter()
                            .take(i)
                            .any(|(old, _)| old == name)
                })
            {
                return Err(refusal("conflicting namespace envelope declarations"));
            }
            element.attrs.retain(|(name, _)| !namespace_attr(name));
            element
                .attrs
                .splice(0..0, anchor.declarations.iter().cloned());
        }
        for witness in &envelope.raw_properties {
            let element = at(&root, &witness.path)
                .ok_or_else(|| refusal("raw property moved outside captured scope"))?;
            if !same_raw(element, &witness.element) {
                return Err(refusal(
                    "raw property changed or moved outside captured scope",
                ));
            }
        }
    }
    validate(&root, &[], false)?;
    let Some(_) = envelope else {
        return Ok(text.to_owned());
    };
    let mut writer = XmlWriter::new(0);
    writer.elem(&root);
    Ok(format!(
        "\u{feff}<?xml version=\"1.0\" encoding=\"UTF-8\"?>\r\n{}",
        writer.text()
    ))
}
fn same_raw(a: &XmlElem, b: &XmlElem) -> bool {
    a.name == b.name
        && a.attrs == b.attrs
        && a.text.as_deref().filter(|s| !s.is_empty())
            == b.text.as_deref().filter(|s| !s.is_empty())
        && a.children.len() == b.children.len()
        && a.children
            .iter()
            .zip(&b.children)
            .all(|(a, b)| same_raw(a, b))
}
fn validate<'a>(
    element: &'a XmlElem,
    inherited: &[(&'a str, &'a str)],
    require_default: bool,
) -> Result<()> {
    let mut bindings = inherited.to_vec();
    for (name, uri) in &element.attrs {
        if let Some(prefix) = name.strip_prefix("xmlns:") {
            if !mdschema::is_ncname(prefix) || uri.is_empty() {
                return Err(refusal("invalid namespace declaration"));
            }
            let reserved = mdschema::FixedNamespace::from_prefix(prefix)
                .map(|fixed| fixed.uri())
                .or_else(|| {
                    crate::NS_DECL
                        .split_ascii_whitespace()
                        .find_map(|declaration| {
                            let (name, value) =
                                declaration.strip_prefix("xmlns:")?.split_once("=\"")?;
                            (name == prefix).then(|| value.strip_suffix('"')).flatten()
                        })
                });
            if prefix == "xmlns"
                || (prefix == "xml" && uri != "http://www.w3.org/XML/1998/namespace")
                || reserved.is_some_and(|expected| expected != uri)
            {
                return Err(refusal("reserved namespace prefix rebound"));
            }
            bindings.push((prefix, uri));
        } else if name == "xmlns" {
            bindings.push(("", uri));
        }
    }
    fn resolve_name<'n, 's>(
        qname: &'n str,
        attribute: bool,
        bindings: &[(&'s str, &'s str)],
    ) -> Option<(&'s str, &'n str)> {
        let (prefix, local) = match qname.split_once(':') {
            Some((prefix, local)) if mdschema::is_ncname(prefix) && mdschema::is_ncname(local) => {
                (prefix, local)
            }
            Some(_) => return None,
            None if mdschema::is_ncname(qname) => ("", qname),
            None => return None,
        };
        let uri = if prefix == "xml" {
            "http://www.w3.org/XML/1998/namespace"
        } else if prefix.is_empty() && attribute {
            ""
        } else {
            match bindings.iter().rev().find(|(p, _)| *p == prefix) {
                Some((_, uri)) => *uri,
                None if prefix.is_empty() => "",
                None => return None,
            }
        };
        Some((uri, local))
    }
    let resolve = |qname, attribute| resolve_name(qname, attribute, &bindings);
    let resolve_qname = |qname: &str| {
        if require_default
            && !qname.contains(':')
            && !bindings.iter().any(|(prefix, _)| prefix.is_empty())
        {
            None
        } else {
            resolve_name(qname, false, &bindings).map(|_| ())
        }
    };
    let (element_uri, element_local) = resolve(&element.name, false)
        .ok_or_else(|| refusal("unsupported element namespace scope"))?;
    for (name, value) in &element.attrs {
        if namespace_attr(name) {
            continue;
        }
        let (uri, local) =
            resolve(name, true).ok_or_else(|| refusal("unsupported attribute namespace scope"))?;
        if uri == "http://www.w3.org/2001/XMLSchema-instance"
            && local == "type"
            && resolve_qname(value).is_none()
        {
            return Err(refusal("unsupported attribute QName namespace scope"));
        }
    }
    if element_uri == mdschema::FixedNamespace::V8.uri()
        && matches!(element_local, "Type" | "TypeSet")
        && !element
            .text
            .as_deref()
            .is_some_and(|text| resolve_qname(text).is_some())
    {
        return Err(refusal("unsupported Type QName namespace scope"));
    }
    for child in &element.children {
        validate(child, &bindings, require_default)?;
    }
    Ok(())
}

pub fn render_fragment(element: &XmlElem, inherited: &[(&str, &str)]) -> Result<String> {
    validate(element, inherited, false)?;
    let mut writer = XmlWriter::new(0);
    writer.elem(element);
    Ok(writer.text())
}
