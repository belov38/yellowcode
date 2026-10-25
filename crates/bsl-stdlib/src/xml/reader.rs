use super::{NS_XSI, parse_error, types, xml_error};
use crate::binary::{read_stream, stream_of};
use crate::bind::{arg_i64, arg_str, arg_value, msg, type_mismatch};
use crate::collections::{
    ctor_not_implemented, method_name, not_implemented, property_name, property_ru,
};
use crate::enums::enum_value;
use crate::globals::reserve_units;
use bsl_lib::xml::{XmlAttr, XmlError, XmlEvent, XmlReader, decode_xml_bytes_partial};
use bsl_prim::Str;
use bsl_vm::{
    Arg, CallResult, Execution, HostError, HostObject, MemberId, Registry, TypeId, TypeImpl, Value,
};
use std::any::Any;
use std::cell::{Cell, RefCell, RefMut};
use std::rc::Rc;
use std::sync::Arc;

const XMLNS_NS: &str = "http://www.w3.org/2000/xmlns/";

pub(crate) struct XmlReaderState {
    r: XmlReader,
    cur: Option<XmlEvent>,
    before: (u32, u32),
    pub(crate) ignore_whitespace: bool,
    stray: Option<(String, String)>,
    deferred: Option<XmlError>,
    ahead_checked: bool,
    broken: bool,
}

const AHEAD_CHARS: usize = 4096;

pub(crate) struct SimpleText {
    pub text: String,
    pub prefix_ns: Option<String>,
}

fn is_declaration(a: &XmlAttr) -> bool {
    a.ns == XMLNS_NS || a.qname == "xmlns" || a.qname.starts_with("xmlns:")
}

impl XmlReaderState {
    fn new(text: String, deferred: Option<XmlError>, ignore_whitespace: bool) -> XmlReaderState {
        XmlReaderState {
            r: XmlReader::new(text),
            cur: None,
            before: (1, 1),
            ignore_whitespace,
            stray: None,
            deferred,
            ahead_checked: false,
            broken: false,
        }
    }

    fn merge(&mut self, e: XmlError) -> XmlError {
        match &self.deferred {
            Some(d) if (e.line, e.col) >= (d.line, d.col) => self.deferred.take().unwrap_or(e),
            _ => e,
        }
    }

    fn check_ahead(&mut self) -> Option<XmlError> {
        self.ahead_checked = true;
        match self.r.check_ahead(AHEAD_CHARS) {
            Err(e) => Some(self.merge(e)),
            Ok(true) => self.deferred.take(),
            Ok(false) => None,
        }
    }

    fn next_event(&mut self, ex: &mut Execution) -> CallResult<Option<XmlEvent>> {
        ex.charge(1)?;
        if self.broken {
            return Ok(None);
        }
        let ahead = if self.ahead_checked {
            None
        } else {
            self.check_ahead()
        };
        let failure = match ahead {
            Some(e) => Some(e),
            None => match self.r.next() {
                Ok(Some(e)) => return Ok(Some(e)),
                Ok(None) => match self.deferred.take() {
                    Some(d) => Some(d),
                    None => return Ok(None),
                },
                Err(e) => Some(self.merge(e)),
            },
        };
        match failure {
            Some(e) => {
                self.broken = true;
                self.cur = None;
                Err(parse_error(ex, &e).into())
            }
            None => Ok(None),
        }
    }

    pub(crate) fn read(&mut self, ex: &mut Execution) -> CallResult<bool> {
        loop {
            self.before = self.r.position();
            match self.next_event(ex)? {
                None => {
                    self.cur = None;
                    return Ok(false);
                }
                Some(XmlEvent::Whitespace(_)) if self.ignore_whitespace => {}
                Some(XmlEvent::Declaration { .. } | XmlEvent::Comment(_)) => {}
                Some(e) => {
                    self.cur = Some(e);
                    return Ok(true);
                }
            }
        }
    }

    pub(crate) fn end_tag_position(&self) -> (u32, u32) {
        match &self.cur {
            Some(XmlEvent::End { prefix, .. }) if !prefix.is_empty() => {
                let (line, col) = self.before;
                (line, col + 2 + prefix.chars().count() as u32)
            }
            _ => self.r.position(),
        }
    }

    pub(crate) fn read_tree(
        &mut self,
        ex: &mut Execution,
    ) -> CallResult<bsl_lib::xdto::XmlElement> {
        if self.cur.is_some() {
            return Err(xml_error(msg::XML_INVALID_DATA).into());
        }
        let tree = match bsl_lib::xdto::read_tree(&mut self.r) {
            Ok(t) => t,
            Err(e) => {
                let e = self.merge(e);
                self.broken = true;
                return Err(parse_error(ex, &e).into());
            }
        };
        match self.deferred.take() {
            Some(d) => Err(parse_error(ex, &d).into()),
            None => Ok(tree),
        }
    }

    pub(crate) fn node(&self) -> Option<&XmlEvent> {
        self.cur.as_ref()
    }

    pub(crate) fn at_start(&self) -> bool {
        matches!(self.cur, Some(XmlEvent::Start { .. }))
    }

    pub(crate) fn lookup_namespace(&self, prefix: &str) -> Option<&str> {
        self.r.lookup_namespace(prefix)
    }

    pub(crate) fn node_type(&self) -> &'static str {
        match &self.cur {
            None => "None",
            Some(XmlEvent::Declaration { .. }) => "XMLDeclaration",
            Some(XmlEvent::Start { .. }) => "StartElement",
            Some(XmlEvent::End { .. }) => "EndElement",
            Some(XmlEvent::Text(_) | XmlEvent::Whitespace(_) | XmlEvent::CData(_)) => "Text",
            Some(XmlEvent::Comment(_)) => "Comment",
            Some(XmlEvent::Pi { .. }) => "ProcessingInstruction",
            Some(XmlEvent::Doctype(_)) => "DocumentTypeDefinition",
        }
    }

    pub(crate) fn all_attributes(&self) -> Vec<&XmlAttr> {
        match &self.cur {
            Some(XmlEvent::Start { attrs, .. }) => attrs.iter().collect(),
            _ => Vec::new(),
        }
    }

    pub(crate) fn attributes(&self) -> Vec<&XmlAttr> {
        match &self.cur {
            Some(XmlEvent::Start { attrs, .. }) => {
                attrs.iter().filter(|a| !is_declaration(a)).collect()
            }
            _ => Vec::new(),
        }
    }

    pub(crate) fn attribute_in(&self, ns: &str, local: &str) -> Option<&str> {
        self.attributes()
            .into_iter()
            .find(|a| a.ns == ns && a.local == local)
            .map(|a| a.value.as_str())
    }

    pub(crate) fn xsi_type(&self) -> Option<(String, String)> {
        let v = self.attribute_in(NS_XSI, "type")?.trim();
        let (prefix, local) = v.split_once(':').unwrap_or(("", v));
        let ns = self.lookup_namespace(prefix).unwrap_or_default();
        Some((ns.to_string(), local.to_string()))
    }

    pub(crate) fn xsi_type_text(&self) -> Option<(String, bool)> {
        let v = self.attribute_in(NS_XSI, "type")?.trim();
        let prefix = v.split_once(':').map_or("", |(p, _)| p);
        Some((v.to_string(), self.lookup_namespace(prefix).is_some()))
    }

    pub(crate) fn position_after_element(&mut self, ex: &mut Execution) -> CallResult<(u32, u32)> {
        self.skip(ex)?;
        Ok(self.r.position())
    }

    pub(crate) fn is_nil(&self) -> bool {
        self.attribute_in(NS_XSI, "nil")
            .is_some_and(|v| matches!(v.trim(), "true" | "1"))
    }

    pub(crate) fn read_simple(
        &mut self,
        ex: &mut Execution,
        qname: bool,
    ) -> CallResult<SimpleText> {
        let mut text = String::new();
        let mut prefix_ns = None;
        loop {
            let Some(e) = self.next_event(ex)? else {
                return Err(xml_error(msg::XML_INVALID_DATA).into());
            };
            match e {
                XmlEvent::Text(s) | XmlEvent::Whitespace(s) | XmlEvent::CData(s) => {
                    ex.reserve(
                        (text.len() as u64)
                            .saturating_add(s.len() as u64)
                            .saturating_mul(2),
                    )?;
                    text.push_str(&s);
                    if qname {
                        let t = text.trim();
                        let p = t.split_once(':').map_or("", |(p, _)| p);
                        prefix_ns = self.r.lookup_namespace(p).map(str::to_string);
                    }
                }
                XmlEvent::Comment(_) | XmlEvent::Pi { .. } => {}
                XmlEvent::End { .. } => break,
                other => {
                    if let XmlEvent::Start { ns, local, .. } = &other {
                        self.stray = Some((ns.clone(), local.clone()));
                    }
                    return Err(xml_error(msg::XML_INVALID_DATA).into());
                }
            }
        }
        self.read(ex)?;
        Ok(SimpleText { text, prefix_ns })
    }

    pub(crate) fn stray_child(&self) -> Option<&(String, String)> {
        self.stray.as_ref()
    }

    pub(crate) fn skip(&mut self, ex: &mut Execution) -> CallResult<()> {
        if !self.at_start() {
            return Ok(());
        }
        let mut depth = 1usize;
        while depth > 0 {
            match self.next_event(ex)? {
                Some(e) => {
                    match &e {
                        XmlEvent::Start { .. } => depth += 1,
                        XmlEvent::End { .. } => depth -= 1,
                        _ => {}
                    }
                    self.cur = Some(e);
                }
                None => return Err(xml_error(msg::XML_INVALID_DATA).into()),
            }
        }
        Ok(())
    }

    pub(crate) fn move_to_content(
        &mut self,
        ex: &mut Execution,
        whitespace: bool,
    ) -> CallResult<()> {
        loop {
            let content = match &self.cur {
                Some(
                    XmlEvent::Start { .. }
                    | XmlEvent::End { .. }
                    | XmlEvent::Text(_)
                    | XmlEvent::CData(_),
                ) => true,
                Some(XmlEvent::Whitespace(_)) => whitespace,
                _ => false,
            };
            if content || !self.read(ex)? {
                return Ok(());
            }
        }
    }

    fn name(&self) -> String {
        match &self.cur {
            Some(XmlEvent::Start { qname, .. } | XmlEvent::End { qname, .. }) => qname.clone(),
            Some(XmlEvent::Pi { target, .. }) => target.clone(),
            Some(XmlEvent::Text(_) | XmlEvent::Whitespace(_) | XmlEvent::CData(_)) => {
                "#text".into()
            }
            Some(XmlEvent::Declaration { .. }) => "xml".into(),
            Some(XmlEvent::Doctype(s)) => s.split_whitespace().next().unwrap_or_default().into(),
            _ => String::new(),
        }
    }

    fn value(&self) -> String {
        match &self.cur {
            Some(
                XmlEvent::Text(s)
                | XmlEvent::Whitespace(s)
                | XmlEvent::CData(s)
                | XmlEvent::Comment(s)
                | XmlEvent::Doctype(s),
            ) => s.clone(),
            Some(XmlEvent::Pi { data, .. }) => data.clone(),
            Some(XmlEvent::Declaration {
                version,
                encoding,
                standalone,
            }) => {
                let mut s = format!("version=\"{version}\"");
                if let Some(e) = encoding {
                    s.push_str(&format!(" encoding=\"{e}\""));
                }
                if let Some(b) = standalone {
                    s.push_str(&format!(
                        " standalone=\"{}\"",
                        if *b { "yes" } else { "no" }
                    ));
                }
                s
            }
            _ => String::new(),
        }
    }
}

pub(crate) struct XmlReaderObject {
    st: RefCell<Option<XmlReaderState>>,
    ignore_whitespace: Cell<bool>,
}

fn no_source() -> HostError {
    xml_error(msg::XML_NO_SOURCE)
}

fn state_of(r: &XmlReaderObject) -> CallResult<RefMut<'_, XmlReaderState>> {
    let g = r.st.try_borrow_mut().map_err(|_| no_source())?;
    RefMut::filter_map(g, Option::as_mut).map_err(|_| no_source().into())
}

pub(crate) fn xml_reader<'a>(
    _ex: &mut Execution,
    v: &'a Value,
) -> CallResult<RefMut<'a, XmlReaderState>> {
    let Some(r) = v
        .as_object()
        .and_then(|o| o.as_any().downcast_ref::<XmlReaderObject>())
    else {
        return Err(type_mismatch(1).into());
    };
    state_of(r)
}

impl XmlReaderObject {
    fn open_text(&self, text: String, deferred: Option<XmlError>) -> CallResult<()> {
        let st = XmlReaderState::new(text, deferred, self.ignore_whitespace.get());
        *self.st.try_borrow_mut().map_err(|_| no_source())? = Some(st);
        Ok(())
    }

    fn open_file(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<()> {
        let path = arg_str(ex, args.first())?.to_string_lossy();
        let bytes = crate::text::read_file(ex, &path)?;
        self.open_bytes(ex, bytes)
    }

    fn open_bytes(&self, ex: &mut Execution, bytes: Vec<u8>) -> CallResult<()> {
        reserve_units(ex, bytes.len(), 4)?;
        let (text, deferred) = decode_xml_bytes_partial(&bytes);
        drop(bytes);
        self.open_text(text, deferred)
    }

    fn open_stream(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<()> {
        let first = arg_value(ex, args.first())?;
        let Some(s) = stream_of(&first) else {
            return Err(type_mismatch(1).into());
        };
        let bytes = read_stream(ex, &s, u64::MAX)?;
        self.open_bytes(ex, bytes)
    }

    fn with_state<T>(&self, f: impl FnOnce(&mut XmlReaderState) -> CallResult<T>) -> CallResult<T> {
        let mut g = state_of(self)?;
        f(&mut g)
    }
}

fn find_attribute<'a>(
    ex: &mut Execution,
    st: &'a XmlReaderState,
    args: &[Arg],
) -> CallResult<Option<&'a XmlAttr>> {
    let attrs = st.all_attributes();
    let first = arg_value(ex, args.first())?;
    if let Value::Number(_) = first {
        let i = arg_i64(ex, args.first(), 1)?;
        return Ok(usize::try_from(i).ok().and_then(|i| attrs.get(i)).copied());
    }
    let name = arg_str(ex, args.first())?.to_string_lossy();
    if args.len() >= 2 {
        let uri = arg_str(ex, args.get(1))?.to_string_lossy();
        return Ok(attrs.into_iter().find(|a| a.local == name && a.ns == uri));
    }
    Ok(attrs.into_iter().find(|a| a.qname == name))
}

fn attribute_at<'a>(
    ex: &mut Execution,
    st: &'a XmlReaderState,
    a: Option<&Arg>,
) -> CallResult<Option<&'a XmlAttr>> {
    let i = arg_i64(ex, a, 1)?;
    Ok(st
        .all_attributes()
        .into_iter()
        .nth(usize::try_from(i).unwrap_or(usize::MAX)))
}

impl HostObject for XmlReaderObject {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(types().reader)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<Str>> {
        Ok(Some(crate::collections::type_name(ex, types().reader)))
    }

    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let t = types().reader;
        let Some(name) = property_name(t, m) else {
            return Err(not_implemented(t, m).into());
        };
        if name == "IgnoreWhitespace" {
            return Ok(Value::Bool(self.ignore_whitespace.get()));
        }
        let st = self.st.try_borrow().map_err(|_| no_source())?;
        let st = st.as_ref();
        let text = |s: &str| Ok(Value::String(Str::from(s)));
        let (ns, prefix, local) = match st.and_then(|s| s.node()) {
            Some(XmlEvent::Start {
                ns, prefix, local, ..
            })
            | Some(XmlEvent::End {
                ns, prefix, local, ..
            }) => (ns.as_str(), prefix.as_str(), local.as_str()),
            _ => ("", "", ""),
        };
        let kind = st.map_or("None", XmlReaderState::node_type);
        match name {
            "NodeType" => {
                enum_value(types().node_type, kind).ok_or_else(|| not_implemented(t, m).into())
            }
            "Name" => text(&st.map(XmlReaderState::name).unwrap_or_default()),
            "LocalName" => text(local),
            "Prefix" => text(prefix),
            "NamespaceURI" => text(ns),
            "Value" => {
                let v = st.map(XmlReaderState::value).unwrap_or_default();
                ex.charge(v.len() as u64 / 64)?;
                text(&v)
            }
            "HasValue" => Ok(Value::Bool(kind == "Text")),
            "HasName" => Ok(Value::Bool(matches!(
                kind,
                "StartElement"
                    | "EndElement"
                    | "ProcessingInstruction"
                    | "XMLDeclaration"
                    | "DocumentTypeDefinition"
            ))),
            "IsWhitespace" => Ok(Value::Bool(matches!(
                st.and_then(|s| s.node()),
                Some(XmlEvent::Whitespace(_))
            ))),
            "IsCharacters" => Ok(Value::Bool(kind == "Text")),
            _ => Err(not_implemented(t, m).into()),
        }
    }

    fn set(&self, ex: &mut Execution, m: MemberId, v: Value) -> CallResult<()> {
        let t = types().reader;
        match property_name(t, m) {
            Some("IgnoreWhitespace") => {
                let on = bsl_vm::ops::to_bool(ex, &v)?;
                self.ignore_whitespace.set(on);
                if let Ok(mut g) = self.st.try_borrow_mut()
                    && let Some(s) = g.as_mut()
                {
                    s.ignore_whitespace = on;
                }
                Ok(())
            }
            Some(_) => Err(HostError::key(msg::PROP_NOT_WRITABLE)
                .with_arg(property_ru(t, m))
                .raw()
                .into()),
            None => Err(not_implemented(t, m).into()),
        }
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        let t = types().reader;
        let Some(name) = method_name(t, m) else {
            return Err(not_implemented(t, m).into());
        };
        let args: &[Arg] = args;
        match name {
            "SetString" => {
                let s = arg_str(ex, args.first())?;
                reserve_units(ex, s.len(), 4)?;
                self.open_text(s.to_string_lossy(), None)?;
            }
            "OpenFile" => self.open_file(ex, args)?,
            "OpenStream" => self.open_stream(ex, args)?,
            "Read" => return Ok(Value::Bool(self.with_state(|s| s.read(ex))?)),
            "MoveToContent" => {
                let ws = !matches!(arg_value(ex, args.first())?, Value::Undefined)
                    && crate::bind::arg_bool(ex, args.first())?;
                let kind = self.with_state(|s| {
                    s.move_to_content(ex, ws)?;
                    Ok(s.node_type())
                })?;
                return enum_value(types().node_type, kind)
                    .ok_or_else(|| not_implemented(t, m).into());
            }
            "Skip" => self.with_state(|s| s.skip(ex))?,
            "Close" => {
                if let Ok(mut g) = self.st.try_borrow_mut() {
                    *g = None;
                }
            }
            "AttributeCount" => {
                let n = self.with_state(|s| Ok(s.all_attributes().len()))?;
                return Ok(Value::Number(bsl_prim::Number::from_i64(n as i64)));
            }
            "AttributeValue" | "GetAttribute" => {
                let st = state_of(self)?;
                return Ok(match find_attribute(ex, &st, args)? {
                    Some(a) => Value::from(a.value.as_str()),
                    None => Value::Undefined,
                });
            }
            "AttributeName"
            | "AttributeLocalName"
            | "AttributePrefix"
            | "AttributeNamespaceURI" => {
                let st = state_of(self)?;
                let Some(a) = attribute_at(ex, &st, args.first())? else {
                    return Ok(Value::Undefined);
                };
                let s = match name {
                    "AttributeName" => &a.qname,
                    "AttributeLocalName" => &a.local,
                    "AttributePrefix" => &a.prefix,
                    _ => &a.ns,
                };
                return Ok(Value::from(s.as_str()));
            }
            _ => return Err(not_implemented(t, m).into()),
        }
        Ok(Value::Undefined)
    }
}

fn ctor(_ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    if !args.is_empty() {
        return Err(ctor_not_implemented(types().reader).into());
    }
    Ok(Value::Object(Rc::new(XmlReaderObject {
        st: RefCell::new(None),
        ignore_whitespace: Cell::new(true),
    })))
}

pub(super) fn register(reg: &mut Registry) {
    reg.type_impl(
        types().reader,
        TypeImpl {
            ctor: Some(Arc::new(ctor)),
        },
    );
}
