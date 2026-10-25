use super::data::{
    ScalarProjection, XdtoData, XdtoWriteFlags, host_child_explicit, validate_host_typing,
    value_from_text,
};
use super::{
    EProp, PropCache, any_type, args_error, expected_line, form_name, headed, is_any,
    is_qname_type, is_qname_union, object_type, property_block, single_value, structure_error,
    text_for, unsupported, value_type,
};
use crate::bind::{key_error, msg, with_other};
use crate::serial::cycle_fatal;
use crate::serial::depth::DepthGuard;
use crate::xml::{NS_XS, NS_XSI, XmlReaderState, XmlWriterState, split_qname, xml_error};
use bsl_lib::xdto::{Form, Model, QName};
use bsl_lib::xml::{XmlEvent, xml_invalid_pos};
use bsl_vm::{CallError, CallResult, Cause, Execution};

struct Out<'a> {
    ex: &'a mut Execution,
    m: &'a Model,
    w: &'a mut XmlWriterState,
    f: XdtoWriteFlags,
    guard: DepthGuard,
    cache: PropCache,
}

pub(crate) fn write_xml(
    ex: &mut Execution,
    m: &Model,
    w: &mut XmlWriterState,
    root: Option<&QName>,
    d: &XdtoData,
    f: XdtoWriteFlags,
) -> CallResult<()> {
    validate_host_typing(ex, m, d, f)?;
    let name = match (root, d) {
        (Some(q), _) => q.clone(),
        (None, XdtoData::Value { ty, .. } | XdtoData::Object { ty, .. }) => ty.clone(),
        (None, XdtoData::Nil) => QName::new("", "Undefined"),
    };
    let mut out = Out {
        ex,
        m,
        w,
        f,
        guard: DepthGuard::new(),
        cache: PropCache::default(),
    };
    out.element(&name.ns, &name.name, None, d, true, None)
}

fn is_qname_value(m: &Model, ty: &QName, lexical: &str) -> bool {
    is_qname_type(m, ty) || (lexical.starts_with('{') && is_qname_union(m, ty))
}

fn is_generated_prefix(p: &str) -> bool {
    let Some(rest) = p.strip_prefix('d') else {
        return false;
    };
    let Some((depth, n)) = rest.split_once('p') else {
        return false;
    };
    !depth.is_empty()
        && !n.is_empty()
        && depth.bytes().all(|b| b.is_ascii_digit())
        && n.bytes().all(|b| b.is_ascii_digit())
}

fn attribute_prefix(ns: &str) -> Option<&'static str> {
    (ns == "http://v8.1c.ru/8.1/data/ui/style").then_some("style")
}

fn clark_parts(s: &str) -> (&str, &str) {
    match s.strip_prefix('{').and_then(|r| r.split_once('}')) {
        Some((ns, local)) => (ns, local),
        None => ("", s),
    }
}

struct QNames {
    text: Option<String>,
    attrs: Vec<(String, String)>,
}

impl Out<'_> {
    fn qname_text(&mut self, depth: u64, gen_n: &mut u32, clark: &str) -> CallResult<String> {
        self.qname_text_as(depth, gen_n, clark, None)
    }

    fn qname_text_as(
        &mut self,
        depth: u64,
        gen_n: &mut u32,
        clark: &str,
        preferred: Option<&str>,
    ) -> CallResult<String> {
        let (ns, local) = clark_parts(clark);
        if ns.is_empty() {
            return Ok(local.to_string());
        }
        let bound = self
            .w
            .prefix_of(ns)
            .filter(|p| !is_generated_prefix(p) || self.w.declared_here(ns) == Some(*p))
            .map(str::to_string);
        let prefix = match bound {
            Some(p) => p,
            None => {
                let p = if ns == NS_XS && self.w.lookup_uri("xs").is_none() {
                    "xs".to_string()
                } else if let Some(p) = preferred.filter(|p| self.w.lookup_uri(p).is_none()) {
                    p.to_string()
                } else {
                    *gen_n += 1;
                    format!("d{depth}p{gen_n}")
                };
                self.w.namespace(self.ex, &p, ns)?;
                p
            }
        };
        Ok(if prefix.is_empty() {
            local.to_string()
        } else {
            format!("{prefix}:{local}")
        })
    }

    fn wrap(&self, ctx: String, e: CallError) -> CallError {
        match e {
            CallError::Host(h) => {
                let inner = with_other(
                    key_error(msg::XDTO_WRITE_ERROR, Vec::new(), String::new()),
                    false,
                )
                .caused_by(Cause::Host(Box::new(h)));
                headed(msg::XDTO_SERIALIZATION, &ctx)
                    .caused_by(Cause::Host(Box::new(inner)))
                    .into()
            }
            other => other,
        }
    }

    fn context(&self, prop: Option<&EProp>, ns: &str, local: &str) -> String {
        let q = QName::new(ns, local).clark();
        let form = super::form_name(self.ex.lang(), Form::Element).to_string();
        match prop {
            None => text_for(self.ex, msg::XDTO_WRITE_CUSTOM, &[form, q]),
            Some(p) => text_for(
                self.ex,
                msg::XDTO_WRITE_PROPERTY,
                &[p.prop.name.clone(), form, q],
            ),
        }
    }

    fn put_text(&mut self, ctx: &str, s: &str) -> CallResult<()> {
        if !s.is_ascii() {
            let units: Vec<u16> = s.encode_utf16().collect();
            let pos = xml_invalid_pos(&units, false);
            if pos != 0 {
                return Err(self.wrap(
                    ctx.to_string(),
                    crate::xml::bad_char_error(pos as usize - 1, s).into(),
                ));
            }
        } else if let Some(i) = s
            .bytes()
            .position(|b| b < 0x20 && !matches!(b, b'\t' | b'\n' | b'\r'))
        {
            return Err(self.wrap(ctx.to_string(), crate::xml::bad_char_error(i, s).into()));
        }
        self.w
            .text(self.ex, s)
            .map_err(|e| self.wrap(ctx.to_string(), e))
    }

    fn put_attribute(
        &mut self,
        ctx: &str,
        local: &str,
        ns: Option<&str>,
        s: &str,
    ) -> CallResult<()> {
        self.w
            .attribute(self.ex, local, ns, s)
            .map_err(|e| self.wrap(ctx.to_string(), e))
    }

    #[allow(clippy::too_many_arguments)]
    fn element(
        &mut self,
        ns: &str,
        local: &str,
        declared: Option<&QName>,
        d: &XdtoData,
        root: bool,
        prop: Option<&EProp>,
    ) -> CallResult<()> {
        self.ex.charge(1)?;
        if self.guard.enter().is_err() {
            return Err(cycle_fatal(self.ex).into());
        }
        let ctx = self.context(prop, ns, local);
        let depth = self.w.depth() + 1;
        let mut gen_n = 0u32;
        let ns_opt = (!ns.is_empty()).then_some(ns);
        self.w.start_element(self.ex, local, ns_opt)?;
        if root && ns.is_empty() && self.w.lookup_uri("").is_none() {
            self.w.namespace(self.ex, "", "")?;
        }
        let qn = self.qualified_names(d, depth, &mut gen_n)?;
        if root {
            if ns != NS_XS && self.w.prefix_of(NS_XS).is_none() {
                self.w.namespace(self.ex, "xs", NS_XS)?;
            }
            if self.w.prefix_of(NS_XSI).is_none() {
                self.w.namespace(self.ex, "xsi", NS_XSI)?;
            }
        }
        let actual = match d {
            XdtoData::Value { ty, .. } | XdtoData::Object { ty, .. } => Some(ty),
            XdtoData::Nil => None,
        };
        let forced_child = if root {
            false
        } else {
            host_child_explicit(d, declared, self.f.untyped)?
        };
        if let Some(ty) = actual
            && !self.f.untyped
            && (if root {
                self.f.explicit_type
            } else {
                declared != Some(ty) || forced_child
            })
        {
            let text = self.qname_text(depth, &mut gen_n, &ty.clark())?;
            self.put_attribute(&ctx, "type", Some(NS_XSI), &text)?;
        }
        match d {
            XdtoData::Nil => self.put_attribute(&ctx, "nil", Some(NS_XSI), "true")?,
            XdtoData::Value { lexical, .. } => {
                let text = qn
                    .as_ref()
                    .and_then(|q| q.text.as_deref())
                    .unwrap_or(lexical);
                if !text.is_empty() {
                    self.put_text(&ctx, text)?;
                }
            }
            XdtoData::Object { ty, props } => {
                let attrs = qn.as_ref().map_or(&[][..], |q| q.attrs.as_slice());
                self.object_body(ty, props, &ctx, attrs)?
            }
        }
        self.w.end_element(self.ex)?;
        self.guard.leave();
        Ok(())
    }

    #[inline(never)]
    fn qualified_names(
        &mut self,
        d: &XdtoData,
        depth: u64,
        gen_n: &mut u32,
    ) -> CallResult<Option<Box<QNames>>> {
        Ok(match d {
            XdtoData::Value { ty, lexical, .. } if is_qname_value(self.m, ty, lexical) => {
                Some(Box::new(QNames {
                    text: Some(self.qname_text(depth, gen_n, lexical)?),
                    attrs: Vec::new(),
                }))
            }
            XdtoData::Object { ty, props } => {
                let attrs = self.qname_attributes(ty, props, depth, gen_n)?;
                (!attrs.is_empty()).then(|| Box::new(QNames { text: None, attrs }))
            }
            _ => None,
        })
    }

    fn qname_attributes(
        &mut self,
        ty: &QName,
        props: &[(String, Vec<XdtoData>)],
        depth: u64,
        gen_n: &mut u32,
    ) -> CallResult<Vec<(String, String)>> {
        let m = self.m;
        let mut out = Vec::new();
        if !props.iter().any(|(_, items)| {
            items.iter().any(|it| {
                matches!(it, XdtoData::Value { ty, lexical, .. } if is_qname_value(m, ty, lexical))
            })
        }) {
            return Ok(out);
        }
        let eps = self.cache.get(self.ex, m, ty)?;
        for ep in eps.iter().filter(|p| p.prop.form == Form::Attribute) {
            let Some((_, items)) = props.iter().find(|(n, _)| n == &ep.prop.name) else {
                continue;
            };
            if let [XdtoData::Value { ty, lexical, .. }] = items.as_slice()
                && is_qname_value(m, ty, lexical)
            {
                let preferred = attribute_prefix(clark_parts(lexical).0);
                let text = self.qname_text_as(depth, gen_n, lexical, preferred)?;
                out.push((ep.prop.name.clone(), text));
            }
        }
        Ok(out)
    }

    #[inline(never)]
    fn attributes(
        &mut self,
        ctx: &str,
        eps: &[EProp],
        props: &[(String, Vec<XdtoData>)],
        attr_names: &[(String, String)],
    ) -> CallResult<()> {
        for ep in eps.iter().filter(|p| p.prop.form == Form::Attribute) {
            let items = props
                .iter()
                .find(|(n, _)| n == &ep.prop.name)
                .map_or(&[][..], |(_, items)| items.as_slice());
            single_value(self.ex, ep, items.len())?;
            for it in items {
                match it {
                    XdtoData::Value { lexical, .. } => {
                        let text = attr_names
                            .iter()
                            .find(|(n, _)| n == &ep.prop.name)
                            .map_or(lexical.as_str(), |(_, t)| t.as_str());
                        self.put_attribute(ctx, ep.xml_name(), ep.xml_ns(), text)?;
                    }
                    _ => return Err(crate::xdto::data::property_type_error().into()),
                }
            }
        }
        Ok(())
    }

    #[inline(never)]
    fn prepare_body(
        &mut self,
        ty: &QName,
        props: &[(String, Vec<XdtoData>)],
        ctx: &str,
        attr_names: &[(String, String)],
    ) -> CallResult<std::sync::Arc<Vec<EProp>>> {
        let eps = self.cache.get(self.ex, self.m, ty)?;
        if object_type(self.m, ty).is_some_and(|o| o.sequenced) {
            return Err(unsupported("Последовательность").into());
        }
        for (name, _) in props {
            if !eps.iter().any(|p| &p.prop.name == name) {
                return Err(args_error(msg::XDTO_UNKNOWN_PROPERTY, vec![name.clone()]).into());
            }
        }
        self.attributes(ctx, &eps, props, attr_names)?;
        Ok(eps)
    }

    fn object_body(
        &mut self,
        ty: &QName,
        props: &[(String, Vec<XdtoData>)],
        ctx: &str,
        attr_names: &[(String, String)],
    ) -> CallResult<()> {
        let eps = self.prepare_body(ty, props, ctx, attr_names)?;
        let ordered = object_type(self.m, ty).is_none_or(|o| o.ordered);
        let len = if ordered { eps.len() } else { props.len() };
        for i in 0..len {
            let ep = if ordered {
                eps.get(i)
            } else {
                props
                    .get(i)
                    .and_then(|(name, _)| eps.iter().find(|p| &p.prop.name == name))
            };
            let Some(ep) = ep.filter(|p| p.prop.form != Form::Attribute) else {
                continue;
            };
            let items = props
                .iter()
                .find(|(n, _)| n == &ep.prop.name)
                .map_or(&[][..], |(_, items)| items.as_slice());
            single_value(self.ex, ep, items.len())?;
            for it in items {
                if ep.prop.form == Form::Text {
                    match it {
                        XdtoData::Value { lexical, .. } => self.put_text(ctx, lexical)?,
                        _ => return Err(crate::xdto::data::property_type_error().into()),
                    }
                    continue;
                }
                self.element(
                    ep.xml_ns().unwrap_or(""),
                    ep.xml_name(),
                    Some(&ep.prop.ty),
                    it,
                    false,
                    Some(ep),
                )?;
            }
        }
        Ok(())
    }
}

struct Frame {
    ty: QName,
    eps: std::sync::Arc<Vec<EProp>>,
    open: bool,
    ordered: bool,
    mixed: bool,
    slots: Vec<Vec<XdtoData>>,
    order: Vec<usize>,
    text: String,
    violation: Option<(EProp, String)>,
    extra: Option<(String, String)>,
    last: usize,
    child: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ReadMode {
    Objects,
    Values,
}

struct In<'a> {
    ex: &'a mut Execution,
    m: &'a Model,
    mode: ReadMode,
    r: &'a mut XmlReaderState,
    guard: DepthGuard,
    bytes: u64,
    stack: Vec<Frame>,
    cache: PropCache,
}

enum Step {
    Done(XdtoData),
    Opened,
}

pub(crate) fn read_xml(
    ex: &mut Execution,
    m: &Model,
    r: &mut XmlReaderState,
    expect: Option<&QName>,
) -> CallResult<XdtoData> {
    read_xml_mode(ex, m, r, expect, ReadMode::Objects)
}

pub(crate) fn read_xml_mode(
    ex: &mut Execution,
    m: &Model,
    r: &mut XmlReaderState,
    expect: Option<&QName>,
    mode: ReadMode,
) -> CallResult<XdtoData> {
    r.move_to_content(ex, false)?;
    let (ns, local) = match r.node() {
        Some(XmlEvent::Start { ns, local, .. }) => (ns.clone(), local.clone()),
        _ => return Err(xml_error(msg::XML_INVALID_DATA).into()),
    };
    let declared = match expect {
        Some(q) => q.clone(),
        None => match m
            .package(&ns)
            .and_then(|p| p.globals.iter().find(|g| g.name == local))
        {
            Some(g) => g.ty.clone(),
            None if m.type_def(&QName::new(&ns, &local)).is_some() => QName::new(&ns, &local),
            None => any_type(),
        },
    };
    let mut inp = In {
        ex,
        m,
        mode,
        r,
        guard: DepthGuard::new(),
        bytes: 0,
        stack: Vec::new(),
        cache: PropCache::default(),
    };
    inp.run(&declared)
}

fn too_deep() -> CallError {
    xml_error(msg::XML_INVALID_DATA).into()
}

enum Next {
    Start(String, String),
    End,
    Text(String),
    Other,
    Eof,
}

impl In<'_> {
    fn grow(&mut self, add: usize) -> CallResult<()> {
        self.bytes = self.bytes.saturating_add(add as u64 + 64);
        self.ex.reserve(self.bytes.saturating_mul(3))?;
        Ok(())
    }

    fn run(&mut self, declared: &QName) -> CallResult<XdtoData> {
        let mut done = match self.begin(declared)? {
            Step::Done(d) => Some(d),
            Step::Opened => None,
        };
        loop {
            if let Some(d) = done.take() {
                match self.stack.last_mut() {
                    None => return Ok(d),
                    Some(f) => {
                        let c = f.child;
                        f.slots[c].push(d);
                    }
                }
            }
            done = self.content()?;
        }
    }

    fn content(&mut self) -> CallResult<Option<XdtoData>> {
        let next = match self.r.node() {
            Some(XmlEvent::Start { ns, local, .. }) => Next::Start(ns.clone(), local.clone()),
            Some(XmlEvent::End { .. }) => Next::End,
            Some(XmlEvent::Text(t) | XmlEvent::CData(t)) => Next::Text(t.clone()),
            Some(_) => Next::Other,
            None => Next::Eof,
        };
        match next {
            Next::Eof => Err(xml_error(msg::XML_INVALID_DATA).into()),
            Next::End => self.close().map(Some),
            Next::Text(t) => {
                self.grow(t.len())?;
                if let Some(f) = self.stack.last_mut() {
                    f.text.push_str(&t);
                }
                self.r.read(self.ex)?;
                Ok(None)
            }
            Next::Other => {
                self.r.read(self.ex)?;
                Ok(None)
            }
            Next::Start(ns, local) => self.child(&ns, local),
        }
    }

    fn child(&mut self, ns: &str, local: String) -> CallResult<Option<XdtoData>> {
        let found = {
            let Some(f) = self.stack.last_mut() else {
                return Err(too_deep());
            };
            let found = f.eps.iter().position(|ep| {
                ep.prop.form == Form::Element
                    && ep.xml_name() == local
                    && ep.xml_ns().unwrap_or("") == ns
            });
            match found {
                Some(i) => {
                    if f.violation.is_none() && f.ordered {
                        if i < f.last {
                            f.violation = Some((
                                f.eps[i].clone(),
                                text_for(self.ex, msg::XDTO_UNORDERED, &[]),
                            ));
                        } else if let Some(j) = (f.last..i).find(|&j| {
                            f.eps[j].prop.form == Form::Element
                                && f.eps[j].prop.lower > 0
                                && f.slots[j].is_empty()
                        }) {
                            f.violation = Some((
                                f.eps[i].clone(),
                                expected_line(self.ex, &[f.eps[j].prop.name.clone()]),
                            ));
                        }
                    }
                    let count = f.slots[i].len();
                    let upper = f.eps[i].prop.upper;
                    if f.violation.is_none() && upper >= 0 && count as u64 >= upper as u64 {
                        f.violation = Some((
                            f.eps[i].clone(),
                            text_for(
                                self.ex,
                                msg::XDTO_UPPER,
                                &[(count + 1).to_string(), upper.to_string()],
                            ),
                        ));
                    }
                    if count == 0 {
                        f.order.push(i);
                    }
                    f.last = f.last.max(i);
                    f.child = i;
                    Some((i, f.eps[i].prop.ty.clone(), f.open))
                }
                None if f.open => None,
                None if self.mode == ReadMode::Objects => {
                    if f.extra.is_none() {
                        f.extra = Some((ns.to_string(), local));
                    }
                    None
                }
                None => {
                    return Err(args_error(msg::XDTO_UNKNOWN_PROPERTY, vec![local]).into());
                }
            }
        };
        match found {
            Some((_, declared, _)) => match self.begin(&declared)? {
                Step::Done(d) => Ok(Some(d)),
                Step::Opened => Ok(None),
            },
            None => {
                self.r.skip(self.ex)?;
                self.r.read(self.ex)?;
                Ok(None)
            }
        }
    }

    fn undefined_type(&mut self, text: &str) -> CallError {
        let name = match self.r.node() {
            Some(XmlEvent::Start { local, .. }) => local.clone(),
            _ => String::new(),
        };
        let (line, col) = match self.r.position_after_element(self.ex) {
            Ok(p) => p,
            Err(e) => return e,
        };
        let prefix = text.split_once(':').map_or("", |(p, _)| p);
        let lang = self.ex.lang();
        let custom = text_for(
            self.ex,
            msg::XDTO_CHECK_CUSTOM,
            &[form_name(lang, Form::Element).to_string(), name],
        );
        let mapping = {
            let line = text_for(self.ex, msg::XDTO_NS_MAPPING_PREFIX, &[prefix.to_string()]);
            with_other(
                key_error(msg::XDTO_NS_MAPPING, Vec::new(), format!("\n{line}")),
                false,
            )
        };
        let undefined = {
            let line = text_for(self.ex, msg::XDTO_UNDEFINED_TYPE_NAME, &[text.to_string()]);
            with_other(
                key_error(msg::XDTO_UNDEFINED_TYPE, Vec::new(), format!("\n{line}")),
                false,
            )
            .caused_by(Cause::Host(Box::new(mapping)))
        };
        headed(
            msg::XDTO_SERIALIZATION,
            &format!(" - [{line},{col}]\n{custom}"),
        )
        .caused_by(Cause::Host(Box::new(undefined)))
        .into()
    }

    fn begin(&mut self, declared: &QName) -> CallResult<Step> {
        self.ex.charge(1)?;
        if !self.r.at_start() {
            return Err(xml_error(msg::XML_INVALID_DATA).into());
        }
        let xsi_type = self.r.xsi_type();
        let explicit_type = xsi_type.is_some();
        let ty = match xsi_type {
            Some((ns, local)) => {
                let q = QName::new(&ns, &local);
                if self.m.type_def(&q).is_none() && !is_any(&q) {
                    if self.mode == ReadMode::Objects {
                        return Err(args_error(msg::XDTO_TYPE_NOT_FOUND, vec![q.clark()]).into());
                    }
                    match self.r.xsi_type_text() {
                        Some((text, false)) => return Err(self.undefined_type(&text)),
                        _ => any_type(),
                    }
                } else {
                    q
                }
            }
            None => declared.clone(),
        };
        if self.r.is_nil() {
            self.r.read_simple(self.ex, false)?;
            return Ok(Step::Done(XdtoData::Nil));
        }
        if object_type(self.m, &ty).is_some() && !is_any(&ty) {
            self.open(ty)?;
            return Ok(Step::Opened);
        }
        if value_type(self.m, &ty).is_some() || is_any(&ty) {
            let was_any = is_any(&ty);
            let ty = if was_any {
                QName::new(NS_XS, "string")
            } else {
                ty
            };
            let here = match self.r.node() {
                Some(XmlEvent::Start { ns, local, .. }) => Some((ns.clone(), local.clone())),
                _ => None,
            };
            return match self.simple(&ty, explicit_type) {
                Err(CallError::Host(_)) if self.mode == ReadMode::Values && was_any => {
                    match (here, self.r.stray_child().cloned()) {
                        (Some(a), Some(b)) => {
                            let ctx = vec![
                                (QName::new(&a.0, &a.1).clark(), any_type()),
                                (QName::new(&b.0, &b.1).clark(), any_type()),
                            ];
                            Err(super::reader_context_chain(self.ex, ctx)
                                .map_or_else(too_deep, CallError::Host))
                        }
                        _ => Err(too_deep()),
                    }
                }
                r => r,
            }
            .map(Step::Done);
        }
        Err(args_error(msg::XDTO_TYPE_NOT_FOUND, vec![ty.clark()]).into())
    }

    fn source_projection(
        &mut self,
        text: &str,
        explicit_type: bool,
    ) -> CallResult<Option<ScalarProjection>> {
        if self.mode != ReadMode::Values {
            return Ok(None);
        }
        self.grow(
            text.len()
                .saturating_add(std::mem::size_of::<ScalarProjection>()),
        )?;
        self.ex.charge(1 + text.len() as u64 / 64)?;
        Ok(Some(ScalarProjection::Read {
            explicit_type,
            source_text: text.to_string(),
        }))
    }

    fn simple(&mut self, ty: &QName, explicit_type: bool) -> CallResult<XdtoData> {
        let qname = is_qname_type(self.m, ty);
        let qunion = !qname && is_qname_union(self.m, ty);
        let s = self.r.read_simple(self.ex, qname || qunion)?;
        self.grow(s.text.len())?;
        let projection = self.source_projection(&s.text, explicit_type)?;
        let mut d = match value_from_text(self.ex, self.m, ty, &s.text, false) {
            Ok(d) => d,
            Err(CallError::Host(_))
                if self.mode == ReadMode::Values && qunion && super::value_map::has_codec(ty) =>
            {
                return Ok(XdtoData::Value {
                    projection,
                    ty: ty.clone(),
                    lexical: s.text.clone(),
                    value: bsl_vm::Value::from(s.text.as_str()),
                });
            }
            Err(CallError::Host(_)) if self.mode == ReadMode::Values => {
                return Err(super::value_map::mapping_error(self.ex, self.m, &s.text, ty).into());
            }
            Err(e) => return Err(e),
        };
        if qunion {
            let t = s.text.trim();
            if !super::takes_without_qname(self.m, ty, t)
                && let Some((_, local)) = split_qname(t)
                && let Some(ns) = s.prefix_ns.as_deref()
                && let XdtoData::Value { lexical, value, .. } = &mut d
            {
                *lexical = QName::new(ns, local).clark();
                *value = bsl_vm::Value::from(lexical.as_str());
            }
        }
        if qname {
            let t = s.text.trim();
            let (prefix, local) = split_qname(t).ok_or_else(|| xml_error(msg::XML_CONVERSION))?;
            let ns = match (prefix, s.prefix_ns) {
                (_, Some(ns)) => ns,
                ("", None) => String::new(),
                (_, None) => return Err(xml_error(msg::XML_CONVERSION).into()),
            };
            if let XdtoData::Value { lexical, value, .. } = &mut d {
                *lexical = QName::new(&ns, local).clark();
                *value = bsl_vm::Value::from(lexical.as_str());
            }
        }
        if let XdtoData::Value {
            projection: stored, ..
        } = &mut d
        {
            *stored = projection;
        }
        Ok(d)
    }

    fn open(&mut self, ty: QName) -> CallResult<()> {
        if self.guard.enter().is_err() {
            return Err(too_deep());
        }
        let m = self.m;
        let eps = self.cache.get(self.ex, m, &ty)?;
        let def = object_type(m, &ty);
        let open = def.is_some_and(|o| o.open);
        let ordered = def.is_none_or(|o| o.ordered);
        let mixed = def.is_some_and(|o| o.mixed);
        if def.is_some_and(|o| o.sequenced) {
            return Err(unsupported("Последовательность").into());
        }
        self.grow(eps.len() * 56)?;
        let mut slots: Vec<Vec<XdtoData>> = vec![Vec::new(); eps.len()];
        let attrs: Vec<(String, String, String)> = self
            .r
            .attributes()
            .into_iter()
            .filter(|a| a.ns != NS_XSI)
            .map(|a| (a.ns.clone(), a.local.clone(), a.value.clone()))
            .collect();
        for (i, ep) in eps.iter().enumerate() {
            if ep.prop.form != Form::Attribute {
                continue;
            }
            let ns = ep.xml_ns().unwrap_or("");
            if let Some((_, _, v)) = attrs
                .iter()
                .find(|(ans, al, _)| ans == ns && al == ep.xml_name())
            {
                self.grow(v.len())?;
                let projection = self.source_projection(v, false)?;
                let mut d = match value_from_text(self.ex, m, &ep.prop.ty, v, false) {
                    Ok(mut d) => {
                        let t = v.trim();
                        if is_qname_union(m, &ep.prop.ty)
                            && !super::takes_without_qname(m, &ep.prop.ty, t)
                            && let Some((prefix, local)) = split_qname(t)
                            && let Some(ns) = self.r.lookup_namespace(prefix)
                            && let XdtoData::Value { lexical, value, .. } = &mut d
                        {
                            *lexical = QName::new(ns, local).clark();
                            *value = bsl_vm::Value::from(lexical.as_str());
                        }
                        d
                    }
                    Err(CallError::Host(_)) if self.mode == ReadMode::Values => XdtoData::Value {
                        projection: None,
                        ty: QName::new(NS_XS, "string"),
                        lexical: v.clone(),
                        value: bsl_vm::Value::from(v.as_str()),
                    },
                    Err(e) => return Err(e),
                };
                if let XdtoData::Value {
                    projection: stored, ..
                } = &mut d
                {
                    *stored = projection;
                }
                slots[i].push(d);
            }
        }
        if !open
            && let Some((_, l, _)) = attrs.iter().find(|(ans, al, _)| {
                !eps.iter().any(|ep| {
                    ep.prop.form == Form::Attribute
                        && ep.xml_name() == al
                        && ep.xml_ns().unwrap_or("") == ans
                })
            })
        {
            return Err(args_error(msg::XDTO_UNKNOWN_PROPERTY, vec![l.clone()]).into());
        }
        self.r.read(self.ex)?;
        let mut order = Vec::with_capacity(eps.len());
        order.extend(
            slots
                .iter()
                .enumerate()
                .filter(|(_, slot)| !slot.is_empty())
                .map(|(i, _)| i),
        );
        self.stack.push(Frame {
            ty,
            eps,
            ordered,
            mixed,
            open,
            slots,
            order,
            text: String::new(),
            violation: None,
            extra: None,
            last: 0,
            child: 0,
        });
        Ok(())
    }

    fn close(&mut self) -> CallResult<XdtoData> {
        let pos = self.r.end_tag_position();
        let Some(mut f) = self.stack.pop() else {
            return Err(too_deep());
        };
        self.guard.leave();
        let m = self.m;
        if let Some(i) = f.eps.iter().position(|p| p.prop.form == Form::Text)
            && (!f.text.trim().is_empty() || f.eps[i].prop.lower > 0)
        {
            let projection = self.source_projection(&f.text, false)?;
            let mut d = value_from_text(self.ex, m, &f.eps[i].prop.ty, &f.text, false)?;
            if let XdtoData::Value {
                projection: stored, ..
            } = &mut d
            {
                *stored = projection;
            }
            if f.slots[i].is_empty() {
                f.order.push(i);
            }
            f.slots[i].push(d);
        }
        if !f.eps.iter().any(|p| p.prop.form == Form::Text) && !f.text.trim().is_empty() {
            if f.mixed {
                return Err(unsupported("Смешанное содержимое").into());
            }
            return Err(xml_error(msg::XML_INVALID_DATA).into());
        }
        if let Some((ns, local)) = f.extra.take() {
            let name = if ns.is_empty() {
                local
            } else {
                QName::new(&ns, &local).clark()
            };
            let custom = text_for(
                self.ex,
                msg::XDTO_CHECK_CUSTOM,
                &[form_name(self.ex.lang(), Form::Element).to_string(), name],
            );
            let body = format!(
                "{} - [{},{}]\n{custom}",
                text_for(self.ex, msg::XDTO_READ_OBJECT, &[f.ty.clark()]),
                pos.0,
                pos.1,
            );
            let structure = text_for(self.ex, msg::XDTO_STRUCTURE, &[f.ty.clark()]);
            return Err(headed(msg::XDTO_SERIALIZATION, &body)
                .caused_by(Cause::Host(Box::new(headed(
                    msg::XDTO_VALIDATION,
                    &structure,
                ))))
                .into());
        }
        let failed = f.violation.take().map(|(at, line)| {
            let e = structure_error(self.ex, &f.ty, &at, &line);
            (at, e)
        });
        if let Some((at, inner)) = failed {
            let body = format!(
                "{} - [{},{}]\n{}",
                text_for(self.ex, msg::XDTO_READ_OBJECT, &[f.ty.clark()]),
                pos.0,
                pos.1,
                property_block(self.ex, &at)
            );
            return Err(headed(msg::XDTO_SERIALIZATION, &body)
                .caused_by(Cause::Host(Box::new(inner)))
                .into());
        }
        if self.mode == ReadMode::Objects
            && let Some((_, property)) = f
                .eps
                .iter()
                .enumerate()
                .find(|(index, property)| property.prop.lower > 0 && f.slots[*index].is_empty())
        {
            return Err(structure_error(
                self.ex,
                &f.ty,
                property,
                &text_for(self.ex, msg::XDTO_REQUIRED, &[]),
            )
            .into());
        }
        self.r.read(self.ex)?;
        let order = if f.ordered {
            (0..f.eps.len()).collect()
        } else {
            f.order
        };
        let props = order
            .into_iter()
            .filter_map(|index| {
                let values = std::mem::take(&mut f.slots[index]);
                (!values.is_empty()).then(|| (f.eps[index].prop.name.clone(), values))
            })
            .collect();
        Ok(XdtoData::Object { ty: f.ty, props })
    }
}
