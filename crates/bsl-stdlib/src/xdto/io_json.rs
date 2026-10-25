use super::data::{
    ScalarProjection, XdtoData, XdtoWriteFlags, host_child_explicit, validate_host_typing,
    value_from_text,
};
use super::io_xml::ReadMode;
use super::{
    PropCache, any_type, args_error, check_structure, is_any, object_type, plain_error,
    single_value, unsupported, value_type,
};
use crate::bind::{lib_error, msg};
use crate::serial::cycle_fatal;
use crate::serial::depth::DepthGuard;
use bsl_lib::json::{Token, Value as JVal};
use bsl_lib::xdto::{Model, QName};
use bsl_prim::Number;
use bsl_vm::{CallError, CallResult, Execution, HostError, Value};

const NS_CORE: &str = "http://v8.1c.ru/8.1/data/core";
const NS_CONFIG: &str = "http://v8.1c.ru/8.1/data/enterprise/current-config";
const NS_UI: &str = "http://v8.1c.ru/8.1/data/ui";

fn le(e: bsl_lib::LibError) -> CallError {
    lib_error(e).into()
}

fn type_text(ty: &QName) -> String {
    match ty.ns.as_str() {
        super::XS => format!("jxs:{}", ty.name),
        NS_CORE => format!("jv8:{}", ty.name),
        NS_CONFIG => format!("jcfg:{}", ty.name),
        NS_UI => format!("jv8ui:{}", ty.name),
        _ => ty.clark(),
    }
}

struct Out<'a> {
    ex: &'a mut Execution,
    m: &'a Model,
    wv: &'a Value,
    f: XdtoWriteFlags,
    guard: DepthGuard,
    depth: u64,
    cache: PropCache,
}

pub(crate) fn write_json(
    ex: &mut Execution,
    m: &Model,
    wv: &Value,
    d: &XdtoData,
    f: XdtoWriteFlags,
) -> CallResult<()> {
    validate_host_typing(ex, m, d, f)?;
    let mut out = Out {
        ex,
        m,
        wv,
        f,
        guard: DepthGuard::new(),
        depth: 0,
        cache: PropCache::default(),
    };
    out.open(false)?;
    let union_root = matches!(d, XdtoData::Value { ty, .. } if super::is_union(m, ty));
    if let XdtoData::Value { ty, .. } | XdtoData::Object { ty, .. } = d
        && (f.explicit_type || union_root)
        && !f.untyped
    {
        out.name("#type")?;
        out.string(&type_text(ty))?;
    }
    out.name("#value")?;
    out.body(d)?;
    out.close(false)
}

impl Out<'_> {
    fn open(&mut self, array: bool) -> CallResult<()> {
        if self.guard.enter().is_err() {
            return Err(cycle_fatal(self.ex).into());
        }
        self.account(2)?;
        let mut w = crate::json::json_writer(self.ex, self.wv)?;
        if array {
            w.start_array().map_err(le)?;
        } else {
            w.start_object().map_err(le)?;
        }
        self.depth += 1;
        Ok(())
    }

    fn close(&mut self, array: bool) -> CallResult<()> {
        self.depth -= 1;
        self.account(2)?;
        let mut w = crate::json::json_writer(self.ex, self.wv)?;
        if array {
            w.end_array().map_err(le)?;
        } else {
            w.end_object().map_err(le)?;
        }
        self.guard.leave();
        Ok(())
    }

    fn account(&mut self, add: u64) -> CallResult<()> {
        self.ex.charge(1)?;
        crate::json::json_account(self.ex, self.wv, add, self.depth)
    }

    fn name(&mut self, n: &str) -> CallResult<()> {
        self.account((n.len() as u64).saturating_mul(6).saturating_add(4))?;
        let u: Vec<u16> = n.encode_utf16().collect();
        crate::json::json_writer(self.ex, self.wv)?
            .property_name(&u)
            .map_err(le)
    }

    fn string(&mut self, s: &str) -> CallResult<()> {
        self.account((s.len() as u64).saturating_mul(6).saturating_add(2))?;
        let u: Vec<u16> = s.encode_utf16().collect();
        crate::json::json_writer(self.ex, self.wv)?
            .string(&u)
            .map_err(le)
    }

    fn scalar(&mut self, ty: &QName, lexical: &str, value: &Value) -> CallResult<()> {
        match super::builtin_root(self.m, ty).as_deref() {
            Some("decimal") => {
                let n = match value {
                    Value::Number(n) => Some(n.clone()),
                    _ => Number::parse(lexical),
                };
                match n {
                    Some(n) => {
                        self.account(64)?;
                        crate::json::json_writer(self.ex, self.wv)?
                            .number(&n)
                            .map_err(le)
                    }
                    None => self.string(lexical),
                }
            }
            Some("boolean") => {
                self.account(5)?;
                crate::json::json_writer(self.ex, self.wv)?
                    .bool(lexical == "true")
                    .map_err(le)
            }
            _ => self.string(lexical),
        }
    }

    fn body(&mut self, d: &XdtoData) -> CallResult<()> {
        match d {
            XdtoData::Nil => {
                self.account(4)?;
                crate::json::json_writer(self.ex, self.wv)?
                    .null()
                    .map_err(le)
            }
            XdtoData::Value {
                ty, lexical, value, ..
            } => self.scalar(ty, lexical, value),
            XdtoData::Object { ty, props } => {
                let eps = self.cache.get(self.ex, self.m, ty)?;
                if object_type(self.m, ty).is_some_and(|o| o.sequenced) {
                    return Err(unsupported("Последовательность").into());
                }
                for (n, _) in props {
                    if !eps.iter().any(|p| &p.prop.name == n) {
                        return Err(args_error(msg::XDTO_UNKNOWN_PROPERTY, vec![n.clone()]).into());
                    }
                }
                if let [only] = eps.as_slice()
                    && only.is_list()
                {
                    let items = props.first().map_or(&[][..], |(_, v)| v.as_slice());
                    return self.array(&only.prop.ty, items);
                }
                self.open(false)?;
                for ep in eps.iter() {
                    let Some((_, items)) = props.iter().find(|(n, _)| n == &ep.prop.name) else {
                        continue;
                    };
                    single_value(self.ex, ep, items.len())?;
                    self.name(ep.json_name())?;
                    if ep.is_list() {
                        self.array(&ep.prop.ty, items)?;
                    } else if let Some(it) = items.first() {
                        self.typed(&ep.prop.ty, it)?;
                    }
                }
                self.close(false)
            }
        }
    }

    fn array(&mut self, declared: &QName, items: &[XdtoData]) -> CallResult<()> {
        self.open(true)?;
        for it in items {
            self.typed(declared, it)?;
        }
        self.close(true)
    }

    fn typed(&mut self, declared: &QName, d: &XdtoData) -> CallResult<()> {
        let actual = match d {
            XdtoData::Value { ty, .. } | XdtoData::Object { ty, .. } => Some(ty),
            XdtoData::Nil => None,
        };
        let forced_child = host_child_explicit(d, Some(declared), self.f.untyped)?;
        match actual {
            Some(ty) if (ty != declared || forced_child) && !self.f.untyped => {
                self.open(false)?;
                self.name("#type")?;
                self.string(&type_text(ty))?;
                self.name("#value")?;
                self.body(d)?;
                self.close(false)
            }
            _ => self.body(d),
        }
    }
}

fn bad_state() -> CallError {
    HostError::key(bsl_lib::msg::BAD_STATE).into()
}

fn tree(ex: &mut Execution, rv: &Value) -> CallResult<(JVal, u64)> {
    enum Frame {
        Arr(Vec<JVal>),
        Obj(Vec<(Vec<u16>, JVal)>, Option<Vec<u16>>),
    }
    let first = {
        let mut s = crate::json::json_reader(ex, rv)?;
        let positioned = !s.consumed
            && match &s.current {
                Some(
                    Token::StartObject
                    | Token::StartArray
                    | Token::String(_)
                    | Token::Number(_)
                    | Token::Bool(_)
                    | Token::Null,
                ) => true,
                Some(_) => return Err(bad_state()),
                None => false,
            };
        match (positioned, s.current.clone()) {
            (true, Some(t)) => t,
            _ => {
                s.consumed = false;
                s.reader.next().map_err(le)?.ok_or_else(bad_state)?
            }
        }
    };
    let mut guard = DepthGuard::new();
    let depth_error = || -> CallError { HostError::key(crate::json::msg::MAX_DEPTH).into() };
    let mut stack: Vec<Frame> = Vec::new();
    let mut tok = first;
    let mut bytes = 0u64;
    let mut last = None;
    loop {
        ex.charge(1)?;
        let done: Option<JVal> = match tok {
            Token::StartArray | Token::StartObject => {
                if guard.enter().is_err() {
                    return Err(depth_error());
                }
                bytes = bytes.saturating_add(96);
                ex.reserve(bytes.saturating_mul(2))?;
                stack.push(if matches!(tok, Token::StartArray) {
                    Frame::Arr(Vec::new())
                } else {
                    Frame::Obj(Vec::new(), None)
                });
                None
            }
            Token::EndArray | Token::EndObject => {
                guard.leave();
                last = Some(if matches!(tok, Token::EndArray) {
                    Token::EndArray
                } else {
                    Token::EndObject
                });
                match stack.pop() {
                    Some(Frame::Arr(v)) => Some(JVal::Array(v)),
                    Some(Frame::Obj(p, _)) => Some(JVal::Object(p)),
                    None => return Err(bad_state()),
                }
            }
            Token::PropertyName(k) => {
                bytes = bytes.saturating_add(k.len() as u64 * 2 + 48);
                ex.reserve(bytes.saturating_mul(2))?;
                match stack.last_mut() {
                    Some(Frame::Obj(_, key)) => *key = Some(k),
                    _ => return Err(bad_state()),
                }
                None
            }
            scalar => {
                if stack.is_empty() {
                    last = Some(scalar.clone());
                }
                bytes = bytes.saturating_add(64);
                if let Token::String(s) = &scalar {
                    bytes = bytes.saturating_add(s.len() as u64 * 2);
                }
                ex.reserve(bytes.saturating_mul(2))?;
                Some(match scalar {
                    Token::Bool(b) => JVal::Bool(b),
                    Token::Number(n) => JVal::Number(n),
                    Token::String(s) => JVal::String(s),
                    _ => JVal::Null,
                })
            }
        };
        if let Some(v) = done {
            match stack.last_mut() {
                None => {
                    let mut s = crate::json::json_reader(ex, rv)?;
                    s.current = last;
                    s.consumed = true;
                    return Ok((v, bytes.saturating_mul(2)));
                }
                Some(Frame::Arr(items)) => items.push(v),
                Some(Frame::Obj(pairs, key)) => {
                    let k = key.take().ok_or_else(bad_state)?;
                    pairs.push((k, v));
                }
            }
        }
        tok = crate::json::json_reader(ex, rv)?
            .reader
            .next()
            .map_err(le)?
            .ok_or_else(bad_state)?;
    }
}

fn member<'a>(pairs: &'a [(Vec<u16>, JVal)], name: &str) -> Option<&'a JVal> {
    let u: Vec<u16> = name.encode_utf16().collect();
    pairs.iter().find(|(k, _)| *k == u).map(|(_, v)| v)
}

struct In<'a> {
    ex: &'a mut Execution,
    m: &'a Model,
    guard: DepthGuard,
    cache: PropCache,
    ctx: Vec<(String, QName)>,
    mode: ReadMode,
    retained_bytes: u64,
}

pub(crate) fn read_json(
    ex: &mut Execution,
    m: &Model,
    rv: &Value,
    expect: Option<&QName>,
) -> CallResult<XdtoData> {
    read_json_mode(ex, m, rv, expect, ReadMode::Objects)
}

pub(crate) fn read_json_mode(
    ex: &mut Execution,
    m: &Model,
    rv: &Value,
    expect: Option<&QName>,
    mode: ReadMode,
) -> CallResult<XdtoData> {
    let (t, retained_bytes) = tree(ex, rv)?;
    let mut inp = In {
        ex,
        m,
        guard: DepthGuard::new(),
        cache: PropCache::default(),
        ctx: Vec::new(),
        mode,
        retained_bytes,
    };
    let declared = expect.cloned().unwrap_or_else(any_type);
    match &t {
        JVal::Object(pairs) if member(pairs, "#value").is_some() => {
            let ty = match member(pairs, "#type") {
                Some(tt) => inp.type_of(tt)?,
                None => declared,
            };
            inp.ctx.push((String::new(), ty.clone()));
            let v = member(pairs, "#value").unwrap_or(&JVal::Null);
            inp.body(&ty, v, member(pairs, "#type").is_some())
        }
        _ if expect.is_some() => {
            inp.ctx.push((String::new(), declared.clone()));
            inp.body(&declared, &t, false)
        }
        JVal::Object(_) | JVal::Array(_) => {
            inp.ctx.push((String::new(), declared));
            Err(inp.untyped_error(&t))
        }
        _ => Err(plain_error(msg::XDTOJSON_INVALID_TYPE).into()),
    }
}

impl In<'_> {
    fn type_of(&mut self, v: &JVal) -> CallResult<QName> {
        let JVal::String(u) = v else {
            return Err(plain_error(msg::XDTOJSON_INVALID_TYPE).into());
        };
        let text = String::from_utf16_lossy(u);
        let known = [
            ("jxs:", super::XS),
            ("jv8:", NS_CORE),
            ("jcfg:", NS_CONFIG),
            ("jv8ui:", NS_UI),
        ];
        for (p, ns) in known {
            if let Some(name) = text.strip_prefix(p) {
                let q = QName::new(ns, name);
                return if self.m.type_def(&q).is_some() || is_any(&q) {
                    Ok(q)
                } else {
                    Err(
                        args_error(msg::XDTOJSON_UNKNOWN_TYPE, vec![format!("{name} ({ns})")])
                            .into(),
                    )
                };
            }
        }
        if let Some((ns, name)) = text.strip_prefix('{').and_then(|r| r.split_once('}'))
            && ![super::XS, NS_CORE, NS_CONFIG, NS_UI].contains(&ns)
        {
            let q = QName::new(ns, name);
            if self.m.type_def(&q).is_some() {
                return Ok(q);
            }
        }
        Err(args_error(msg::XDTOJSON_UNKNOWN_TYPE, vec![text]).into())
    }

    fn untyped_error(&mut self, v: &JVal) -> CallError {
        let mut ctx = self.ctx.clone();
        match v {
            JVal::Object(p) => {
                if let Some((k, first)) = p.first() {
                    ctx.push((String::from_utf16_lossy(k), self.guess(first)));
                }
            }
            JVal::Array(a) => {
                if let Some(first) = a.first() {
                    ctx.push((String::new(), self.guess(first)));
                }
            }
            _ => {}
        }
        match super::reader_context_chain(self.ex, ctx) {
            Some(e) => e.into(),
            None => plain_error(msg::XDTOJSON_INVALID_TYPE).into(),
        }
    }

    fn guess(&self, v: &JVal) -> QName {
        match v {
            JVal::Object(p) => match member(p, "#type") {
                Some(JVal::String(u)) => {
                    let t = String::from_utf16_lossy(u);
                    let split = t.split_once(':');
                    match split {
                        Some(("jxs", n)) => QName::new(super::XS, n),
                        Some(("jv8", n)) => QName::new(NS_CORE, n),
                        Some(("jcfg", n)) => QName::new(NS_CONFIG, n),
                        Some(("jv8ui", n)) => QName::new(NS_UI, n),
                        _ => any_type(),
                    }
                }
                _ => any_type(),
            },
            JVal::Number(_) => QName::new(super::XS, "double"),
            JVal::String(_) => QName::new(super::XS, "string"),
            JVal::Bool(_) => QName::new(super::XS, "boolean"),
            _ => any_type(),
        }
    }

    fn item(&mut self, declared: &QName, v: &JVal) -> CallResult<XdtoData> {
        if let JVal::Object(p) = v
            && let (Some(t), Some(inner)) = (member(p, "#type"), member(p, "#value"))
        {
            let ty = self.type_of(t)?;
            return self.body(&ty, inner, true);
        }
        self.body(declared, v, false)
    }

    fn scalar_text(v: &JVal) -> Option<String> {
        match v {
            JVal::String(u) => Some(String::from_utf16_lossy(u)),
            JVal::Number(n) => Some(n.to_xml_string()),
            JVal::Bool(b) => Some(if *b { "true" } else { "false" }.to_string()),
            _ => None,
        }
    }

    fn body(&mut self, ty: &QName, v: &JVal, explicit_type: bool) -> CallResult<XdtoData> {
        self.ex.charge(1)?;
        if matches!(v, JVal::Null) {
            return Ok(XdtoData::Nil);
        }
        if object_type(self.m, ty).is_some() && !is_any(ty) {
            return match v {
                JVal::Object(pairs) => self.object(ty, pairs),
                JVal::Array(items) => self.list_object(ty, items),
                _ => Err(plain_error(msg::XDTOJSON_INVALID_TYPE).into()),
            };
        }
        let target = if is_any(ty) {
            match v {
                JVal::Number(_) => QName::new(super::XS, "decimal"),
                JVal::Bool(_) => QName::new(super::XS, "boolean"),
                JVal::String(_) => QName::new(super::XS, "string"),
                _ => return Err(self.untyped_error(v)),
            }
        } else if value_type(self.m, ty).is_some() {
            ty.clone()
        } else {
            return Err(args_error(msg::XDTO_TYPE_NOT_FOUND, vec![ty.clark()]).into());
        };
        if self.mode == ReadMode::Values {
            let source_bound = match v {
                JVal::String(units) => (units.len() as u64).saturating_mul(3),
                JVal::Number(n) => n.size_hint_limbs().saturating_mul(18).saturating_add(32),
                JVal::Bool(_) => 5,
                _ => 0,
            };
            self.retained_bytes = self
                .retained_bytes
                .saturating_add(source_bound.saturating_mul(3))
                .saturating_add(std::mem::size_of::<XdtoData>() as u64);
            self.ex.reserve(self.retained_bytes)?;
            self.ex.charge(1 + source_bound / 64)?;
        }
        let Some(text) = Self::scalar_text(v) else {
            return Err(plain_error(msg::XDTOJSON_INVALID_TYPE).into());
        };
        let mut data = match value_from_text(self.ex, self.m, &target, &text, false) {
            Err(CallError::Host(_))
                if text.starts_with('{') && super::is_qname_union(self.m, &target) =>
            {
                Ok(XdtoData::Value {
                    projection: None,
                    ty: target,
                    lexical: text.clone(),
                    value: Value::from(text.as_str()),
                })
            }
            r => r,
        }?;
        if self.mode == ReadMode::Values
            && let XdtoData::Value { projection, .. } = &mut data
        {
            *projection = Some(ScalarProjection::Read {
                explicit_type,
                source_text: text,
            });
        }
        Ok(data)
    }

    fn list_object(&mut self, ty: &QName, items: &[JVal]) -> CallResult<XdtoData> {
        let eps = self.cache.get(self.ex, self.m, ty)?;
        let [only] = eps.as_slice() else {
            return Err(plain_error(msg::XDTOJSON_INVALID_TYPE).into());
        };
        if !only.is_list() {
            return Err(plain_error(msg::XDTOJSON_INVALID_TYPE).into());
        }
        if self.guard.enter().is_err() {
            return Err(HostError::key(crate::json::msg::MAX_DEPTH).into());
        }
        self.ctx
            .push((only.json_name().to_string(), only.prop.ty.clone()));
        let mut out = Vec::with_capacity(items.len());
        for it in items {
            out.push(self.item(&only.prop.ty, it)?);
        }
        self.ctx.pop();
        self.guard.leave();
        let props = if out.is_empty() {
            Vec::new()
        } else {
            vec![(only.prop.name.clone(), out)]
        };
        Ok(XdtoData::Object {
            ty: ty.clone(),
            props,
        })
    }

    fn object(&mut self, ty: &QName, pairs: &[(Vec<u16>, JVal)]) -> CallResult<XdtoData> {
        if self.guard.enter().is_err() {
            return Err(HostError::key(crate::json::msg::MAX_DEPTH).into());
        }
        let eps = self.cache.get(self.ex, self.m, ty)?;
        let mut slots: Vec<Vec<XdtoData>> = vec![Vec::new(); eps.len()];
        for (k, v) in pairs {
            let name = String::from_utf16_lossy(k);
            let Some(i) = eps.iter().position(|p| p.json_name() == name) else {
                return Err(args_error(msg::XDTO_UNKNOWN_PROPERTY, vec![name]).into());
            };
            self.ctx.push((name, eps[i].prop.ty.clone()));
            let declared = eps[i].prop.ty.clone();
            if eps[i].is_list() {
                let JVal::Array(items) = v else {
                    return Err(plain_error(msg::XDTOJSON_INVALID_TYPE).into());
                };
                for it in items {
                    let d = self.item(&declared, it)?;
                    slots[i].push(d);
                }
            } else {
                let d = self.item(&declared, v)?;
                slots[i].push(d);
            }
            self.ctx.pop();
        }
        for (ep, s) in eps.iter().zip(&slots) {
            single_value(self.ex, ep, s.len())?;
        }
        let counts: Vec<(String, usize)> = eps
            .iter()
            .zip(&slots)
            .map(|(p, s)| (p.prop.name.clone(), s.len()))
            .collect();
        let count_of = |n: &str| counts.iter().find(|(c, _)| c == n).map_or(0, |(_, k)| *k);
        if let Err(b) = check_structure(
            self.ex,
            ty,
            &eps,
            object_type(self.m, ty).is_none_or(|o| o.ordered),
            &count_of,
        ) {
            return Err(b.1.into());
        }
        self.guard.leave();
        let props = eps
            .iter()
            .zip(slots)
            .filter(|(_, s)| !s.is_empty())
            .map(|(p, s)| (p.prop.name.clone(), s))
            .collect();
        Ok(XdtoData::Object {
            ty: ty.clone(),
            props,
        })
    }
}
