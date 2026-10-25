use crate::binary::{new_read_only_stream, stream_of, write_stream};
use crate::bind::{arg_str, arg_value, type_mismatch};
use crate::collections::{ctor_not_implemented, method_name, not_implemented, types};
use crate::fs::{fs, fs_error};
use bsl_prim::{Str, boost_hash};
use bsl_vm::{
    Arg, CallResult, Exception, Execution, HostObject, MemberId, SharedObject, SharedValue, TypeId,
    Value,
};
use std::any::Any;
use std::rc::Rc;
use std::sync::Arc;

pub(crate) struct BinaryData {
    bytes: Arc<[u8]>,
}

impl BinaryData {
    fn of(v: &Value) -> Option<&BinaryData> {
        v.as_object()?.as_any().downcast_ref::<BinaryData>()
    }
}

impl BinaryData {
    fn write(&self, ex: &mut Execution, m: MemberId, args: &[Arg]) -> CallResult<Value> {
        if args.len() != 1 {
            return Err(not_implemented(types().binary, m).into());
        }
        match arg_value(ex, args.first())? {
            Value::String(path) => {
                let path = path.to_string_lossy();
                let f = fs(ex, &path)?;
                ex.charge(self.bytes.len() as u64 / 64)?;
                f.get().write(&path, &self.bytes, false).map_err(fs_error)?;
                Ok(Value::Undefined)
            }
            v => match stream_of(&v) {
                Some(s) => {
                    write_stream(ex, &s, &self.bytes)?;
                    Ok(Value::Undefined)
                }
                None if matches!(v, Value::Object(_)) => {
                    Err(not_implemented(types().binary, m).into())
                }
                None => Err(type_mismatch(1).into()),
            },
        }
    }
}

pub(super) fn ctor(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    if args.len() != 1 {
        return Err(ctor_not_implemented(types().binary).into());
    }
    let path = arg_str(ex, args.first())?.to_string_lossy();
    let f = fs(ex, &path)?;
    if let Some(st) = f.get().stat(&path).map_err(fs_error)? {
        crate::binary::guard_bytes(ex, st.size)?;
    }
    let data = f.get().read(&path).map_err(fs_error)?;
    Ok(new_binary_data(data))
}

impl HostObject for BinaryData {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(types().binary)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        Err(not_implemented(types().binary, m).into())
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        Err(not_implemented(types().binary, m).into())
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        match method_name(types().binary, m) {
            Some("Size") => Ok(Value::from(self.bytes.len() as i64)),
            Some("Write") => self.write(ex, m, args),
            Some("OpenStreamForRead") => {
                ex.charge(1)?;
                Ok(new_read_only_stream(self.bytes.clone()))
            }
            _ => Err(not_implemented(types().binary, m).into()),
        }
    }

    fn presentation(&self, _ex: &mut Execution) -> CallResult<Option<Str>> {
        Ok(Some(Str::from(bsl_lib::hex::hex_spaced(&self.bytes))))
    }

    fn boost_hash(&self) -> Option<i32> {
        Some(boost_hash::bytes(&self.bytes))
    }

    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        let o = other.as_any().downcast_ref::<BinaryData>()?;
        Some(o.bytes == self.bytes)
    }

    fn to_shared(&self) -> Option<SharedValue> {
        Some(SharedValue::Object(Arc::new(SharedBinary(
            self.bytes.clone(),
        ))))
    }
}

struct SharedBinary(Arc<[u8]>);

impl SharedObject for SharedBinary {
    fn to_value(&self, _ex: &mut Execution) -> Result<Value, Exception> {
        Ok(Value::Object(Rc::new(BinaryData {
            bytes: self.0.clone(),
        })))
    }
}

pub fn new_binary_data(bytes: Vec<u8>) -> Value {
    Value::Object(Rc::new(BinaryData {
        bytes: Arc::from(bytes),
    }))
}

pub fn binary_data_bytes(v: &Value) -> Option<&[u8]> {
    BinaryData::of(v).map(|b| &*b.bytes)
}

pub(crate) fn new_binary_data_arc(bytes: Arc<[u8]>) -> Value {
    Value::Object(Rc::new(BinaryData { bytes }))
}

pub(crate) fn binary_data_bytes_direct(value: &Value) -> Option<&[u8]> {
    let object = value.as_object()?;
    let actual: &dyn Any = object.as_ref();
    actual
        .downcast_ref::<BinaryData>()
        .map(|data| data.bytes.as_ref())
}

pub(crate) fn binary_data_arc(v: &Value) -> Option<Arc<[u8]>> {
    BinaryData::of(v).map(|b| b.bytes.clone())
}

pub(super) struct BinaryCodec;

impl crate::serial::codec::ValueCodec for BinaryCodec {
    fn internal_write(
        &self,
        ex: &mut Execution,
        v: &bsl_vm::ObjRef,
        w: &mut crate::serial::codec::InternalWriter,
    ) -> Result<Option<Vec<brace::Node>>, Exception> {
        let Some(b) = BinaryData::of(&Value::Object(v.clone())).map(|b| b.bytes.clone()) else {
            return Ok(None);
        };
        let data = if w.is_storage() {
            ex.reserve(b.len() as u64)?;
            let n = w.attach(b.to_vec());
            brace::Node::int(n.map_or(0, |n| n as i64))
        } else {
            let text = (b.len() as u64).div_ceil(3).saturating_mul(4);
            ex.reserve(text.saturating_mul(2))?;
            w.account(ex, text + text / 32)?;
            brace::base64_atom(&b, brace::Newline::Lf, true)
        };
        Ok(Some(vec![brace::Node::List(vec![
            brace::Node::int(1),
            brace::Node::List(vec![data]),
        ])]))
    }

    fn internal_read(
        &self,
        ex: &mut Execution,
        items: &[brace::Node],
        r: &mut crate::serial::codec::InternalReader,
    ) -> Result<Value, Exception> {
        let atom = items
            .first()
            .and_then(brace::Node::as_list)
            .and_then(|l| l.get(1))
            .and_then(brace::Node::as_list)
            .and_then(|l| l.first())
            .and_then(brace::Node::as_atom);
        let Some(atom) = atom else {
            return Err(r.format_error(ex));
        };
        if atom.starts_with("#base64:") {
            ex.reserve((atom.len() as u64).saturating_mul(2))?;
            return match brace::Node::atom(atom).base64() {
                Some(b) => Ok(new_binary_data(b)),
                None => Err(r.format_error(ex)),
            };
        }
        match atom.parse::<usize>().ok().and_then(|n| r.stream(n)) {
            Some(b) => {
                ex.reserve(b.len() as u64)?;
                Ok(new_binary_data(b.to_vec()))
            }
            None if r.is_storage() && atom.parse::<usize>().is_ok() => {
                Ok(new_binary_data(Vec::new()))
            }
            None => Err(r.format_error(ex)),
        }
    }
}
