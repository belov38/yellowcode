use super::{msg, types};
use crate::bind::{arg_bool, arg_str, arg_value, is_given, lib_error, msg as bmsg, type_mismatch};
use crate::collections::{
    ctor_not_implemented, method_name, not_implemented, property_name, property_ru,
};
use crate::enums::{as_enum_value, enum_value};
use crate::fs::{fs, fs_error};
use crate::globals::data::encoding_from_value;
use bsl_lib::codec::{self, Encoding};
use bsl_lib::json::{Escape, LineBreak, WriteOptions, Writer};
use bsl_prim::Str;
use bsl_vm::{
    Arg, CallResult, Execution, HostError, HostObject, MemberId, Registry, TypeId, TypeIdx,
    TypeImpl, Value,
};
use std::any::Any;
use std::cell::{Cell, RefCell, RefMut};
use std::rc::Rc;
use std::sync::Arc;

const MAX_UNITS: u64 = 1 << 30;

pub(crate) struct WriterSettings {
    o: WriteOptions,
}

impl WriterSettings {
    fn of(v: &Value) -> Option<&WriterSettings> {
        v.as_object()?.as_any().downcast_ref::<WriterSettings>()
    }

    fn new_value(o: WriteOptions) -> Value {
        Value::Object(Rc::new(WriterSettings { o }))
    }
}

fn default_options() -> WriteOptions {
    WriteOptions {
        escape_line_separators: true,
        ..WriteOptions::default()
    }
}

fn line_break_name(l: LineBreak) -> &'static str {
    match l {
        LineBreak::Auto => "Auto",
        LineBreak::Windows => "Windows",
        LineBreak::Unix => "Unix",
        LineBreak::None => "None",
    }
}

fn escape_name(e: Escape) -> &'static str {
    match e {
        Escape::None => "None",
        Escape::NonAscii => "NotASCIISymbols",
        Escape::NonBmp => "SymbolsNotInBMP",
    }
}

fn enum_name(
    ex: &mut Execution,
    a: Option<&Arg>,
    t: TypeIdx,
    param: usize,
) -> CallResult<Option<&'static str>> {
    let v = arg_value(ex, a)?;
    if matches!(v, Value::Undefined) {
        return Ok(None);
    }
    match as_enum_value(&v) {
        Some(e) if e.type_idx() == t => Ok(Some(e.name_en())),
        _ => Err(type_mismatch(param).into()),
    }
}

fn bool_arg(ex: &mut Execution, a: Option<&Arg>, default: bool) -> CallResult<bool> {
    if matches!(arg_value(ex, a)?, Value::Undefined) {
        Ok(default)
    } else {
        arg_bool(ex, a)
    }
}

fn settings_ctor(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    if args.len() > 9 {
        return Err(ctor_not_implemented(types().settings).into());
    }
    let mut o = default_options();
    match enum_name(ex, args.first(), types().line_break, 1)? {
        Some("Unix") => o.line_break = LineBreak::Unix,
        Some("Windows") => o.line_break = LineBreak::Windows,
        Some("None") => o.line_break = LineBreak::None,
        _ => {}
    }
    if is_given(args.get(1)) && !matches!(arg_value(ex, args.get(1))?, Value::Undefined) {
        o.indent = arg_str(ex, args.get(1))?.units().to_vec();
    }
    o.double_quotes = bool_arg(ex, args.get(2), true)?;
    match enum_name(ex, args.get(3), types().escape, 4)? {
        Some("NotASCIISymbols") => o.escape = Escape::NonAscii,
        Some("SymbolsNotInBMP") => o.escape = Escape::NonBmp,
        _ => {}
    }
    o.escape_angle = bool_arg(ex, args.get(4), false)?;
    o.escape_line_separators = bool_arg(ex, args.get(5), true)?;
    o.escape_ampersand = bool_arg(ex, args.get(6), false)?;
    o.escape_single_quotes = bool_arg(ex, args.get(7), false)?;
    o.escape_slash = bool_arg(ex, args.get(8), false)?;
    Ok(WriterSettings::new_value(o))
}

fn read_only(t: TypeIdx, m: MemberId) -> bsl_vm::CallError {
    match property_name(t, m) {
        Some(_) => HostError::key(bmsg::PROP_NOT_WRITABLE)
            .with_arg(property_ru(t, m))
            .raw()
            .into(),
        None => not_implemented(t, m).into(),
    }
}

impl HostObject for WriterSettings {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(types().settings)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let t = types().settings;
        let o = &self.o;
        let name = property_name(t, m).ok_or_else(|| not_implemented(t, m))?;
        match name {
            "UseDoubleQuotes" => Some(Value::Bool(o.double_quotes)),
            "NewLines" => enum_value(types().line_break, line_break_name(o.line_break)),
            "PaddingSymbols" => Some(Value::String(Str::from_units(&o.indent))),
            "EscapeCharacters" => enum_value(types().escape, escape_name(o.escape)),
            "EscapeAmpersand" => Some(Value::Bool(o.escape_ampersand)),
            "EscapeSingleQuotes" => Some(Value::Bool(o.escape_single_quotes)),
            "EscapeLineTerminators" => Some(Value::Bool(o.escape_line_separators)),
            "EscapeSlash" => Some(Value::Bool(o.escape_slash)),
            "EscapeAngleBrackets" => Some(Value::Bool(o.escape_angle)),
            _ => None,
        }
        .ok_or_else(|| not_implemented(t, m).into())
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        Err(read_only(types().settings, m))
    }

    fn call(&self, _ex: &mut Execution, m: MemberId, _args: &mut [Arg]) -> CallResult<Value> {
        Err(not_implemented(types().settings, m).into())
    }
}

pub(crate) struct JsonWriter {
    w: RefCell<Option<Writer>>,
    settings: RefCell<Option<Value>>,
    validate: Cell<bool>,
    depth: Cell<u64>,
    indent: Cell<u64>,
    pretty: Cell<bool>,
    file: RefCell<Option<FileTarget>>,
}

struct FileTarget {
    path: String,
    enc: Encoding,
    bom: bool,
}

fn no_target() -> HostError {
    HostError::key(msg::NO_TARGET)
}

pub(crate) fn json_writer<'a>(_ex: &mut Execution, v: &'a Value) -> CallResult<RefMut<'a, Writer>> {
    let Some(w) = v
        .as_object()
        .and_then(|o| o.as_any().downcast_ref::<JsonWriter>())
    else {
        return Err(type_mismatch(1).into());
    };
    writer_of(w)
}

pub(crate) fn json_account(ex: &mut Execution, v: &Value, add: u64, depth: u64) -> CallResult<()> {
    let Some(w) = v
        .as_object()
        .and_then(|o| o.as_any().downcast_ref::<JsonWriter>())
    else {
        return Err(type_mismatch(1).into());
    };
    w.grow_at(ex, add, w.depth.get().saturating_add(depth))
}

fn writer_of(w: &JsonWriter) -> CallResult<RefMut<'_, Writer>> {
    let g = w.w.try_borrow_mut().map_err(|_| no_target())?;
    RefMut::filter_map(g, Option::as_mut).map_err(|_| no_target().into())
}

impl JsonWriter {
    fn set_target(&self, ex: &mut Execution, a: Option<&Arg>) -> CallResult<()> {
        let sv = Self::settings_value(ex, a)?;
        *self.file.borrow_mut() = None;
        self.apply_settings(sv)
    }

    fn open_file(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<()> {
        let path = arg_str(ex, args.first())?.to_string_lossy();
        let ev = arg_value(ex, args.get(1))?;
        let (enc, default_bom) = encoding_from_value(ex, &ev, 2)?;
        let bom = if matches!(arg_value(ex, args.get(2))?, Value::Undefined) {
            default_bom
        } else {
            arg_bool(ex, args.get(2))?
        };
        let sv = Self::settings_value(ex, args.get(3))?;
        let f = fs(ex, &path)?;
        f.get().write(&path, &[], false).map_err(fs_error)?;
        self.apply_settings(sv)?;
        *self.file.borrow_mut() = Some(FileTarget { path, enc, bom });
        Ok(())
    }

    fn settings_value(ex: &mut Execution, a: Option<&Arg>) -> CallResult<Value> {
        let v = arg_value(ex, a)?;
        match &v {
            Value::Undefined => Ok(WriterSettings::new_value(default_options())),
            v if WriterSettings::of(v).is_some() => Ok(v.clone()),
            _ => Err(type_mismatch(1).into()),
        }
    }

    fn apply_settings(&self, sv: Value) -> CallResult<()> {
        let o = WriterSettings::of(&sv)
            .map(|s| s.o.clone())
            .unwrap_or_default();
        self.indent.set(o.indent.len() as u64);
        self.pretty.set(o.line_break != LineBreak::None);
        self.depth.set(0);
        *self.w.try_borrow_mut().map_err(|_| no_target())? = Some(Writer::new(o));
        *self.settings.borrow_mut() = Some(sv);
        Ok(())
    }

    fn grow(&self, ex: &mut Execution, add: u64) -> CallResult<()> {
        self.grow_at(ex, add, self.depth.get())
    }

    fn grow_at(&self, ex: &mut Execution, add: u64, depth: u64) -> CallResult<()> {
        let extra = if self.pretty.get() {
            depth.saturating_mul(self.indent.get()).saturating_add(4)
        } else {
            0
        };
        let total = self.real_len().saturating_add(add).saturating_add(extra);
        if total > MAX_UNITS {
            return Err(ex.out_of_memory().into());
        }
        ex.charge(add.saturating_add(extra) / 64)?;
        ex.reserve(total.saturating_mul(2))?;
        Ok(())
    }

    fn real_len(&self) -> u64 {
        self.w
            .try_borrow()
            .ok()
            .and_then(|g| g.as_ref().map(|w| w.len() as u64))
            .unwrap_or(0)
    }

    fn write_value(&self, ex: &mut Execution, v: &Value) -> CallResult<()> {
        match v {
            Value::String(s) => {
                self.grow(ex, (s.len() as u64).saturating_mul(6).saturating_add(2))?;
                writer_of(self)?.string(s.units()).map_err(lib_error)?;
            }
            Value::Number(n) => {
                self.grow(ex, 64)?;
                writer_of(self)?.number(n).map_err(lib_error)?;
            }
            Value::Bool(b) => {
                self.grow(ex, 5)?;
                writer_of(self)?.bool(*b).map_err(lib_error)?;
            }
            Value::Undefined => {
                self.grow(ex, 4)?;
                writer_of(self)?.null().map_err(lib_error)?;
            }
            _ => return Err(HostError::key(msg::UNSUPPORTED).into()),
        }
        Ok(())
    }
}

impl HostObject for JsonWriter {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(types().writer)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let t = types().writer;
        match property_name(t, m) {
            Some("ValidateStructure") => Ok(Value::Bool(self.validate.get())),
            Some("Settings") => self
                .settings
                .borrow()
                .clone()
                .ok_or_else(|| no_target().into()),
            _ => Err(not_implemented(t, m).into()),
        }
    }

    fn set(&self, ex: &mut Execution, m: MemberId, v: Value) -> CallResult<()> {
        let t = types().writer;
        match property_name(t, m) {
            Some("ValidateStructure") => {
                self.validate.set(bsl_vm::ops::to_bool(ex, &v)?);
                Ok(())
            }
            _ => Err(read_only(t, m)),
        }
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        let t = types().writer;
        let Some(name) = method_name(t, m) else {
            return Err(not_implemented(t, m).into());
        };
        match name {
            "SetString" => {
                self.set_target(ex, args.first())?;
            }
            "WriteStartObject" | "WriteStartArray" => {
                self.grow(ex, 1)?;
                let mut w = writer_of(self)?;
                if name == "WriteStartObject" {
                    w.start_object()
                } else {
                    w.start_array()
                }
                .map_err(lib_error)?;
                self.depth.set(self.depth.get().saturating_add(1));
            }
            "WriteEndObject" | "WriteEndArray" => {
                self.grow(ex, 1)?;
                let mut w = writer_of(self)?;
                if name == "WriteEndObject" {
                    w.end_object()
                } else {
                    w.end_array()
                }
                .map_err(lib_error)?;
                self.depth.set(self.depth.get().saturating_sub(1));
            }
            "WritePropertyName" => {
                let s = arg_str(ex, args.first())?;
                self.grow(ex, (s.len() as u64).saturating_mul(6).saturating_add(4))?;
                writer_of(self)?
                    .property_name(s.units())
                    .map_err(lib_error)?;
            }
            "WriteValue" => {
                let v = arg_value(ex, args.first())?;
                self.write_value(ex, &v)?;
            }
            "WriteRaw" => {
                let s = arg_str(ex, args.first())?;
                self.grow(ex, s.len() as u64)?;
                writer_of(self)?.raw(s.units()).map_err(lib_error)?;
            }
            "OpenFile" => {
                self.open_file(ex, args)?;
            }
            "Close" => {
                let w = self.w.try_borrow_mut().map_err(|_| no_target())?.take();
                let target = self.file.borrow_mut().take();
                return match (w, target) {
                    (Some(w), Some(t)) => {
                        let text = w.finish().map_err(lib_error)?;
                        let n = (text.len() as u64).saturating_mul(4);
                        ex.charge(n / 64)?;
                        ex.reserve(n)?;
                        let bytes = codec::string_to_bytes(&text, t.enc, t.bom);
                        let f = fs(ex, &t.path)?;
                        f.get().write(&t.path, &bytes, false).map_err(fs_error)?;
                        Ok(Value::Undefined)
                    }
                    (Some(w), None) => {
                        Ok(Value::String(Str::from_vec(w.finish().map_err(lib_error)?)))
                    }
                    _ => Ok(Value::Undefined),
                };
            }
            _ => return Err(not_implemented(t, m).into()),
        }
        Ok(Value::Undefined)
    }
}

fn writer_ctor(_ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    if !args.is_empty() {
        return Err(ctor_not_implemented(types().writer).into());
    }
    Ok(Value::Object(Rc::new(JsonWriter {
        w: RefCell::new(None),
        settings: RefCell::new(None),
        validate: Cell::new(true),
        depth: Cell::new(0),
        indent: Cell::new(0),
        pretty: Cell::new(true),
        file: RefCell::new(None),
    })))
}

pub(super) fn register(reg: &mut Registry) {
    reg.type_impl(
        types().writer,
        TypeImpl {
            ctor: Some(Arc::new(writer_ctor)),
        },
    );
    reg.type_impl(
        types().settings,
        TypeImpl {
            ctor: Some(Arc::new(settings_ctor)),
        },
    );
}
