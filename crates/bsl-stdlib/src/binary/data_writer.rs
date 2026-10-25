use super::buffer::{BinBuf, order_arg};
use super::stream::{SharedStream, open_file, stream_of, stream_types, write_stream};
use crate::bind::{
    arg_str, arg_value, is_given, msg, strict_count, strict_i64, strict_number, type_mismatch,
    with_other,
};
use crate::collections::{method_name, not_implemented, property_name, property_ru};
use crate::enums::enum_value;
use crate::globals::data::encoding_from_value;
use crate::globals::reserve_units;
use crate::values::binary_data_bytes;
use bsl_lib::codec::{self, Encoding};
use bsl_lib::textio::writer_text;
use bsl_prim::Number;
use bsl_vm::{Arg, CallResult, Execution, HostError, HostObject, MemberId, TypeId, Value};
use std::any::Any;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const LF: [u16; 1] = [0x0A];
const CRLF: [u16; 2] = [0x0D, 0x0A];

struct DataWriter {
    s: SharedStream,
    target: Value,
    owns: bool,
    enc: Cell<Encoding>,
    enc_value: RefCell<Value>,
    big: Cell<bool>,
    sep: RefCell<Vec<u16>>,
    conv: RefCell<Vec<u16>>,
    closed: Cell<bool>,
    bom: RefCell<Vec<u8>>,
}

fn unsigned(n: &Number) -> Option<u64> {
    n.trunc().to_xml_string().parse().ok()
}

pub(super) fn ctor(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    let first = arg_value(ex, args.first())?;
    let by_name = matches!(first, Value::String(_));
    let (conv_at, bom_at) = if by_name { (5, 6) } else { (4, 5) };
    if args.is_empty() || args.len() > bom_at + 1 {
        return Err(crate::collections::ctor_not_implemented(stream_types().writer).into());
    }
    let enc_value = arg_value(ex, args.get(1))?;
    let (enc, _) = encoding_from_value(ex, &enc_value, 2)?;
    let big = order_arg(ex, args.get(2), 3, false)?;
    let sep = text_arg(ex, args.get(3), &LF)?;
    let append = by_name && is_given(args.get(4)) && crate::bind::arg_bool(ex, args.get(4))?;
    let conv = text_arg(ex, args.get(conv_at), &CRLF)?;
    let bom = is_given(args.get(bom_at)) && crate::bind::arg_bool(ex, args.get(bom_at))?;
    let (target, owns) = if by_name {
        let Value::String(path) = &first else {
            return Err(type_mismatch(1).into());
        };
        let mode = if append {
            "Дописать"
        } else {
            "Создать"
        };
        let v = open_file(ex, &path.to_string_lossy(), mode, Some("Запись"))?;
        (v, true)
    } else if stream_of(&first).is_some() {
        (first, false)
    } else {
        return Err(type_mismatch(1).into());
    };
    let Some(s) = stream_of(&target) else {
        return Err(type_mismatch(1).into());
    };
    let w = DataWriter {
        s,
        target,
        owns,
        enc: Cell::new(enc),
        enc_value: RefCell::new(enc_value),
        big: Cell::new(big),
        sep: RefCell::new(sep),
        conv: RefCell::new(conv),
        closed: Cell::new(false),
        bom: RefCell::new(if bom {
            codec::bom(enc).to_vec()
        } else {
            Vec::new()
        }),
    };
    Ok(Value::Object(Rc::new(w)))
}

fn text_arg(ex: &mut Execution, a: Option<&Arg>, default: &[u16]) -> CallResult<Vec<u16>> {
    if !is_given(a) {
        return Ok(default.to_vec());
    }
    Ok(arg_str(ex, a)?.units().to_vec())
}

impl DataWriter {
    fn open(&self, ex: &Execution) -> CallResult<()> {
        if self.closed.get() {
            return Err(super::stream::StreamError::Closed.into_call(ex));
        }
        Ok(())
    }

    fn put(&self, ex: &mut Execution, bytes: &[u8]) -> CallResult<Value> {
        let bom = std::mem::take(&mut *self.bom.borrow_mut());
        if bom.is_empty() {
            write_stream(ex, &self.s, bytes)?;
        } else {
            let mut all = bom;
            all.extend_from_slice(bytes);
            write_stream(ex, &self.s, &all)?;
        }
        Ok(Value::Undefined)
    }

    fn encoding(&self, ex: &mut Execution, a: Option<&Arg>, param: usize) -> CallResult<Encoding> {
        if !is_given(a) {
            return Ok(self.enc.get());
        }
        let v = arg_value(ex, a)?;
        if matches!(v, Value::Undefined) {
            return Ok(self.enc.get());
        }
        Ok(encoding_from_value(ex, &v, param)?.0)
    }

    fn encode(&self, ex: &mut Execution, text: &[u16], enc: Encoding) -> CallResult<Vec<u8>> {
        reserve_units(ex, text.len(), 4)?;
        let t = writer_text(text, &self.conv.borrow());
        Ok(codec::encode(&t, enc))
    }

    fn int(&self, ex: &mut Execution, args: &[Arg], width: usize) -> CallResult<Value> {
        let n = strict_number(ex, args.first(), 1, true)?;
        let big = order_arg(ex, args.get(1), 2, self.big.get())?;
        let max = if width == 8 {
            u64::MAX
        } else {
            (1u64 << (width * 8)) - 1
        };
        let Some(v) = n
            .is_integer()
            .then(|| unsigned(&n))
            .flatten()
            .filter(|v| *v <= max)
        else {
            return Err(with_other(HostError::key(msg::INTEGER_RANGE), true).into());
        };
        let bytes = if big {
            v.to_be_bytes()[8 - width..].to_vec()
        } else {
            v.to_le_bytes()[..width].to_vec()
        };
        self.put(ex, &bytes)
    }

    fn byte(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let n = strict_number(ex, args.first(), 1, true)?;
        let Some(b) = n
            .is_integer()
            .then(|| u8::try_from(n.trunc_to_i64_saturating()).ok())
            .flatten()
        else {
            return Err(with_other(HostError::key(msg::BYTE_RANGE), true).into());
        };
        self.put(ex, &[b])
    }

    fn buffer(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let v = arg_value(ex, args.first())?;
        let Some(b) = BinBuf::of(&v) else {
            return Err(with_other(type_mismatch(1), true).into());
        };
        let bytes = if is_given(args.get(1)) {
            let pos = strict_i64(ex, args.get(1), 2, true)?;
            let count = strict_count(ex, args.get(2), 3, true)?;
            b.stream_span(pos, count, false)?;
            b.get_range(pos, count as i64)?
        } else {
            b.bytes()
        };
        self.put(ex, &bytes)
    }

    fn binary(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let v = arg_value(ex, args.first())?;
        if let Some(d) = binary_data_bytes(&v) {
            return self.put(ex, d);
        }
        if let Some(b) = BinBuf::of(&v) {
            return self.put(ex, &b.bytes());
        }
        Err(with_other(type_mismatch(1), true).into())
    }

    fn chars(&self, ex: &mut Execution, args: &[Arg], line: bool) -> CallResult<Value> {
        let s = arg_str(ex, args.first())?;
        let enc = self.encoding(ex, args.get(1), 2)?;
        let mut text = s.units().to_vec();
        if line {
            let default = self.sep.borrow().clone();
            let sep = text_arg(ex, args.get(2), &default)?;
            text.extend_from_slice(&sep);
        }
        let bytes = self.encode(ex, &text, enc)?;
        self.put(ex, &bytes)
    }

    fn close(&self, ex: &mut Execution) -> CallResult<Value> {
        if self.closed.replace(true) {
            return Ok(Value::Undefined);
        }
        let mut b = self.s.borrow_mut();
        let r = if self.owns { b.close() } else { b.flush() };
        r.map_err(|e| e.into_call(ex))?;
        Ok(Value::Undefined)
    }
}

impl HostObject for DataWriter {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(stream_types().writer)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let t = stream_types().writer;
        let text = |u: &[u16]| Value::String(bsl_prim::Str::from_vec(u.to_vec()));
        match property_name(t, m) {
            Some("TextEncoding") => Ok(match &*self.enc_value.borrow() {
                Value::Undefined => crate::enums::type_by_id("TextEncoding")
                    .and_then(|t| enum_value(t, "UTF8"))
                    .unwrap_or_default(),
                v => v.clone(),
            }),
            Some("ByteOrder") => {
                let name = if self.big.get() {
                    "BigEndian"
                } else {
                    "LittleEndian"
                };
                enum_value(crate::collections::types().byte_order, name)
                    .ok_or_else(|| not_implemented(t, m).into())
            }
            Some("LineSplitter") => Ok(text(&self.sep.borrow())),
            Some("ConvertibleSplitterOfLines") => Ok(text(&self.conv.borrow())),
            _ => {
                let _ = ex;
                Err(not_implemented(t, m).into())
            }
        }
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        let t = stream_types().writer;
        match property_name(t, m) {
            Some(_) => Err(HostError::key(msg::PROP_NOT_WRITABLE)
                .with_arg(property_ru(t, m))
                .raw()
                .into()),
            None => Err(not_implemented(t, m).into()),
        }
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        let t = stream_types().writer;
        let args: &[Arg] = args;
        let name = method_name(t, m);
        if name == Some("Close") {
            return self.close(ex);
        }
        if name.is_none() {
            return Err(not_implemented(t, m).into());
        }
        self.open(ex)?;
        match name {
            Some("Write") => self.binary(ex, args),
            Some("WriteByte") => self.byte(ex, args),
            Some("WriteBinaryDataBuffer") => self.buffer(ex, args),
            Some("WriteChars") => self.chars(ex, args, false),
            Some("WriteLine") => self.chars(ex, args, true),
            Some("WriteInt16") => self.int(ex, args, 2),
            Some("WriteInt32") => self.int(ex, args, 4),
            Some("WriteInt64") => self.int(ex, args, 8),
            Some("Flush") => {
                self.s.borrow_mut().flush().map_err(|e| e.into_call(ex))?;
                Ok(Value::Undefined)
            }
            Some("TargetStream") => Ok(self.target.clone()),
            _ => Err(not_implemented(t, m).into()),
        }
    }
}
