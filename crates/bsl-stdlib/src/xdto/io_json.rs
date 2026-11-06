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
    serializer_value_lists: bool,
    live_bytes: u64,
}

pub(crate) fn write_json(
    ex: &mut Execution,
    m: &Model,
    wv: &Value,
    d: &XdtoData,
    f: XdtoWriteFlags,
) -> CallResult<()> {
    write_json_impl(ex, m, wv, d, f, false, 0)
}

pub(crate) fn write_serializer_json(
    ex: &mut Execution,
    m: &Model,
    wv: &Value,
    d: &XdtoData,
    platform: bool,
    live_bytes: u64,
    f: XdtoWriteFlags,
) -> CallResult<()> {
    write_json_impl(ex, m, wv, d, f, platform, live_bytes)
}

fn write_json_impl(
    ex: &mut Execution,
    m: &Model,
    wv: &Value,
    d: &XdtoData,
    f: XdtoWriteFlags,
    serializer_value_lists: bool,
    live_bytes: u64,
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
        serializer_value_lists,
        live_bytes,
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
        crate::json::json_account_with_live(self.ex, self.wv, add, self.depth, self.live_bytes)
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
                if self.live_bytes != 0 {
                    let scratch = match value {
                        Value::Number(n) => {
                            n.size_hint_limbs().saturating_mul(18).saturating_add(64)
                        }
                        _ => (lexical.len() as u64).saturating_mul(2).saturating_add(64),
                    };
                    self.account(scratch)?;
                }
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
                if object_type(self.m, ty).is_some_and(|o| o.sequenced) {
                    if !(self.serializer_value_lists
                        && ty.ns == NS_CORE
                        && ty.name == "ValueListType")
                    {
                        return Err(unsupported("Последовательность").into());
                    }
                    crate::serial::valuelist_json::validate(self.ex, self.m, d)?;
                }
                let eps = self.cache.get(self.ex, self.m, ty)?;
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

fn tree(ex: &mut Execution, rv: &Value, guarded: bool) -> CallResult<(JVal, u64)> {
    enum Frame {
        Arr(Vec<JVal>),
        Obj(Vec<(Vec<u16>, JVal)>, Option<Vec<u16>>),
    }
    let input_bytes = if guarded {
        crate::json::json_reader(ex, rv)?
            .retained_bytes()
            .ok_or_else(|| ex.out_of_memory())?
    } else {
        0
    };
    if guarded {
        let (input, work, scratch, clones) = {
            let s = crate::json::json_reader(ex, rv)?;
            let input = s.retained_bytes().ok_or_else(|| ex.out_of_memory())?;
            let work = s
                .reader
                .remaining_work_bound()
                .ok_or_else(|| ex.out_of_memory())?;
            let scratch = s
                .reader
                .next_scratch_bound()
                .ok_or_else(|| ex.out_of_memory())?;
            let clones = s
                .current_clone_bound()
                .ok_or_else(|| ex.out_of_memory())?
                .checked_mul(2)
                .ok_or_else(|| ex.out_of_memory())?;
            (input, work, scratch, clones)
        };
        ex.charge(work)?;
        ex.reserve(
            input
                .checked_add(scratch)
                .and_then(|n| n.checked_add(clones))
                .ok_or_else(|| ex.out_of_memory())?,
        )?;
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
                ex.reserve(bytes.saturating_mul(2).saturating_add(input_bytes))?;
                if guarded {
                    stack.try_reserve(1).map_err(|_| ex.out_of_memory())?;
                }
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
                bytes = bytes
                    .saturating_add((if guarded { k.capacity() } else { k.len() }) as u64 * 2 + 48);
                ex.reserve(bytes.saturating_mul(2).saturating_add(input_bytes))?;
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
                    bytes = bytes
                        .saturating_add((if guarded { s.capacity() } else { s.len() }) as u64 * 2);
                }
                if guarded && let Token::Number(n) = &scalar {
                    bytes =
                        bytes.saturating_add((n.backing_capacity_limbs() as u64).saturating_mul(4));
                }
                ex.reserve(bytes.saturating_mul(2).saturating_add(input_bytes))?;
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
                Some(Frame::Arr(items)) => {
                    if guarded {
                        items.try_reserve(1).map_err(|_| ex.out_of_memory())?;
                    }
                    items.push(v)
                }
                Some(Frame::Obj(pairs, key)) => {
                    let k = key.take().ok_or_else(bad_state)?;
                    if guarded {
                        pairs.try_reserve(1).map_err(|_| ex.out_of_memory())?;
                    }
                    pairs.push((k, v));
                }
            }
        }
        if guarded {
            let (input, scratch) = {
                let s = crate::json::json_reader(ex, rv)?;
                (
                    s.retained_bytes().ok_or_else(|| ex.out_of_memory())?,
                    s.reader
                        .next_scratch_bound()
                        .ok_or_else(|| ex.out_of_memory())?,
                )
            };
            let live = bytes
                .checked_mul(2)
                .and_then(|n| n.checked_add(input))
                .and_then(|n| n.checked_add(scratch))
                .ok_or_else(|| ex.out_of_memory())?;
            ex.reserve(live)?;
        }
        let mut s = crate::json::json_reader(ex, rv)?;
        tok = s.reader.next().map_err(le)?.ok_or_else(bad_state)?;
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
    guarded: bool,
    cache_bound: u64,
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
    read_json_mode_impl(ex, m, rv, expect, mode, false).map(|(data, _)| data)
}

pub(crate) fn read_serializer_json(
    ex: &mut Execution,
    m: &Model,
    rv: &Value,
    expect: Option<&QName>,
) -> CallResult<(XdtoData, u64)> {
    read_json_mode_impl(ex, m, rv, expect, ReadMode::Values, true)
}

fn read_json_mode_impl(
    ex: &mut Execution,
    m: &Model,
    rv: &Value,
    expect: Option<&QName>,
    mode: ReadMode,
    guarded: bool,
) -> CallResult<(XdtoData, u64)> {
    let (t, retained_bytes) = tree(ex, rv, guarded)?;
    let input = if guarded {
        crate::json::json_reader(ex, rv)?
            .retained_bytes()
            .ok_or_else(|| ex.out_of_memory())?
    } else {
        0
    };
    let initial = if guarded {
        retained_bytes
            .checked_add(input)
            .ok_or_else(|| ex.out_of_memory())?
    } else {
        retained_bytes
    };
    let mut inp = In {
        ex,
        m,
        guard: DepthGuard::new(),
        cache: PropCache::default(),
        ctx: Vec::new(),
        mode,
        retained_bytes: initial,
        guarded,
        cache_bound: 0,
    };
    let declared = expect.cloned().unwrap_or_else(any_type);
    let result = match &t {
        JVal::Object(pairs) if member(pairs, "#value").is_some() => {
            let ty = match member(pairs, "#type") {
                Some(tt) => inp.type_of(tt)?,
                None => declared,
            };
            inp.context_slot()?;
            inp.ctx.push((String::new(), ty.clone()));
            let v = member(pairs, "#value").unwrap_or(&JVal::Null);
            inp.body(&ty, v, member(pairs, "#type").is_some())
        }
        _ if expect.is_some() => {
            inp.context_slot()?;
            inp.ctx.push((String::new(), declared.clone()));
            inp.body(&declared, &t, false)
        }
        JVal::Object(_) | JVal::Array(_) => {
            inp.context_slot()?;
            inp.ctx.push((String::new(), declared));
            Err(inp.untyped_error(&t)?)
        }
        _ => Err(plain_error(msg::XDTOJSON_INVALID_TYPE).into()),
    }?;
    let projection = inp
        .retained_bytes
        .saturating_sub(retained_bytes)
        .saturating_sub(input)
        .saturating_sub(inp.cache_bound);
    Ok((result, projection))
}

impl In<'_> {
    fn admit(&mut self, bytes: u64) -> CallResult<()> {
        if self.guarded {
            self.ex.charge(1 + bytes / 64)?;
            self.retained_bytes = self
                .retained_bytes
                .checked_add(bytes)
                .ok_or_else(|| self.ex.out_of_memory())?;
            self.ex.reserve(self.retained_bytes)?;
        }
        Ok(())
    }

    fn context_slot(&mut self) -> CallResult<()> {
        if self.guarded {
            self.admit(512)?;
            self.ctx
                .try_reserve(1)
                .map_err(|_| self.ex.out_of_memory())?;
        }
        Ok(())
    }

    fn admit_model_properties(&mut self, ty: &QName) -> CallResult<()> {
        if !self.guarded {
            return Ok(());
        }
        use std::mem::size_of;
        type Entry = (QName, std::sync::Arc<Vec<super::EProp>>);
        let capacity = self.cache.map.borrow().capacity();
        let future = capacity
            .checked_add(1)
            .and_then(|n| n.checked_mul(2))
            .ok_or_else(|| self.ex.out_of_memory())?;
        let old_table = bsl_vm::retained_hash_bytes::<Entry>(capacity)
            .ok_or_else(|| self.ex.out_of_memory())?;
        let new_table =
            bsl_vm::retained_hash_bytes::<Entry>(future).ok_or_else(|| self.ex.out_of_memory())?;
        let mut bytes = old_table
            .checked_add(new_table)
            .and_then(|n| {
                n.checked_add(
                    (size_of::<std::sync::Arc<Vec<super::EProp>>>()
                        + size_of::<Vec<super::EProp>>()
                        + 2 * size_of::<usize>()) as u64,
                )
            })
            .ok_or_else(|| self.ex.out_of_memory())?;
        let mut current = Some(ty);
        for _ in 0..super::MAX_BASES {
            let Some(q) = current else { break };
            let type_text =
                q.ns.len()
                    .checked_add(q.name.len())
                    .ok_or_else(|| self.ex.out_of_memory())?;
            self.ex
                .charge(1 + u64::try_from(type_text).map_err(|_| self.ex.out_of_memory())?)?;
            let Some(object) = object_type(self.m, q) else {
                break;
            };
            let base_text = match &object.base {
                Some(base) => base
                    .ns
                    .len()
                    .checked_add(base.name.len())
                    .ok_or_else(|| self.ex.out_of_memory())?,
                None => 0,
            };
            let names = type_text
                .checked_add(base_text)
                .ok_or_else(|| self.ex.out_of_memory())?;
            bytes = bytes
                .checked_add(u64::try_from(names).map_err(|_| self.ex.out_of_memory())?)
                .and_then(|n| {
                    n.checked_add(
                        (2 * size_of::<&bsl_lib::xdto::ObjectType>() + size_of::<QName>()) as u64,
                    )
                })
                .ok_or_else(|| self.ex.out_of_memory())?;
            self.ex.charge(
                u64::try_from(object.properties.len()).map_err(|_| self.ex.out_of_memory())?,
            )?;
            for p in &object.properties {
                let text = p
                    .name
                    .len()
                    .checked_add(p.ty.ns.len())
                    .and_then(|n| n.checked_add(p.ty.name.len()))
                    .and_then(|n| n.checked_add(object.name.ns.len()))
                    .and_then(|n| n.checked_add(p.local_name.as_ref().map_or(0, String::len)))
                    .and_then(|n| n.checked_add(p.default.as_ref().map_or(0, String::len)))
                    .and_then(|n| n.checked_add(p.fixed.as_ref().map_or(0, String::len)))
                    .ok_or_else(|| self.ex.out_of_memory())?;
                self.ex
                    .charge(1 + u64::try_from(text).map_err(|_| self.ex.out_of_memory())?)?;
                let entry = 3usize
                    .checked_mul(size_of::<super::EProp>())
                    .and_then(|n| n.checked_add(text.checked_mul(2)?))
                    .ok_or_else(|| self.ex.out_of_memory())?;
                bytes = bytes
                    .checked_add(u64::try_from(entry).map_err(|_| self.ex.out_of_memory())?)
                    .ok_or_else(|| self.ex.out_of_memory())?;
            }
            current = object.base.as_ref();
        }
        self.admit(bytes)?;
        self.cache_bound = self
            .cache_bound
            .checked_add(bytes)
            .ok_or_else(|| self.ex.out_of_memory())?;
        Ok(())
    }
    fn type_of(&mut self, v: &JVal) -> CallResult<QName> {
        let JVal::String(u) = v else {
            return Err(plain_error(msg::XDTOJSON_INVALID_TYPE).into());
        };
        self.admit((u.capacity() as u64).saturating_mul(16).saturating_add(512))?;
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

    fn untyped_error(&mut self, v: &JVal) -> CallResult<CallError> {
        let mut ctx = if self.guarded {
            self.ex.charge(self.ctx.len() as u64)?;
            let mut bytes = (self.ctx.len() as u64 + 1)
                .checked_mul((std::mem::size_of::<(String, QName)>() * 2) as u64)
                .ok_or_else(|| self.ex.out_of_memory())?;
            for (name, ty) in &self.ctx {
                let text = name
                    .len()
                    .checked_add(ty.ns.len())
                    .and_then(|n| n.checked_add(ty.name.len()))
                    .ok_or_else(|| self.ex.out_of_memory())?;
                bytes = bytes
                    .checked_add(u64::try_from(text).map_err(|_| self.ex.out_of_memory())?)
                    .ok_or_else(|| self.ex.out_of_memory())?;
            }
            self.admit(bytes)?;
            let mut copy = Vec::new();
            copy.try_reserve_exact(self.ctx.len().saturating_add(1))
                .map_err(|_| self.ex.out_of_memory())?;
            copy.extend(self.ctx.iter().cloned());
            copy
        } else {
            self.ctx.clone()
        };
        if self.guarded {
            let (key_units, first) = match v {
                JVal::Object(p) => p
                    .first()
                    .map_or((0usize, None), |(k, v)| (k.capacity(), Some(v))),
                JVal::Array(a) => (0, a.first()),
                _ => (0, None),
            };
            if let Some(JVal::Object(p)) = first {
                self.ex
                    .charge(u64::try_from(p.len()).map_err(|_| self.ex.out_of_memory())?)?;
            }
            let type_units = match first {
                Some(JVal::Object(p)) => p
                    .iter()
                    .find_map(|(k, v)| {
                        if k.iter().copied().eq("#type".encode_utf16())
                            && let JVal::String(s) = v
                        {
                            return Some(s.capacity());
                        }
                        None
                    })
                    .unwrap_or(0),
                _ => 0,
            };
            let units = key_units
                .checked_add(type_units)
                .ok_or_else(|| self.ex.out_of_memory())?;
            self.admit(
                u64::try_from(units)
                    .map_err(|_| self.ex.out_of_memory())?
                    .checked_mul(16)
                    .ok_or_else(|| self.ex.out_of_memory())?,
            )?;
        }
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
        Ok(match super::reader_context_chain(self.ex, ctx) {
            Some(e) => e.into(),
            None => plain_error(msg::XDTOJSON_INVALID_TYPE).into(),
        })
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
                _ => return Err(self.untyped_error(v)?),
            }
        } else if value_type(self.m, ty).is_some() {
            ty.clone()
        } else {
            return Err(args_error(msg::XDTO_TYPE_NOT_FOUND, vec![ty.clark()]).into());
        };
        if self.mode == ReadMode::Values {
            let source_bound = match v {
                JVal::String(units) => (if self.guarded {
                    units.capacity()
                } else {
                    units.len()
                } as u64)
                    .saturating_mul(3),
                JVal::Number(n) => n
                    .size_hint_limbs()
                    .saturating_mul(18)
                    .saturating_add(if self.guarded {
                        (n.backing_capacity_limbs() as u64).saturating_mul(4)
                    } else {
                        0
                    })
                    .saturating_add(32),
                JVal::Bool(_) => 5,
                _ => 0,
            };
            self.retained_bytes = if self.guarded {
                self.retained_bytes
                    .checked_add(
                        source_bound
                            .checked_mul(3)
                            .ok_or_else(|| self.ex.out_of_memory())?,
                    )
                    .and_then(|n| n.checked_add(std::mem::size_of::<XdtoData>() as u64))
                    .ok_or_else(|| self.ex.out_of_memory())?
            } else {
                self.retained_bytes
                    .saturating_add(source_bound.saturating_mul(3))
                    .saturating_add(std::mem::size_of::<XdtoData>() as u64)
            };
            self.ex.reserve(self.retained_bytes)?;
            self.ex.charge(1 + source_bound / 64)?;
        }
        let Some(text) = Self::scalar_text(v) else {
            return Err(plain_error(msg::XDTOJSON_INVALID_TYPE).into());
        };
        let type_description_type = self.guarded
            && target.ns == super::XS
            && target.name == "QName"
            && matches!(self.ctx.as_slice(), [.., (_, owner), (name, declared)]
                if owner.ns == NS_CORE && owner.name == "TypeDescription"
                    && name == "Type" && declared.ns == super::XS && declared.name == "QName");
        let valid_clark = type_description_type
            && text
                .strip_prefix('{')
                .and_then(|rest| rest.split_once('}'))
                .is_some_and(|(ns, local)| {
                    !ns.is_empty()
                        && !ns.contains(['{', '}'])
                        && !local.is_empty()
                        && !local.contains(['{', '}', ':'])
                        && bsl_lib::xdto::check_lexical(self.m, &target, local).is_ok()
                });
        let mut data = if valid_clark {
            XdtoData::Value {
                projection: None,
                ty: target,
                lexical: text.clone(),
                value: Value::from(text.as_str()),
            }
        } else {
            match value_from_text(self.ex, self.m, &target, &text, false) {
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
            }?
        };
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
        if self.guarded {
            let width = u64::try_from(items.len()).map_err(|_| self.ex.out_of_memory())?;
            self.ex.charge(width)?;
            self.admit(width.saturating_mul(512).saturating_add(1024))?;
        }
        self.admit_model_properties(ty)?;
        let eps = self.cache.get(self.ex, self.m, ty)?;
        let [only] = eps.as_slice() else {
            return Err(plain_error(msg::XDTOJSON_INVALID_TYPE).into());
        };
        if self.guarded {
            let text = ty
                .ns
                .len()
                .checked_add(ty.name.len())
                .and_then(|n| n.checked_add(only.prop.name.len()))
                .ok_or_else(|| self.ex.out_of_memory())?;
            self.admit(
                u64::try_from(text)
                    .map_err(|_| self.ex.out_of_memory())?
                    .checked_add(std::mem::size_of::<(String, Vec<XdtoData>)>() as u64)
                    .ok_or_else(|| self.ex.out_of_memory())?,
            )?;
        }
        if !only.is_list() {
            return Err(plain_error(msg::XDTOJSON_INVALID_TYPE).into());
        }
        if self.guard.enter().is_err() {
            return Err(HostError::key(crate::json::msg::MAX_DEPTH).into());
        }
        self.context_slot()?;
        self.ctx
            .push((only.json_name().to_string(), only.prop.ty.clone()));
        let mut out = if self.guarded {
            Vec::new()
        } else {
            Vec::with_capacity(items.len())
        };
        if self.guarded {
            out.try_reserve_exact(items.len())
                .map_err(|_| self.ex.out_of_memory())?;
        }
        for it in items {
            out.push(self.item(&only.prop.ty, it)?);
        }
        self.ctx.pop();
        self.guard.leave();
        let props = if out.is_empty() {
            Vec::new()
        } else if self.guarded {
            let mut props = Vec::new();
            props
                .try_reserve_exact(1)
                .map_err(|_| self.ex.out_of_memory())?;
            props.push((only.prop.name.clone(), out));
            props
        } else {
            vec![(only.prop.name.clone(), out)]
        };
        Ok(XdtoData::Object {
            ty: ty.clone(),
            props,
        })
    }

    fn object(&mut self, ty: &QName, pairs: &[(Vec<u16>, JVal)]) -> CallResult<XdtoData> {
        if self.guarded {
            let width = u64::try_from(pairs.len()).map_err(|_| self.ex.out_of_memory())?;
            self.ex.charge(width)?;
            self.admit(width.saturating_mul(1024).saturating_add(4096))?;
        }
        if self.guard.enter().is_err() {
            return Err(HostError::key(crate::json::msg::MAX_DEPTH).into());
        }
        self.admit_model_properties(ty)?;
        let eps = self.cache.get(self.ex, self.m, ty)?;
        if self.guarded {
            let mut output = 0u64;
            self.ex
                .charge(u64::try_from(eps.len()).map_err(|_| self.ex.out_of_memory())?)?;
            for ep in eps.iter() {
                let name =
                    u64::try_from(ep.prop.name.len()).map_err(|_| self.ex.out_of_memory())?;
                output = output
                    .checked_add(name.checked_mul(3).ok_or_else(|| self.ex.out_of_memory())?)
                    .and_then(|n| {
                        n.checked_add((3 * std::mem::size_of::<(String, Vec<XdtoData>)>()) as u64)
                    })
                    .ok_or_else(|| self.ex.out_of_memory())?;
            }
            self.admit(output)?;
        }
        let mut slots: Vec<Vec<XdtoData>> = if self.guarded {
            let mut slots = Vec::new();
            slots
                .try_reserve_exact(eps.len())
                .map_err(|_| self.ex.out_of_memory())?;
            slots.resize_with(eps.len(), Vec::new);
            slots
        } else {
            vec![Vec::new(); eps.len()]
        };
        for (k, v) in pairs {
            if self.guarded {
                let width = u64::try_from(eps.len()).map_err(|_| self.ex.out_of_memory())?;
                let key = u64::try_from(k.len()).map_err(|_| self.ex.out_of_memory())?;
                self.ex.charge(
                    width
                        .checked_mul(key.saturating_add(1))
                        .ok_or_else(|| self.ex.out_of_memory())?,
                )?;
            }
            let name = String::from_utf16_lossy(k);
            let Some(i) = eps.iter().position(|p| p.json_name() == name) else {
                return Err(args_error(msg::XDTO_UNKNOWN_PROPERTY, vec![name]).into());
            };
            self.context_slot()?;
            self.ctx.push((name, eps[i].prop.ty.clone()));
            let declared = eps[i].prop.ty.clone();
            if eps[i].is_list() {
                let JVal::Array(items) = v else {
                    return Err(plain_error(msg::XDTOJSON_INVALID_TYPE).into());
                };
                for it in items {
                    if self.guarded {
                        self.ex.charge(1)?;
                        self.admit(512)?;
                        slots[i]
                            .try_reserve(1)
                            .map_err(|_| self.ex.out_of_memory())?;
                    }
                    let d = self.item(&declared, it)?;
                    slots[i].push(d);
                }
            } else {
                if self.guarded {
                    slots[i]
                        .try_reserve(1)
                        .map_err(|_| self.ex.out_of_memory())?;
                }
                let d = self.item(&declared, v)?;
                slots[i].push(d);
            }
            self.ctx.pop();
        }
        for (ep, s) in eps.iter().zip(&slots) {
            single_value(self.ex, ep, s.len())?;
        }
        if self.guarded {
            let width = u64::try_from(eps.len()).map_err(|_| self.ex.out_of_memory())?;
            self.ex.charge(width)?;
            let name_bytes = eps
                .iter()
                .try_fold(0u64, |sum, ep| {
                    sum.checked_add(u64::try_from(ep.prop.name.len()).ok()?)
                })
                .ok_or_else(|| self.ex.out_of_memory())?;
            self.ex.charge(
                width
                    .checked_mul(name_bytes.saturating_add(width))
                    .ok_or_else(|| self.ex.out_of_memory())?,
            )?;
        }
        let counts: Vec<(String, usize)> = if self.guarded {
            let mut counts = Vec::new();
            counts
                .try_reserve_exact(eps.len())
                .map_err(|_| self.ex.out_of_memory())?;
            for (p, s) in eps.iter().zip(&slots) {
                counts.push((p.prop.name.clone(), s.len()));
            }
            counts
        } else {
            eps.iter()
                .zip(&slots)
                .map(|(p, s)| (p.prop.name.clone(), s.len()))
                .collect()
        };
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
        let props = if self.guarded {
            let mut props = Vec::new();
            props
                .try_reserve_exact(eps.len())
                .map_err(|_| self.ex.out_of_memory())?;
            for (p, s) in eps.iter().zip(slots) {
                if !s.is_empty() {
                    props.push((p.prop.name.clone(), s));
                }
            }
            props
        } else {
            eps.iter()
                .zip(slots)
                .filter(|(_, s)| !s.is_empty())
                .map(|(p, s)| (p.prop.name.clone(), s))
                .collect()
        };
        Ok(XdtoData::Object {
            ty: ty.clone(),
            props,
        })
    }
}
