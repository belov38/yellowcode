use super::{guard_bytes, new_ordered};
use crate::bind::{
    arg_i64, arg_value, is_given, msg, strict_i64, strict_number, type_mismatch, value_mismatch,
    with_other,
};
use crate::collections::{
    array_items, method_name, new_array, no_field, not_implemented, property_name, property_ru,
    reserve_values, types,
};
use crate::enums::enum_value;
use crate::globals::{enum_arg, enum_of};
use bsl_prim::Number;
use bsl_vm::{Arg, CallResult, Execution, HostError, HostObject, MemberId, TypeId, Value};
use std::any::Any;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

pub(crate) struct BinBuf {
    store: Rc<RefCell<Vec<u8>>>,
    off: usize,
    len: usize,
    big: Cell<bool>,
    ro: Cell<bool>,
}

fn range() -> HostError {
    HostError::key(msg::RANGE)
}

fn readonly() -> HostError {
    HostError::key(msg::BUFFER_READONLY)
}

pub(super) fn order_arg(
    ex: &mut Execution,
    a: Option<&Arg>,
    param: usize,
    default: bool,
) -> CallResult<bool> {
    Ok(match enum_arg(ex, a, "ПорядокБайтов", param)? {
        Some("BigEndian") => true,
        Some(_) => false,
        None => default,
    })
}

fn buffer_arg(ex: &mut Execution, a: Option<&Arg>, param: usize) -> CallResult<Vec<u8>> {
    let v = arg_value(ex, a)?;
    let Some(b) = BinBuf::of(&v) else {
        return Err(type_mismatch(param).into());
    };
    ex.charge(b.len as u64 / 64)?;
    Ok(b.bytes())
}

fn number_u64(n: &Number) -> Option<u64> {
    n.trunc().to_xml_string().parse().ok()
}

#[derive(Clone, Copy)]
enum Op {
    Copy,
    And,
    Or,
    Xor,
    AndNot,
}

impl Op {
    fn apply(self, dst: u8, src: u8) -> u8 {
        match self {
            Op::Copy => src,
            Op::And => dst & src,
            Op::Or => dst | src,
            Op::Xor => dst ^ src,
            Op::AndNot => dst & !src,
        }
    }
}

impl BinBuf {
    pub(crate) fn of(v: &Value) -> Option<&BinBuf> {
        v.as_object()?.as_any().downcast_ref::<BinBuf>()
    }

    pub(crate) fn new_value(bytes: Vec<u8>, big: bool) -> Value {
        let len = bytes.len();
        Value::Object(Rc::new(BinBuf {
            store: Rc::new(RefCell::new(bytes)),
            off: 0,
            len,
            big: Cell::new(big),
            ro: Cell::new(false),
        }))
    }

    pub(crate) fn stream_span(&self, pos: i64, count: u64, write: bool) -> Result<(), HostError> {
        if write {
            self.writable()?;
        }
        if pos < 0 {
            return Err(with_other(range(), true));
        }
        let end = u64::try_from(pos).ok().and_then(|p| p.checked_add(count));
        if end.is_none_or(|e| e > self.len as u64) {
            return Err(with_other(HostError::key(msg::BUFFER_OVERFLOW), false));
        }
        Ok(())
    }

    pub(crate) fn get_range(&self, pos: i64, count: i64) -> Result<Vec<u8>, HostError> {
        let at = self.span(pos, count)?;
        let st = self.store.borrow();
        let n = usize::try_from(count).map_err(|_| range())?;
        Ok(st.get(at..at + n).ok_or_else(range)?.to_vec())
    }

    pub(crate) fn put_range(&self, pos: i64, data: &[u8]) -> Result<(), HostError> {
        self.writable()?;
        let at = self.span(pos, data.len() as i64)?;
        let mut st = self.store.borrow_mut();
        st.get_mut(at..at + data.len())
            .ok_or_else(range)?
            .copy_from_slice(data);
        Ok(())
    }

    pub(crate) fn window(&self) -> (Rc<RefCell<Vec<u8>>>, usize, usize, bool) {
        (self.store.clone(), self.off, self.len, self.ro.get())
    }

    pub(crate) fn bytes(&self) -> Vec<u8> {
        let st = self.store.borrow();
        st.get(self.off..self.off + self.len)
            .map_or_else(Vec::new, <[u8]>::to_vec)
    }

    fn span(&self, pos: i64, count: i64) -> Result<usize, HostError> {
        let end = pos.checked_add(count).ok_or_else(range)?;
        if pos < 0 || count < 0 || end > self.len as i64 {
            return Err(range());
        }
        Ok(self.off + pos as usize)
    }

    fn writable(&self) -> Result<(), HostError> {
        if self.ro.get() {
            Err(readonly())
        } else {
            Ok(())
        }
    }

    fn byte(&self, pos: i64) -> Result<Value, HostError> {
        let at = self.span(pos, 1)?;
        let st = self.store.borrow();
        Ok(Value::from(i64::from(
            st.get(at).copied().ok_or_else(range)?,
        )))
    }

    fn set_byte(&self, pos: i64, v: &Value, param: usize) -> CallResult<()> {
        self.writable()?;
        let n = match v {
            Value::Number(n) => n,
            _ if param == 0 => return Err(HostError::key(msg::PROPERTY_TYPE_MISMATCH).into()),
            _ => return Err(type_mismatch(param).into()),
        };
        let b = n
            .is_integer()
            .then(|| u8::try_from(n.trunc_to_i64_saturating()).ok())
            .flatten();
        let Some(b) = b else {
            return Err(HostError::key(msg::BYTE_RANGE).into());
        };
        let at = self.span(pos, 1)?;
        if let Some(x) = self.store.borrow_mut().get_mut(at) {
            *x = b;
        }
        Ok(())
    }

    fn write_op(&self, ex: &mut Execution, args: &[Arg], op: Op) -> CallResult<Value> {
        self.writable()?;
        let pos = strict_i64(ex, args.first(), 1, false)?;
        let src = buffer_arg(ex, args.get(1), 2)?;
        let mut count = if is_given(args.get(2)) {
            strict_i64(ex, args.get(2), 3, false)?
        } else {
            src.len() as i64
        };
        if count < 0 || count > src.len() as i64 || pos < 0 || pos > self.len as i64 {
            return Err(range().into());
        }
        let room = self.len - pos as usize;
        if matches!(op, Op::Copy) {
            count = count.min(room as i64);
        }
        let n = count as usize;
        let at = self.span(pos, n as i64)?;
        let mut st = self.store.borrow_mut();
        if let Some(dst) = st.get_mut(at..at + n) {
            for (d, s) in dst.iter_mut().zip(&src[..n]) {
                *d = op.apply(*d, *s);
            }
        }
        Ok(Value::Undefined)
    }

    fn invert(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        self.writable()?;
        let pos = if is_given(args.first()) {
            arg_i64(ex, args.first(), 1)?
        } else {
            0
        };
        let count = if is_given(args.get(1)) {
            arg_i64(ex, args.get(1), 2)?
        } else {
            (self.len as i64).saturating_sub(pos)
        };
        let at = self.span(pos, count)?;
        ex.charge(count as u64 / 64)?;
        if let Some(dst) = self.store.borrow_mut().get_mut(at..at + count as usize) {
            for d in dst {
                *d = !*d;
            }
        }
        Ok(Value::Undefined)
    }

    fn read_int(&self, ex: &mut Execution, args: &[Arg], width: usize) -> CallResult<Value> {
        let pos = strict_i64(ex, args.first(), 1, false)?;
        let big = order_arg(ex, args.get(1), 2, self.big.get())?;
        let at = self.span(pos, width as i64)?;
        let st = self.store.borrow();
        let mut raw = [0u8; 8];
        let src = st.get(at..at + width).ok_or_else(range)?;
        let mut v = 0u64;
        if big {
            for b in src {
                v = (v << 8) | u64::from(*b);
            }
        } else {
            raw[..width].copy_from_slice(src);
            v = u64::from_le_bytes(raw);
        }
        Ok(Value::Number(Number::from_u64(v)))
    }

    fn write_int(&self, ex: &mut Execution, args: &[Arg], width: usize) -> CallResult<Value> {
        self.writable()?;
        let pos = strict_i64(ex, args.first(), 1, false)?;
        let n = strict_number(ex, args.get(1), 2, false)?;
        let big = order_arg(ex, args.get(2), 3, self.big.get())?;
        let max = if width == 8 {
            u64::MAX
        } else {
            (1u64 << (width * 8)) - 1
        };
        let Some(v) = n
            .is_integer()
            .then(|| number_u64(&n))
            .flatten()
            .filter(|v| *v <= max)
        else {
            return Err(HostError::key(msg::INTEGER_RANGE).into());
        };
        let at = self.span(pos, width as i64)?;
        let bytes = if big {
            v.to_be_bytes()[8 - width..].to_vec()
        } else {
            v.to_le_bytes()[..width].to_vec()
        };
        if let Some(dst) = self.store.borrow_mut().get_mut(at..at + width) {
            dst.copy_from_slice(&bytes);
        }
        Ok(Value::Undefined)
    }

    fn slice(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let pos = arg_i64(ex, args.first(), 1)?;
        let count = if is_given(args.get(1)) {
            arg_i64(ex, args.get(1), 2)?
        } else {
            (self.len as i64).saturating_sub(pos)
        };
        let at = self.span(pos, count)?;
        Ok(Value::Object(Rc::new(BinBuf {
            store: self.store.clone(),
            off: at,
            len: count as usize,
            big: Cell::new(self.big.get()),
            ro: Cell::new(self.ro.get()),
        })))
    }

    fn read(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let pos = arg_i64(ex, args.first(), 1)?;
        let count = arg_i64(ex, args.get(1), 2)?;
        let at = self.span(pos, count)?;
        guard_bytes(ex, count as u64)?;
        let bytes = self
            .store
            .borrow()
            .get(at..at + count as usize)
            .map_or_else(Vec::new, <[u8]>::to_vec);
        Ok(Value::Object(Rc::new(self.derive(bytes))))
    }

    fn derive(&self, bytes: Vec<u8>) -> BinBuf {
        let len = bytes.len();
        BinBuf {
            store: Rc::new(RefCell::new(bytes)),
            off: 0,
            len,
            big: Cell::new(self.big.get()),
            ro: Cell::new(false),
        }
    }

    fn derived(&self, ex: &mut Execution, bytes: Vec<u8>) -> CallResult<Value> {
        Ok(new_ordered(ex, bytes, self.big.get())?)
    }

    fn split(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let v = arg_value(ex, args.first())?;
        let seps: Vec<Vec<u8>> = if let Some(items) = array_items(&v) {
            let mut out = Vec::with_capacity(items.len());
            for it in &items {
                let Some(b) = BinBuf::of(it) else {
                    return Err(type_mismatch(1).into());
                };
                out.push(b.bytes());
            }
            out
        } else if let Some(b) = BinBuf::of(&v) {
            vec![b.bytes()]
        } else {
            return Err(type_mismatch(1).into());
        };
        if seps.iter().any(Vec::is_empty) {
            return Err(value_mismatch(1).into());
        }
        let bytes = self.bytes();
        ex.charge(bytes.len() as u64 / 64)?;
        let mut ranges = Vec::new();
        let (mut start, mut i) = (0usize, 0usize);
        while i < bytes.len() {
            match seps.iter().find(|s| bytes[i..].starts_with(s)) {
                Some(s) => {
                    ranges.push(start..i);
                    i += s.len();
                    start = i;
                }
                None => i += 1,
            }
        }
        ranges.push(start..bytes.len());
        reserve_values(ex, ranges.len() as u64)?;
        ex.charge(ranges.len() as u64)?;
        let items = ranges
            .into_iter()
            .map(|r| Value::Object(Rc::new(self.derive(bytes[r].to_vec()))))
            .collect();
        Ok(new_array(items))
    }

    fn concat(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let v = arg_value(ex, args.first())?;
        let mut parts = vec![self.bytes()];
        if let Some(items) = array_items(&v) {
            for it in &items {
                let Some(b) = BinBuf::of(it) else {
                    return Err(type_mismatch(1).into());
                };
                parts.push(b.bytes());
            }
        } else if let Some(b) = BinBuf::of(&v) {
            parts.push(b.bytes());
        } else {
            return Err(type_mismatch(1).into());
        }
        let total = parts
            .iter()
            .fold(0u64, |a, p| a.saturating_add(p.len() as u64));
        guard_bytes(ex, total)?;
        self.derived(ex, parts.concat())
    }
}

impl HostObject for BinBuf {
    fn session_edges(&self, _: &mut dyn bsl_vm::SessionEdges) -> bool {
        true
    }
    fn session_transfer_supported(&self) -> bool {
        true
    }
    fn session_transfer(
        &self,
        out: &mut dyn bsl_vm::SessionExport,
    ) -> Result<Option<Box<dyn bsl_vm::SessionObject>>, bsl_vm::Exception> {
        out.reserve(std::mem::size_of::<SessionBuffer>() as u64)?;
        let bytes = self.store.borrow();
        let backing = out.backing(
            std::any::TypeId::of::<SessionBuffer>(),
            Rc::as_ptr(&self.store) as usize,
            &mut |out| {
                out.reserve(bytes.len() as u64 + bsl_vm::session_shared_bytes::<Vec<u8>>())?;
                Ok(std::sync::Arc::new(
                    crate::session::copy(&bytes).map_err(|_| out.memory_error())?,
                ))
            },
        )?;
        let Ok(bytes) = backing.downcast::<Vec<u8>>() else {
            return Ok(None);
        };
        Ok(Some(Box::new(SessionBuffer {
            bytes,
            off: self.off,
            len: self.len,
            big: self.big.get(),
            ro: self.ro.get(),
        })))
    }
    fn retained_memory(
        &self,
        visit: &mut dyn bsl_vm::RetainedMemory,
    ) -> Result<bool, bsl_vm::Exception> {
        visit.mutable();
        let bytes = self.store.borrow();
        visit.allocation(
            Rc::as_ptr(&self.store) as usize,
            (bytes.capacity() as u64).saturating_add(
                (std::mem::size_of::<RefCell<Vec<u8>>>() + 2 * std::mem::size_of::<usize>()) as u64,
            ),
        )?;
        Ok(true)
    }
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(types().binary_buffer)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<bsl_prim::Str>> {
        Ok(Some(crate::collections::type_name(
            ex,
            types().binary_buffer,
        )))
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let t = types().binary_buffer;
        match property_name(t, m) {
            Some("Size") => Ok(Value::from(self.len as i64)),
            Some("ReadOnly") => Ok(Value::Bool(self.ro.get())),
            Some("ByteOrder") => {
                let name = if self.big.get() {
                    "BigEndian"
                } else {
                    "LittleEndian"
                };
                let ty = types().byte_order;
                enum_value(ty, name).ok_or_else(|| not_implemented(t, m).into())
            }
            _ => Err(not_implemented(t, m).into()),
        }
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, v: Value) -> CallResult<()> {
        let t = types().binary_buffer;
        match property_name(t, m) {
            Some("ByteOrder") => match enum_of(&v, "ПорядокБайтов", 1)? {
                Some(name) => {
                    self.big.set(name == "BigEndian");
                    Ok(())
                }
                None => Err(type_mismatch(1).into()),
            },
            Some(_) => Err(HostError::key(msg::PROP_NOT_WRITABLE)
                .with_arg(property_ru(t, m))
                .raw()
                .into()),
            None => Err(not_implemented(t, m).into()),
        }
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        ex.with_reuse_check(|ex| {
            let t = types().binary_buffer;
            let args: &[Arg] = args;
            match method_name(t, m) {
                Some("Get") => {
                    let i = strict_i64(ex, args.first(), 1, false)?;
                    Ok(self.byte(i)?)
                }
                Some("Set") => {
                    let i = strict_i64(ex, args.first(), 1, false)?;
                    let v = arg_value(ex, args.get(1))?;
                    self.set_byte(i, &v, 2)?;
                    Ok(Value::Undefined)
                }
                Some("Read") => self.read(ex, args),
                Some("Write") => self.write_op(ex, args, Op::Copy),
                Some("WriteBitwiseAnd") => self.write_op(ex, args, Op::And),
                Some("WriteBitwiseOr") => self.write_op(ex, args, Op::Or),
                Some("WriteBitwiseXor") => self.write_op(ex, args, Op::Xor),
                Some("WriteBitwiseAndNot") => self.write_op(ex, args, Op::AndNot),
                Some("Invert") => self.invert(ex, args),
                Some("ReadInt16") => self.read_int(ex, args, 2),
                Some("ReadInt32") => self.read_int(ex, args, 4),
                Some("ReadInt64") => self.read_int(ex, args, 8),
                Some("WriteInt16") => self.write_int(ex, args, 2),
                Some("WriteInt32") => self.write_int(ex, args, 4),
                Some("WriteInt64") => self.write_int(ex, args, 8),
                Some("GetSlice") => self.slice(ex, args),
                Some("Copy") => {
                    guard_bytes(ex, self.len as u64)?;
                    let bytes = self.bytes();
                    Ok(Value::Object(Rc::new(self.derive(bytes))))
                }
                Some("Reverse") => {
                    guard_bytes(ex, self.len as u64)?;
                    let mut bytes = self.bytes();
                    bytes.reverse();
                    Ok(Value::Object(Rc::new(self.derive(bytes))))
                }
                Some("Split") if !args.is_empty() => self.split(ex, args),
                Some("Concat") => self.concat(ex, args),
                Some("SetReadOnly") => {
                    self.ro.set(true);
                    Ok(Value::Undefined)
                }
                _ => Err(not_implemented(t, m).into()),
            }
        })
    }

    fn index_get(&self, ex: &mut Execution, key: &Value) -> CallResult<Value> {
        let Value::Number(n) = key else {
            return Err(no_field(ex, key)?.into());
        };
        self.byte(n.trunc_to_i64_saturating())
            .map_err(|_| HostError::key(msg::INDEX_OFFBOUND).into())
    }

    fn index_set(&self, ex: &mut Execution, key: &Value, v: Value) -> CallResult<()> {
        ex.with_reuse_check(|ex| {
            let Value::Number(n) = key else {
                return Err(no_field(ex, key)?.into());
            };
            self.set_byte(n.trunc_to_i64_saturating(), &v, 0)
        })
    }

    fn iter_len(&self, _ex: &mut Execution) -> Option<usize> {
        Some(self.len)
    }

    fn iter_get(&self, _ex: &mut Execution, i: usize) -> CallResult<Value> {
        Ok(self.byte(i64::try_from(i).unwrap_or(i64::MAX))?)
    }
}

struct SessionBuffer {
    bytes: std::sync::Arc<Vec<u8>>,
    off: usize,
    len: usize,
    big: bool,
    ro: bool,
}
impl bsl_vm::SessionObject for SessionBuffer {
    fn retained_memory(
        &self,
        visit: &mut dyn bsl_vm::RetainedMemory,
    ) -> Result<(), bsl_vm::Exception> {
        visit
            .allocation(
                std::sync::Arc::as_ptr(&self.bytes) as usize,
                self.bytes.capacity() as u64 + bsl_vm::session_shared_bytes::<Vec<u8>>(),
            )
            .map(|_| ())
    }
    fn create(
        &self,
        ex: &mut Execution,
        imports: &mut bsl_vm::SessionImports,
    ) -> Result<Value, bsl_vm::Exception> {
        let store = imports.backing(ex, std::sync::Arc::as_ptr(&self.bytes) as usize, |ex| {
            Ok(Rc::new(RefCell::new(
                crate::session::copy(&self.bytes).map_err(|_| ex.out_of_memory())?,
            )))
        })?;
        Ok(Value::Object(Rc::new(BinBuf {
            store,
            off: self.off,
            len: self.len,
            big: Cell::new(self.big),
            ro: Cell::new(self.ro),
        })))
    }
    fn fill(&self, _: &mut Execution, _: &Value, _: Vec<Value>) -> Result<(), bsl_vm::Exception> {
        Ok(())
    }
    fn clear(&self, _: &Value) {}
    fn restore_bytes(&self, _: usize) -> u64 {
        self.bytes.len() as u64 + 128
    }
    fn restore_memory(
        &self,
        _: usize,
        visit: &mut dyn bsl_vm::RetainedMemory,
    ) -> Result<(), bsl_vm::Exception> {
        visit.bytes((std::mem::size_of::<BinBuf>() + 2 * std::mem::size_of::<usize>()) as u64)?;
        visit
            .allocation(
                std::sync::Arc::as_ptr(&self.bytes) as usize,
                (self.bytes.len()
                    + std::mem::size_of::<RefCell<Vec<u8>>>()
                    + 2 * std::mem::size_of::<usize>()) as u64,
            )
            .map(|_| ())
    }
}
