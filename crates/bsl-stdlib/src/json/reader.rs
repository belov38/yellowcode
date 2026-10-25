use super::{msg, types};
use crate::bind::{arg_str, arg_value, lib_error};
use crate::collections::{ctor_not_implemented, method_name, not_implemented, property_name};
use crate::enums::enum_value;
use crate::fs::{fs, fs_error};
use crate::globals::data::encoding_from_value;
use crate::globals::reserve_units;
use bsl_lib::codec;
use bsl_lib::json::{OwnedReader, Token};
use bsl_prim::Str;
use bsl_vm::{
    Arg, CallResult, Execution, HostError, HostObject, MemberId, Registry, TypeId, TypeImpl, Value,
};
use platform_catalog::messages::text_key;
use std::any::Any;
use std::cell::{RefCell, RefMut};
use std::rc::Rc;
use std::sync::Arc;

pub(crate) struct JsonReaderState {
    pub reader: OwnedReader,
    pub current: Option<Token>,
    pub consumed: bool,
}

pub(crate) struct JsonReader {
    st: RefCell<Option<JsonReaderState>>,
}

fn no_source() -> HostError {
    HostError::key(msg::NO_SOURCE)
}

pub(crate) fn json_reader<'a>(
    _ex: &mut Execution,
    v: &'a Value,
) -> CallResult<RefMut<'a, JsonReaderState>> {
    let Some(r) = v
        .as_object()
        .and_then(|o| o.as_any().downcast_ref::<JsonReader>())
    else {
        return Err(crate::bind::type_mismatch(1).into());
    };
    state(r)
}

fn state(r: &JsonReader) -> CallResult<RefMut<'_, JsonReaderState>> {
    let g = r.st.try_borrow_mut().map_err(|_| no_source())?;
    RefMut::filter_map(g, Option::as_mut).map_err(|_| no_source().into())
}

pub(crate) fn token_type(t: Option<&Token>) -> &'static str {
    match t {
        None => "None",
        Some(Token::StartObject) => "ObjectStart",
        Some(Token::EndObject) => "ObjectEnd",
        Some(Token::StartArray) => "ArrayStart",
        Some(Token::EndArray) => "ArrayEnd",
        Some(Token::PropertyName(_)) => "PropertyName",
        Some(Token::Null) => "Null",
        Some(Token::Bool(_)) => "Boolean",
        Some(Token::Number(_)) => "Number",
        Some(Token::String(_)) => "String",
    }
}

pub(crate) fn value_type_text(
    name_en: &str,
    lang: platform_catalog::messages::Lang,
) -> Option<Str> {
    let key = match name_en {
        "None" => "json:IDS_JSON_VALUE_TYPE_NONE",
        "ObjectStart" => "json:IDS_JSON_VALUE_TYPE_OBJECT_START",
        "ObjectEnd" => "json:IDS_JSON_VALUE_TYPE_OBJECT_END",
        "ArrayStart" => "json:IDS_JSON_VALUE_TYPE_ARRAY_START",
        "ArrayEnd" => "json:IDS_JSON_VALUE_TYPE_ARRAY_END",
        "PropertyName" => "json:IDS_JSON_VALUE_TYPE_PROPERTY_NAME",
        "Null" => "json:IDS_JSON_VALUE_TYPE_NULL",
        "Boolean" => "json:IDS_JSON_VALUE_TYPE_BOOLEAN",
        "Number" => "json:IDS_JSON_VALUE_TYPE_NUMBER",
        "String" => "json:IDS_JSON_VALUE_TYPE_STRING",
        "Comment" => "json:IDS_JSON_VALUE_TYPE_COMMENT",
        _ => return None,
    };
    text_key(key, lang).map(Str::from)
}

impl JsonReader {
    fn open(&self, ex: &mut Execution, text: &Str) -> CallResult<()> {
        reserve_units(ex, text.len(), 1)?;
        let r = OwnedReader::new(Arc::from(text.units()));
        *self
            .st
            .try_borrow_mut()
            .map_err(|_| HostError::key(msg::NO_SOURCE))? = Some(JsonReaderState {
            reader: r,
            current: None,
            consumed: false,
        });
        Ok(())
    }

    fn open_file(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<()> {
        let path = arg_str(ex, args.first())?.to_string_lossy();
        let ev = arg_value(ex, args.get(1))?;
        let (enc, _) = encoding_from_value(ex, &ev, 2)?;
        let f = fs(ex, &path)?;
        if let Some(st) = f.get().stat(&path).map_err(fs_error)? {
            let n = usize::try_from(st.size).unwrap_or(usize::MAX);
            reserve_units(ex, n, 3)?;
        }
        let bytes = f.get().read(&path).map_err(fs_error)?;
        let units = codec::bytes_to_string(&bytes, enc);
        drop(bytes);
        self.open(ex, &Str::from_vec(units))
    }

    fn read(&self) -> CallResult<bool> {
        let mut s = state(self)?;
        s.consumed = false;
        match s.reader.next().map_err(lib_error)? {
            Some(t) => {
                s.current = Some(t);
                Ok(true)
            }
            None => {
                s.current = None;
                Ok(false)
            }
        }
    }

    fn skip(&self, ex: &mut Execution) -> CallResult<()> {
        let mut s = state(self)?;
        if !matches!(s.current, Some(Token::StartObject | Token::StartArray)) {
            return Ok(());
        }
        let mut depth = 1usize;
        while depth > 0 {
            ex.charge(1)?;
            match s.reader.next().map_err(lib_error)? {
                Some(t) => {
                    match t {
                        Token::StartObject | Token::StartArray => depth += 1,
                        Token::EndObject | Token::EndArray => depth -= 1,
                        _ => {}
                    }
                    s.current = Some(t);
                }
                None => return Err(HostError::key(bsl_lib::msg::BAD_STATE).into()),
            }
        }
        Ok(())
    }

    fn current_value(&self, ex: &mut Execution) -> CallResult<Value> {
        let s = state(self)?;
        match &s.current {
            Some(Token::PropertyName(u) | Token::String(u)) => {
                ex.charge(u.len() as u64 / 64)?;
                Ok(Value::String(Str::from_units(u)))
            }
            Some(Token::Number(n)) => Ok(Value::Number(n.clone())),
            Some(Token::Bool(b)) => Ok(Value::Bool(*b)),
            Some(Token::Null) => Ok(Value::Undefined),
            _ => Err(HostError::key(msg::NO_CURRENT).into()),
        }
    }
}

impl HostObject for JsonReader {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(types().reader)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        match property_name(types().reader, m) {
            Some("CurrentValue") => self.current_value(ex),
            Some("CurrentValueType") => {
                let name = token_type(state(self)?.current.as_ref());
                enum_value(types().value_type, name)
                    .ok_or_else(|| not_implemented(types().reader, m).into())
            }
            _ => Err(not_implemented(types().reader, m).into()),
        }
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        Err(not_implemented(types().reader, m).into())
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        match method_name(types().reader, m) {
            Some("SetString") => {
                let s = arg_str(ex, args.first())?;
                self.open(ex, &s)?;
                Ok(Value::Undefined)
            }
            Some("OpenFile") => {
                self.open_file(ex, args)?;
                Ok(Value::Undefined)
            }
            Some("Read") => Ok(Value::Bool(self.read()?)),
            Some("Skip") => {
                self.skip(ex)?;
                Ok(Value::Undefined)
            }
            Some("Close") => {
                if let Ok(mut g) = self.st.try_borrow_mut() {
                    *g = None;
                }
                Ok(Value::Undefined)
            }
            _ => Err(not_implemented(types().reader, m).into()),
        }
    }
}

fn ctor(_ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    if !args.is_empty() {
        return Err(ctor_not_implemented(types().reader).into());
    }
    Ok(Value::Object(Rc::new(JsonReader {
        st: RefCell::new(None),
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
