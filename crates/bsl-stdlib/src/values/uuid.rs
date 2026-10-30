use crate::bind::{arg_value, value_mismatch};
use crate::collections::{ctor_not_implemented, not_implemented, types};
use bsl_prim::{Str, boost_hash};
use bsl_vm::{
    Arg, CallResult, Exception, Execution, HostObject, MemberId, SharedObject, SharedValue, TypeId,
    Value,
};
use std::any::Any;
use std::rc::Rc;
use std::sync::Arc;

pub(crate) struct Uuid {
    bytes: [u8; 16],
}

impl Uuid {
    fn of(v: &Value) -> Option<&Uuid> {
        v.as_object()?.as_any().downcast_ref::<Uuid>()
    }
}

impl HostObject for Uuid {
    fn session_edges(&self, _: &mut dyn bsl_vm::SessionEdges) -> bool {
        true
    }
    fn session_transfer_supported(&self) -> bool {
        true
    }
    fn session_transfer(
        &self,
        out: &mut dyn bsl_vm::SessionExport,
    ) -> Result<Option<Box<dyn bsl_vm::SessionObject>>, Exception> {
        out.reserve(16)?;
        Ok(Some(Box::new(SessionUuid(self.bytes))))
    }
    fn retained_memory(&self, _: &mut dyn bsl_vm::RetainedMemory) -> Result<bool, Exception> {
        Ok(true)
    }
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(types().uuid)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        Err(not_implemented(types().uuid, m).into())
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        Err(not_implemented(types().uuid, m).into())
    }

    fn call(&self, _ex: &mut Execution, m: MemberId, _args: &mut [Arg]) -> CallResult<Value> {
        Err(not_implemented(types().uuid, m).into())
    }

    fn presentation(&self, _ex: &mut Execution) -> CallResult<Option<Str>> {
        Ok(Some(Str::from(uuid_string(&self.bytes))))
    }

    fn boost_hash(&self) -> Option<i32> {
        Some(boost_hash::uuid(&self.bytes))
    }

    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        let o = other.as_any().downcast_ref::<Uuid>()?;
        Some(o.bytes == self.bytes)
    }

    fn to_shared(&self) -> Option<SharedValue> {
        Some(SharedValue::Object(Arc::new(SharedUuid(self.bytes))))
    }
}

struct SessionUuid([u8; 16]);
impl bsl_vm::SessionObject for SessionUuid {
    fn retained_memory(&self, _: &mut dyn bsl_vm::RetainedMemory) -> Result<(), Exception> {
        Ok(())
    }
    fn create(
        &self,
        _: &mut Execution,
        _: &mut bsl_vm::SessionImports,
    ) -> Result<Value, Exception> {
        Ok(new_uuid(self.0))
    }
    fn fill(&self, _: &mut Execution, _: &Value, _: Vec<Value>) -> Result<(), Exception> {
        Ok(())
    }
    fn clear(&self, _: &Value) {}
    fn restore_bytes(&self, _: usize) -> u64 {
        32
    }
}

struct SharedUuid([u8; 16]);

impl SharedObject for SharedUuid {
    fn to_value(&self, _ex: &mut Execution) -> Result<Value, Exception> {
        Ok(new_uuid(self.0))
    }
}

pub(super) fn ctor(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    if args.len() > 1 {
        return Err(new_not_implemented());
    }
    match arg_value(ex, args.first())? {
        Value::Undefined if args.iter().all(|a| matches!(a, Arg::Omitted)) => {
            Ok(new_uuid(random_v4()))
        }
        Value::String(s) => match parse_uuid(s.units()) {
            Some(b) => Ok(new_uuid(b)),
            None => Err(value_mismatch(1).into()),
        },
        Value::Number(_) => Err(new_not_implemented()),
        _ => Err(value_mismatch(1).into()),
    }
}

fn new_not_implemented() -> bsl_vm::CallError {
    ctor_not_implemented(types().uuid).into()
}

fn random_v4() -> [u8; 16] {
    let mut b: [u8; 16] = rand::random();
    b[6] = (b[6] & 0x0F) | 0x40;
    b[8] = (b[8] & 0x3F) | 0x80;
    b
}

pub fn parse_uuid(units: &[u16]) -> Option<[u8; 16]> {
    if units.len() != 36 {
        return None;
    }
    let digit = |u: u16| char::from_u32(u32::from(u))?.to_digit(16);
    let mut out = [0u8; 16];
    let mut nibbles = 0;
    for (i, &u) in units.iter().enumerate() {
        if matches!(i, 8 | 13 | 18 | 23) {
            if u != u16::from(b'-') {
                return None;
            }
            continue;
        }
        let d = digit(u)? as u8;
        out[nibbles / 2] |= if nibbles % 2 == 0 { d << 4 } else { d };
        nibbles += 1;
    }
    Some(out)
}

pub fn uuid_string(b: &[u8; 16]) -> String {
    let mut s = String::with_capacity(36);
    for (i, x) in b.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            s.push('-');
        }
        s.push_str(&format!("{x:02x}"));
    }
    s
}

pub fn new_uuid(bytes: [u8; 16]) -> Value {
    Value::Object(Rc::new(Uuid { bytes }))
}

pub fn uuid_bytes(v: &Value) -> Option<[u8; 16]> {
    Uuid::of(v).map(|u| u.bytes)
}

pub(super) struct UuidCodec;

impl crate::serial::codec::ValueCodec for UuidCodec {
    fn internal_write(
        &self,
        _ex: &mut Execution,
        v: &bsl_vm::ObjRef,
        _w: &mut crate::serial::codec::InternalWriter,
    ) -> Result<Option<Vec<brace::Node>>, Exception> {
        Ok(Uuid::of(&Value::Object(v.clone()))
            .map(|u| vec![brace::Node::Atom(uuid_string(&u.bytes))]))
    }

    fn internal_read(
        &self,
        ex: &mut Execution,
        items: &[brace::Node],
        r: &mut crate::serial::codec::InternalReader,
    ) -> Result<Value, Exception> {
        let units: Option<Vec<u16>> = items
            .first()
            .and_then(brace::Node::as_atom)
            .map(|a| a.encode_utf16().collect());
        match units.as_deref().and_then(parse_uuid) {
            Some(b) => Ok(new_uuid(b)),
            None => Err(r.format_error(ex)),
        }
    }
}
