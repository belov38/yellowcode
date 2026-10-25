use super::{msg, types};
use crate::bind::{
    arg_bool, arg_str, arg_value, global, is_given, key_error, lib_error, type_mismatch,
};

fn le(e: bsl_lib::LibError) -> bsl_vm::CallError {
    lib_error(e).into()
}

use crate::collections::types as coll_types;
use crate::collections::{
    Snapshot, ctor_not_implemented, is_name, map_put, method_name, new_array, new_map,
    new_structure, not_implemented, property_name, reserve_values, snapshot,
};
use crate::enums::{as_enum_value, enum_value};
use crate::globals::tz::{session_offset_of_local, utc_to_session};
use crate::globals::{enum_arg, reserve_units};
use crate::serial::cycle_fatal;
use crate::serial::depth::DepthGuard;
use bsl_lib::json::{DateFormat, DateVariant, Token, read_date, write_date};
use bsl_prim::{Number, Str, fold_name};
use bsl_vm::{
    Arg, CallResult, Execution, HostError, HostObject, MemberId, Place, Registry, TypeId, TypeImpl,
    Value,
};
use std::any::Any;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

pub(crate) struct SerializerSettings {
    format: Cell<DateFormat>,
    variant: Cell<DateVariant>,
    arrays_as_objects: Cell<bool>,
}

impl SerializerSettings {
    fn of(v: &Value) -> Option<&SerializerSettings> {
        v.as_object()?.as_any().downcast_ref::<SerializerSettings>()
    }
}

fn format_name(f: DateFormat) -> &'static str {
    match f {
        DateFormat::Iso => "ISO",
        DateFormat::Microsoft => "Microsoft",
        DateFormat::JavaScript => "JavaScript",
    }
}

fn variant_name(v: DateVariant) -> &'static str {
    match v {
        DateVariant::Local => "LocalDate",
        DateVariant::LocalWithOffset => "LocalDateWithOffset",
        DateVariant::Universal => "UniversalDate",
    }
}

const FORMAT_ALIAS: &str = "ФорматСериализацииДаты";
const FORMAT_ALIAS_ID: MemberId = MemberId(0x4000_0000);

fn settings_ctor(_ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    if !args.is_empty() {
        return Err(ctor_not_implemented(types().serializer_settings).into());
    }
    Ok(Value::Object(Rc::new(SerializerSettings {
        format: Cell::new(DateFormat::Iso),
        variant: Cell::new(DateVariant::Local),
        arrays_as_objects: Cell::new(false),
    })))
}

impl HostObject for SerializerSettings {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(types().serializer_settings)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn find_member(&self, name: &str) -> Option<MemberId> {
        (fold_name(name) == fold_name(FORMAT_ALIAS)).then_some(FORMAT_ALIAS_ID)
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let t = types().serializer_settings;
        match property_name(t, m).or((m == FORMAT_ALIAS_ID).then_some("DateSerializationFormat")) {
            Some("DateSerializationFormat") => {
                enum_value(types().date_format, format_name(self.format.get()))
            }
            Some("DateWritingVariant") => {
                enum_value(types().date_variant, variant_name(self.variant.get()))
            }
            Some("SerializeArraysAsObjects") => Some(Value::Bool(self.arrays_as_objects.get())),
            _ => None,
        }
        .ok_or_else(|| not_implemented(t, m).into())
    }

    fn set(&self, ex: &mut Execution, m: MemberId, v: Value) -> CallResult<()> {
        let t = types().serializer_settings;
        match property_name(t, m).or((m == FORMAT_ALIAS_ID).then_some("DateSerializationFormat")) {
            Some("DateSerializationFormat") => {
                let name = enum_name(&v, types().date_format)?;
                self.format.set(match name {
                    "Microsoft" => DateFormat::Microsoft,
                    "JavaScript" => DateFormat::JavaScript,
                    _ => DateFormat::Iso,
                });
                Ok(())
            }
            Some("DateWritingVariant") => {
                let name = enum_name(&v, types().date_variant)?;
                self.variant.set(match name {
                    "LocalDateWithOffset" => DateVariant::LocalWithOffset,
                    "UniversalDate" => DateVariant::Universal,
                    _ => DateVariant::Local,
                });
                Ok(())
            }
            Some("SerializeArraysAsObjects") => {
                self.arrays_as_objects.set(bsl_vm::ops::to_bool(ex, &v)?);
                Ok(())
            }
            _ => Err(not_implemented(t, m).into()),
        }
    }

    fn call(&self, _ex: &mut Execution, m: MemberId, _args: &mut [Arg]) -> CallResult<Value> {
        let t = types().serializer_settings;
        let name = method_name(t, m).unwrap_or_default();
        Err(HostError::not_implemented("НастройкиСериализацииJSON", name).into())
    }
}

fn enum_name(v: &Value, t: platform_catalog::TypeIdx) -> CallResult<&'static str> {
    match as_enum_value(v) {
        Some(e) if e.type_idx() == t => Ok(e.name_en()),
        _ => Err(type_mismatch(1).into()),
    }
}

struct Callback {
    obj: bsl_vm::ObjRef,
    member: MemberId,
    extra: Value,
}

impl Callback {
    fn from_args(ex: &mut Execution, a: &[Arg], param: usize) -> CallResult<Option<Callback>> {
        let name = arg_str(ex, a.get(param - 1))?;
        if name.is_empty() {
            return Ok(None);
        }
        let module = arg_value(ex, a.get(param))?;
        let Some(obj) = module.as_object() else {
            return Err(type_mismatch(param + 1).into());
        };
        let name = name.to_string_lossy();
        let Some(member) = obj.find_member(&name) else {
            return Err(HostError::key(crate::bind::msg::METHOD_NOT_FOUND)
                .with_arg(name)
                .into());
        };
        let extra = arg_value(ex, a.get(param + 1))?;
        Ok(Some(Callback {
            obj: obj.clone(),
            member,
            extra,
        }))
    }

    fn call(&self, ex: &mut Execution, mut args: Vec<Arg>) -> CallResult<Value> {
        self.obj.call(ex, self.member, &mut args)
    }
}

enum Frame {
    Array {
        items: std::vec::IntoIter<Value>,
        convert: bool,
    },
    Object {
        pairs: std::vec::IntoIter<(Value, Value)>,
        convert: bool,
    },
}

struct Write<'a> {
    wv: &'a Value,
    format: DateFormat,
    variant: DateVariant,
    cb: Option<&'a Callback>,
    guard: DepthGuard,
    stack: Vec<Frame>,
    live: u64,
    sizes: Vec<u64>,
}

fn unsupported() -> HostError {
    HostError::key(msg::UNSUPPORTED)
}

impl Write<'_> {
    fn account(&self, ex: &mut Execution, add: u64) -> CallResult<()> {
        crate::json::json_account(ex, self.wv, add, self.stack.len() as u64)
    }

    fn convert(&self, ex: &mut Execution, prop: Str, v: Value) -> CallResult<Option<Value>> {
        let Some(cb) = self.cb else {
            return Err(unsupported().into());
        };
        let cancel = Place::temp(Value::Bool(false));
        let mut args = vec![Arg::Val(Value::String(prop)), Arg::Val(v)];
        args.push(Arg::Val(cb.extra.clone()));
        args.push(Arg::Place(cancel.clone()));
        let r = cb.call(ex, args)?;
        if matches!(cancel.temp_value(), Some(Value::Bool(true))) {
            return Ok(None);
        }
        Ok(Some(r))
    }

    fn item(
        &mut self,
        ex: &mut Execution,
        name: Option<&Str>,
        v: Value,
        convert: bool,
    ) -> CallResult<()> {
        ex.charge(1)?;
        let mut convert = convert;
        let mut v = v;
        let mut snap = None;
        if !is_scalar(&v) {
            snap = snapshot(&v);
            if snap.is_none() {
                if !convert {
                    return Err(unsupported().into());
                }
                let prop = name.cloned().unwrap_or_default();
                let Some(r) = self.convert(ex, prop, v)? else {
                    return Ok(());
                };
                convert = false;
                v = r;
                if !is_scalar(&v) {
                    snap = snapshot(&v);
                    if snap.is_none() {
                        return Err(unsupported().into());
                    }
                }
            }
        }
        if let Some(n) = name {
            self.account(ex, (n.len() as u64).saturating_mul(6).saturating_add(4))?;
            crate::json::json_writer(ex, self.wv)?
                .property_name(n.units())
                .map_err(le)?;
        }
        match snap {
            Some(s) => self.open(ex, s, convert),
            None => self.scalar(ex, &v),
        }
    }

    fn scalar(&self, ex: &mut Execution, v: &Value) -> CallResult<()> {
        match v {
            Value::String(s) => {
                self.account(ex, (s.len() as u64).saturating_mul(6).saturating_add(2))?;
                crate::json::json_writer(ex, self.wv)?
                    .string(s.units())
                    .map_err(le)
            }
            Value::Number(n) => {
                self.account(ex, 64)?;
                crate::json::json_writer(ex, self.wv)?.number(n).map_err(le)
            }
            Value::Bool(b) => {
                self.account(ex, 5)?;
                crate::json::json_writer(ex, self.wv)?.bool(*b).map_err(le)
            }
            Value::Date(d) => {
                self.account(ex, 64)?;
                let text = write_date(
                    *d,
                    session_offset_of_local(ex, *d),
                    self.format,
                    self.variant,
                )
                .map_err(le)?;
                let units: Vec<u16> = text.encode_utf16().collect();
                crate::json::json_writer(ex, self.wv)?
                    .string(&units)
                    .map_err(le)
            }
            _ => {
                self.account(ex, 4)?;
                crate::json::json_writer(ex, self.wv)?.null().map_err(le)
            }
        }
    }

    fn open(&mut self, ex: &mut Execution, s: Snapshot, convert: bool) -> CallResult<()> {
        if self.guard.enter().is_err() {
            return Err(cycle_fatal(ex).into());
        }
        let (n, frame, is_obj) = match s {
            Snapshot::Array(items) => (
                items.len(),
                Frame::Array {
                    items: items.into_iter(),
                    convert,
                },
                false,
            ),
            Snapshot::Structure(f) => {
                let pairs: Vec<(Value, Value)> =
                    f.into_iter().map(|(k, v)| (Value::String(k), v)).collect();
                (
                    pairs.len(),
                    Frame::Object {
                        pairs: pairs.into_iter(),
                        convert,
                    },
                    true,
                )
            }
            Snapshot::Map(pairs) => (
                pairs.len(),
                Frame::Object {
                    pairs: pairs.into_iter(),
                    convert,
                },
                true,
            ),
        };
        self.live = self.live.saturating_add(n as u64);
        self.sizes.push(n as u64);
        reserve_values(ex, self.live)?;
        self.account(ex, 1)?;
        {
            let mut w = crate::json::json_writer(ex, self.wv)?;
            if is_obj {
                w.start_object()
            } else {
                w.start_array()
            }
            .map_err(le)?;
        }
        self.stack.push(frame);
        Ok(())
    }

    fn close(&mut self, ex: &mut Execution, is_obj: bool) -> CallResult<()> {
        self.stack.pop();
        self.live = self.live.saturating_sub(self.sizes.pop().unwrap_or(0));
        self.guard.leave();
        self.account(ex, 1)?;
        let mut w = crate::json::json_writer(ex, self.wv)?;
        if is_obj {
            w.end_object()
        } else {
            w.end_array()
        }
        .map_err(le)
    }

    fn run(&mut self, ex: &mut Execution, root: Value) -> CallResult<()> {
        self.item(ex, None, root, true)?;
        loop {
            let step = match self.stack.last_mut() {
                None => return Ok(()),
                Some(Frame::Array { items, convert }) => match items.next() {
                    Some(v) => Step::Item(None, v, *convert),
                    None => Step::Close(false),
                },
                Some(Frame::Object { pairs, convert }) => match pairs.next() {
                    Some((Value::String(k), v)) => Step::Item(Some(k), v, *convert),
                    Some(_) => return Err(HostError::key(msg::BAD_KEY_TYPE).into()),
                    None => Step::Close(true),
                },
            };
            match step {
                Step::Item(name, v, convert) => self.item(ex, name.as_ref(), v, convert)?,
                Step::Close(is_obj) => self.close(ex, is_obj)?,
            }
        }
    }
}

enum Step {
    Item(Option<Str>, Value, bool),
    Close(bool),
}

fn is_scalar(v: &Value) -> bool {
    matches!(
        v,
        Value::Undefined | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Date(_)
    )
}

fn write_json(ex: &mut Execution, a: &mut [Arg]) -> CallResult<Value> {
    let wv = arg_value(ex, a.first())?;
    drop(crate::json::json_writer(ex, &wv)?);
    let value = arg_value(ex, a.get(1))?;
    let (format, variant) = match arg_value(ex, a.get(2))? {
        Value::Undefined => (DateFormat::Iso, DateVariant::Local),
        v => match SerializerSettings::of(&v) {
            Some(s) => (s.format.get(), s.variant.get()),
            None => return Err(type_mismatch(3).into()),
        },
    };
    let cb = Callback::from_args(ex, a, 4)?;
    let mut w = Write {
        wv: &wv,
        format,
        variant,
        cb: cb.as_ref(),
        guard: DepthGuard::new(),
        stack: Vec::new(),
        live: 0,
        sizes: Vec::new(),
    };
    w.run(ex, value)?;
    Ok(Value::Undefined)
}

enum Build {
    Array {
        items: Vec<Value>,
        prop: Vec<u16>,
    },
    Object {
        pairs: Vec<(Vec<u16>, Value)>,
        key: Option<Vec<u16>>,
        prop: Vec<u16>,
    },
}

impl Build {
    fn prop(&self) -> &[u16] {
        match self {
            Build::Array { prop, .. } | Build::Object { prop, .. } => prop,
        }
    }
}

struct Read<'a> {
    rv: &'a Value,
    to_map: bool,
    date_names: Vec<Vec<u16>>,
    date_format: DateFormat,
    restore: Option<&'a Callback>,
    restore_names: Option<Vec<Vec<u16>>>,
    count: u64,
}

fn bad_state() -> HostError {
    HostError::key(bsl_lib::msg::BAD_STATE)
}

fn names_arg(
    ex: &mut Execution,
    a: Option<&Arg>,
    param: usize,
) -> CallResult<Option<Vec<Vec<u16>>>> {
    match arg_value(ex, a)? {
        Value::Undefined => Ok(None),
        Value::String(s) => Ok(Some(vec![s.units().to_vec()])),
        v => match crate::collections::array_items(&v) {
            Some(items) => items
                .iter()
                .map(|i| match i {
                    Value::String(s) => Ok(s.units().to_vec()),
                    _ => Err(type_mismatch(param).into()),
                })
                .collect::<CallResult<Vec<_>>>()
                .map(Some),
            None => Err(type_mismatch(param).into()),
        },
    }
}

impl Read<'_> {
    fn first_token(&self, ex: &mut Execution) -> CallResult<Token> {
        let mut s = crate::json::json_reader(ex, self.rv)?;
        s.consumed = false;
        s.reader
            .next_value_token()
            .map_err(le)?
            .ok_or_else(|| bad_state().into())
    }

    fn next_token(&self, ex: &mut Execution) -> CallResult<Token> {
        let mut s = crate::json::json_reader(ex, self.rv)?;
        s.reader
            .next()
            .map_err(le)?
            .ok_or_else(|| bad_state().into())
    }

    fn finish(&self, ex: &mut Execution, last: Option<Token>) -> CallResult<()> {
        let mut s = crate::json::json_reader(ex, self.rv)?;
        s.current = last;
        s.consumed = true;
        Ok(())
    }

    fn key_name(&self, ex: &mut Execution, key: &[u16]) -> CallResult<()> {
        if self.to_map || is_name(key) {
            return Ok(());
        }
        let ty = ex
            .runtime()
            .types()
            .name(
                TypeId::of_catalog(coll_types().structure),
                ex.env().interface_language(),
            )
            .to_string();
        Err(key_error(
            msg::BAD_PROPERTY_NAME,
            vec![String::from_utf16_lossy(key), ty],
            String::new(),
        )
        .into())
    }

    fn complete(
        &self,
        ex: &mut Execution,
        prop: &[u16],
        in_object: bool,
        v: Value,
    ) -> CallResult<Value> {
        let mut v = v;
        if in_object && self.date_names.iter().any(|n| n == prop) {
            v = self.date_field(ex, v)?;
        }
        let Some(cb) = self.restore else {
            return Ok(v);
        };
        if let Some(names) = &self.restore_names
            && !names.iter().any(|n| n == prop)
        {
            return Ok(v);
        }
        let mut args = vec![Arg::Val(Value::String(Str::from_units(prop))), Arg::Val(v)];
        args.push(Arg::Val(cb.extra.clone()));
        cb.call(ex, args)
    }

    fn date_field(&self, ex: &mut Execution, v: Value) -> CallResult<Value> {
        let text: Vec<u16> = match &v {
            Value::String(s) => s.units().to_vec(),
            Value::Number(_) | Value::Bool(_) => Vec::new(),
            _ => return Ok(v),
        };
        let x = read_date(&text, self.date_format).map_err(le)?;
        Ok(Value::Date(match x.offset {
            None => x.date,
            Some(o) => utc_to_session(ex, x.date.add_seconds(&Number::from_i64(-i64::from(o)))),
        }))
    }

    fn container(&self, ex: &mut Execution, b: Build) -> CallResult<Value> {
        match b {
            Build::Array { items, .. } => Ok(new_array(items)),
            Build::Object { pairs, .. } if self.to_map => {
                let m = new_map();
                for (k, v) in pairs {
                    map_put(ex, &m, Value::String(Str::from_units(&k)), v)?;
                }
                Ok(m)
            }
            Build::Object { pairs, .. } => {
                let keys: Vec<String> = pairs
                    .iter()
                    .map(|(k, _)| String::from_utf16_lossy(k))
                    .collect();
                let fields = keys
                    .iter()
                    .map(String::as_str)
                    .zip(pairs.into_iter().map(|(_, v)| v))
                    .collect();
                Ok(new_structure(fields))
            }
        }
    }

    fn run(&self, ex: &mut Execution, mut guard: DepthGuard, limited: bool) -> CallResult<Value> {
        let depth_error = move || -> HostError {
            if limited {
                crate::bind::with_other(HostError::key(msg::MAX_DEPTH), false)
            } else {
                bad_state()
            }
        };
        let mut stack: Vec<Build> = Vec::new();
        let mut tok = self.first_token(ex)?;
        let mut last: Option<Token> = None;
        let mut count = self.count;
        loop {
            ex.charge(1)?;
            let value = match tok {
                Token::StartObject | Token::StartArray => {
                    if guard.enter().is_err() {
                        return Err(depth_error().into());
                    }
                    let prop = self.member_prop(&stack);
                    stack.push(if tok == Token::StartArray {
                        Build::Array {
                            items: Vec::new(),
                            prop,
                        }
                    } else {
                        Build::Object {
                            pairs: Vec::new(),
                            key: None,
                            prop,
                        }
                    });
                    None
                }
                Token::EndObject | Token::EndArray => {
                    let Some(b) = stack.pop() else {
                        return Err(bad_state().into());
                    };
                    guard.leave();
                    let prop = b.prop().to_vec();
                    let in_object = matches!(stack.last(), Some(Build::Object { .. }));
                    let v = self.container(ex, b)?;
                    if stack.is_empty() {
                        last = Some(if matches!(tok, Token::EndArray) {
                            Token::EndArray
                        } else {
                            Token::EndObject
                        });
                    }
                    Some(self.complete(ex, &prop, in_object, v)?)
                }
                Token::PropertyName(k) => {
                    self.key_name(ex, &k)?;
                    match stack.last_mut() {
                        Some(Build::Object { key, .. }) => *key = Some(k),
                        _ => return Err(bad_state().into()),
                    }
                    None
                }
                scalar => {
                    let v = match &scalar {
                        Token::String(u) => Value::String(Str::from_units(u)),
                        Token::Number(n) => Value::Number(n.clone()),
                        Token::Bool(b) => Value::Bool(*b),
                        _ => Value::Undefined,
                    };
                    reserve_units(ex, 0, 1)?;
                    let (prop, in_object) = match stack.last() {
                        Some(Build::Object { key: Some(k), .. }) => (k.clone(), true),
                        _ => (Vec::new(), false),
                    };
                    if stack.is_empty() {
                        last = Some(scalar);
                    }
                    Some(self.complete(ex, &prop, in_object, v)?)
                }
            };
            if let Some(v) = value {
                count += 1;
                reserve_values(ex, count)?;
                match stack.last_mut() {
                    None => {
                        self.finish(ex, last)?;
                        return Ok(v);
                    }
                    Some(Build::Array { items, .. }) => items.push(v),
                    Some(Build::Object { pairs, key, .. }) => match key.take() {
                        Some(k) => pairs.push((k, v)),
                        None => return Err(bad_state().into()),
                    },
                }
            }
            tok = self.next_token(ex)?;
        }
    }

    fn member_prop(&self, stack: &[Build]) -> Vec<u16> {
        match stack.last() {
            Some(Build::Object { key: Some(k), .. }) => k.clone(),
            _ => Vec::new(),
        }
    }
}

fn read_json(ex: &mut Execution, a: &mut [Arg]) -> CallResult<Value> {
    let rv = arg_value(ex, a.first())?;
    drop(crate::json::json_reader(ex, &rv)?);
    let to_map = if is_given(a.get(1)) {
        arg_bool(ex, a.get(1))?
    } else {
        false
    };
    let date_names = names_arg(ex, a.get(2), 3)?.unwrap_or_default();
    let date_format = match enum_arg(ex, a.get(3), "ФорматДатыJSON", 4)? {
        Some("Microsoft") => DateFormat::Microsoft,
        Some("JavaScript") => DateFormat::JavaScript,
        _ => DateFormat::Iso,
    };
    let restore = Callback::from_args(ex, a, 5)?;
    let restore_names = names_arg(ex, a.get(7), 8)?;
    let (guard, limited) = match arg_value(ex, a.get(8))? {
        Value::Undefined => (DepthGuard::with_max(499), true),
        Value::Number(n) => match n.to_i64() {
            Some(n) if n > 0 => {
                let max = usize::try_from(n).unwrap_or(usize::MAX);
                (DepthGuard::with_max(max.saturating_sub(1)), true)
            }
            Some(_) => (DepthGuard::new(), false),
            None => return Err(crate::bind::value_mismatch(9).into()),
        },
        _ => return Err(type_mismatch(9).into()),
    };
    let r = Read {
        rv: &rv,
        to_map,
        date_names,
        date_format,
        restore: restore.as_ref(),
        restore_names,
        count: 0,
    };
    r.run(ex, guard, limited)
}

pub(super) fn register(reg: &mut Registry) {
    reg.type_impl(
        types().serializer_settings,
        TypeImpl {
            ctor: Some(Arc::new(settings_ctor)),
        },
    );
    global(reg, "ЗаписатьJSON", "WriteJSON", write_json);
    global(reg, "ПрочитатьJSON", "ReadJSON", read_json);
}
