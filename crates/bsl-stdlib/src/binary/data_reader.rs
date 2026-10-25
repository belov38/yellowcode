use super::buffer::{BinBuf, order_arg};
use super::stream::{
    SharedStream, StreamError, available, copy_stream, new_read_only_stream, read_stream, seek_to,
    stream_of, stream_types,
};
use super::{guard_bytes, new_ordered};
use crate::bind::{
    arg_i64, arg_str, arg_value, is_given, lib_error, msg, strict_count, strict_i64, type_mismatch,
    value_mismatch, with_other,
};
use crate::collections::{
    array_items, ctor_not_implemented, method_name, new_array, not_implemented, property_name,
    property_ru, reserve_values, types,
};
use crate::enums::{enum_value, type_by_id};
use crate::fs::{fs, fs_error};
use crate::globals::data::encoding_from_value;
use crate::values::{binary_data_arc, new_binary_data_arc};
use bsl_lib::codec::{self, CodePages, Encoding, EncodingName};
use bsl_prim::{Number, Str};
use bsl_vm::{Arg, CallResult, Execution, HostError, HostObject, MemberId, TypeId, Value};
use std::any::Any;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

const STEP: u64 = 8 << 10;

const LF: [u16; 1] = [0x0A];

struct DataReader {
    s: SharedStream,
    source: Value,
    enc: Cell<Encoding>,
    enc_value: RefCell<Value>,
    bad_name: RefCell<Option<Vec<u16>>>,
    big: Cell<bool>,
    sep: RefCell<Vec<u16>>,
    sep_default: bool,
    conv: RefCell<Vec<u16>>,
    closed: Cell<bool>,
    bom_pending: Cell<bool>,
}

pub(super) fn ctor(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    if args.is_empty() || args.len() > 5 {
        return Err(ctor_not_implemented(stream_types().reader).into());
    }
    let first = arg_value(ex, args.first())?;
    let enc_value = arg_value(ex, args.get(1))?;
    let (enc, bad_name) = match resolve_encoding(ex, &enc_value, 2)? {
        Ok((e, _)) => (e, None),
        Err(name) => (Encoding::Utf8, Some(name)),
    };
    let big = order_arg(ex, args.get(2), 3, false)?;
    let sep = text_arg(ex, args.get(3), &[])?;
    let sep_default = sep.is_empty();
    let sep = if sep_default { LF.to_vec() } else { sep };
    let conv = text_arg(ex, args.get(4), &[0x0D, 0x0A])?;
    let source = if stream_of(&first).is_some() {
        first
    } else if let Some(b) = binary_data_arc(&first) {
        new_read_only_stream(b)
    } else if let Value::String(path) = &first {
        let path = path.to_string_lossy();
        let f = fs(ex, &path)?;
        if let Some(st) = f.get().stat(&path).map_err(fs_error)? {
            guard_bytes(ex, st.size)?;
        }
        let data = f.get().read(&path).map_err(fs_error)?;
        new_read_only_stream(Arc::from(data))
    } else {
        return Err(type_mismatch(1).into());
    };
    let Some(s) = stream_of(&source) else {
        return Err(type_mismatch(1).into());
    };
    Ok(Value::Object(Rc::new(DataReader {
        s,
        source,
        enc: Cell::new(enc),
        enc_value: RefCell::new(enc_value),
        bad_name: RefCell::new(bad_name),
        big: Cell::new(big),
        sep: RefCell::new(sep),
        sep_default,
        conv: RefCell::new(conv),
        closed: Cell::new(false),
        bom_pending: Cell::new(true),
    })))
}

fn resolve_encoding(
    ex: &mut Execution,
    v: &Value,
    param: usize,
) -> CallResult<Result<(Encoding, bool), Vec<u16>>> {
    if let Value::String(s) = v {
        return Ok(
            codec::resolve(EncodingName::Named(s.units()), &CodePages::STAND)
                .map_err(|_| s.units().to_vec()),
        );
    }
    Ok(Ok(encoding_from_value(ex, v, param)?))
}

fn bad_name_error(name: &[u16]) -> bsl_vm::CallError {
    let e = match codec::resolve(EncodingName::Named(name), &CodePages::STAND) {
        Err(e) => lib_error(e),
        Ok(_) => HostError::key(msg::CHAR_CONVERSION),
    };
    with_other(e, false).into()
}

fn text_arg(ex: &mut Execution, a: Option<&Arg>, default: &[u16]) -> CallResult<Vec<u16>> {
    if !is_given(a) {
        return Ok(default.to_vec());
    }
    Ok(arg_str(ex, a)?.units().to_vec())
}

fn char_width(b: &[u8], enc: Encoding) -> usize {
    let w = match enc {
        Encoding::Utf8 => match b.first() {
            Some(0xC0..=0xDF) => 2,
            Some(0xE0..=0xEF) => 3,
            Some(0xF0..=0xF7) => 4,
            _ => 1,
        },
        Encoding::Utf16Le | Encoding::Utf16Be => {
            let unit = |i: usize| match (b.get(i), b.get(i + 1)) {
                (Some(&x), Some(&y)) if enc == Encoding::Utf16Le => u16::from_le_bytes([x, y]),
                (Some(&x), Some(&y)) => u16::from_be_bytes([x, y]),
                _ => 0,
            };
            if (0xD800..0xDC00).contains(&unit(0)) && (0xDC00..0xE000).contains(&unit(2)) {
                4
            } else {
                2
            }
        }
        Encoding::Utf32Le | Encoding::Utf32Be => 4,
        Encoding::Single(_) => 1,
    };
    w.min(b.len()).max(1)
}

fn unit_size(enc: Encoding) -> usize {
    match enc {
        Encoding::Utf16Le | Encoding::Utf16Be => 2,
        Encoding::Utf32Le | Encoding::Utf32Be => 4,
        _ => 1,
    }
}

fn find(
    win: &[u8],
    markers: &[Vec<u8>],
    from: usize,
    align: usize,
) -> Option<(usize, usize, usize)> {
    let mut i = from;
    while i < win.len() {
        let rest = win.get(i..)?;
        let mut hit: Option<(usize, usize)> = None;
        for (k, m) in markers.iter().enumerate() {
            if !m.is_empty() && rest.starts_with(m) && hit.is_none_or(|(l, _)| m.len() > l) {
                hit = Some((m.len(), k));
            }
        }
        if let Some((l, k)) = hit {
            return Some((i, l, k));
        }
        i += align;
    }
    None
}

struct Found {
    data: Vec<u8>,
    marker: usize,
    index: usize,
}

fn markers_of(ex: &mut Execution, v: &Value, enc: Encoding) -> CallResult<Vec<Vec<u8>>> {
    let units = |s: &Str| codec::encode(s.units(), enc);
    if let Value::String(s) = v {
        return Ok(vec![units(s)]);
    }
    let Some(items) = array_items(v) else {
        return Err(type_mismatch(1).into());
    };
    let mut out = Vec::with_capacity(items.len());
    for it in &items {
        let Value::String(s) = it else {
            return Err(type_mismatch(1).into());
        };
        out.push(units(s));
    }
    ex.charge(out.len() as u64 / 64)?;
    Ok(out)
}

fn number(n: u64) -> Value {
    Value::from(i64::try_from(n).unwrap_or(i64::MAX))
}

struct ReadResult {
    bytes: Arc<[u8]>,
    big: bool,
}

fn result_value(bytes: Vec<u8>, big: bool) -> Value {
    Value::Object(Rc::new(ReadResult {
        bytes: Arc::from(bytes),
        big,
    }))
}

impl DataReader {
    fn open(&self, ex: &Execution) -> CallResult<()> {
        if self.closed.get() || self.s.borrow().closed() {
            return Err(StreamError::Closed.into_call(ex));
        }
        Ok(())
    }

    fn own_encoding(&self) -> CallResult<Encoding> {
        match &*self.bad_name.borrow() {
            Some(name) => Err(bad_name_error(name)),
            None => Ok(self.enc.get()),
        }
    }

    fn encoding(&self, ex: &mut Execution, a: Option<&Arg>, param: usize) -> CallResult<Encoding> {
        if !is_given(a) {
            return self.own_encoding();
        }
        let v = arg_value(ex, a)?;
        if matches!(v, Value::Undefined) {
            return self.own_encoding();
        }
        match resolve_encoding(ex, &v, param)? {
            Ok((e, _)) => Ok(e),
            Err(name) => Err(bad_name_error(&name)),
        }
    }

    fn skip_bom(&self, ex: &mut Execution, enc: Encoding) -> CallResult<()> {
        if !self.bom_pending.replace(false) || matches!(enc, Encoding::Utf8 | Encoding::Single(_)) {
            return Ok(());
        }
        let bom = codec::bom(enc);
        let (start, head) = self.peek(ex, bom.len() as u64)?;
        if start == 0 && !bom.is_empty() && head == bom {
            seek_to(ex, &self.s, start + bom.len() as u64)?;
        }
        Ok(())
    }

    fn take(&self, ex: &mut Execution, n: u64) -> CallResult<Vec<u8>> {
        read_stream(ex, &self.s, n)
    }

    fn peek(&self, ex: &mut Execution, n: u64) -> CallResult<(u64, Vec<u8>)> {
        let start = self.s.borrow().position();
        let data = read_stream(ex, &self.s, n)?;
        seek_to(ex, &self.s, start)?;
        Ok((start, data))
    }

    fn search(&self, ex: &mut Execution, markers: &[Vec<u8>], enc: Encoding) -> CallResult<Found> {
        let align = unit_size(enc);
        let longest = markers.iter().map(Vec::len).max().unwrap_or(0);
        let start = self.s.borrow().position();
        let mut win: Vec<u8> = Vec::new();
        let mut from = 0usize;
        let mut hit = None;
        if longest > 0 {
            loop {
                let chunk = read_stream(ex, &self.s, STEP)?;
                if chunk.is_empty() {
                    break;
                }
                win.extend_from_slice(&chunk);
                if let Some(h) = find(&win, markers, from, align) {
                    hit = Some(h);
                    break;
                }
                let back = win.len().saturating_sub(longest - 1);
                from = back - back % align;
            }
        } else {
            win = read_stream(ex, &self.s, u64::MAX)?;
        }
        let Some((at, len, index)) = hit else {
            return Ok(Found {
                data: win,
                marker: 0,
                index: 0,
            });
        };
        seek_to(ex, &self.s, start.saturating_add((at + len) as u64))?;
        win.truncate(at);
        Ok(Found {
            data: win,
            marker: len,
            index,
        })
    }

    fn decode(&self, ex: &mut Execution, bytes: &[u8], enc: Encoding) -> CallResult<Value> {
        crate::globals::reserve_units(ex, bytes.len(), 2)?;
        Ok(Value::String(Str::from_vec(codec::decode(bytes, enc))))
    }

    fn byte(&self, ex: &mut Execution) -> CallResult<Value> {
        let b = self.take(ex, 1)?;
        Ok(b.first()
            .map_or(Value::Undefined, |b| number(u64::from(*b))))
    }

    fn int(&self, ex: &mut Execution, args: &[Arg], width: usize) -> CallResult<Value> {
        let big = order_arg(ex, args.first(), 1, self.big.get())?;
        let (_, b) = self.peek(ex, width as u64)?;
        if b.len() < width {
            return Ok(Value::Undefined);
        }
        self.take(ex, width as u64)?;
        let mut v = 0u64;
        if big {
            for x in &b {
                v = (v << 8) | u64::from(*x);
            }
        } else {
            for x in b.iter().rev() {
                v = (v << 8) | u64::from(*x);
            }
        }
        Ok(Value::Number(Number::from_u64(v)))
    }

    fn line(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let enc = self.encoding(ex, args.first(), 1)?;
        self.skip_bom(ex, enc)?;
        let given = text_arg(ex, args.get(1), &[])?;
        let sep = if !given.is_empty() {
            Some(given)
        } else if self.sep_default {
            None
        } else {
            Some(self.sep.borrow().clone())
        };
        let markers = match sep {
            Some(sep) => vec![codec::encode(&sep, enc)],
            None => vec![
                codec::encode(&[0x0D, 0x0A], enc),
                codec::encode(&LF, enc),
                codec::encode(&[0x0D], enc),
            ],
        };
        let f = self.search(ex, &markers, enc)?;
        self.decode(ex, &f.data, enc)
    }

    fn chars(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let enc = self.encoding(ex, args.get(1), 2)?;
        self.skip_bom(ex, enc)?;
        let n = if is_given(args.first()) {
            strict_count(ex, args.first(), 1, true)?
        } else {
            u64::MAX
        };
        let (start, bytes) = self.peek(ex, n.saturating_mul(4))?;
        let (mut at, mut seen) = (0usize, 0u64);
        while seen < n && at < bytes.len() {
            at += char_width(&bytes[at..], enc);
            seen += 1;
        }
        seek_to(ex, &self.s, start + at as u64)?;
        self.decode(ex, &bytes[..at], enc)
    }

    fn read(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let n = self.count(ex, args.first(), 1)?;
        let bytes = self.take(ex, n)?;
        Ok(result_value(bytes, self.big.get()))
    }

    fn count(&self, ex: &mut Execution, a: Option<&Arg>, param: usize) -> CallResult<u64> {
        if !is_given(a) {
            return Ok(u64::MAX);
        }
        strict_count(ex, a, param, true)
    }

    fn skip(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let n = self.count(ex, args.first(), 1)?;
        let left = available(ex, &self.s)?;
        let k = n.min(left);
        let pos = self.s.borrow().position();
        seek_to(ex, &self.s, pos + k)?;
        Ok(number(k))
    }

    fn marker_args(
        &self,
        ex: &mut Execution,
        args: &[Arg],
    ) -> CallResult<(Vec<Vec<u8>>, Encoding)> {
        let v = arg_value(ex, args.first())?;
        let enc = self.encoding(ex, args.get(1), 2)?;
        Ok((markers_of(ex, &v, enc)?, enc))
    }

    fn read_to(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let (markers, enc) = self.marker_args(ex, args)?;
        let f = self.search(ex, &markers, enc)?;
        Ok(result_value(f.data, self.big.get()))
    }

    fn skip_to(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let (markers, enc) = self.marker_args(ex, args)?;
        let f = self.search(ex, &markers, enc)?;
        Ok(if f.marker == 0 {
            Value::from(-1i64)
        } else {
            number(f.index as u64)
        })
    }

    fn split(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let (markers, enc) = self.marker_args(ex, args)?;
        let rest = self.take(ex, u64::MAX)?;
        let align = unit_size(enc);
        let mut parts: Vec<Vec<u8>> = Vec::new();
        let (mut start, mut from) = (0usize, 0usize);
        while let Some((at, len, _)) = find(&rest, &markers, from, align) {
            parts.push(rest.get(start..at).unwrap_or_default().to_vec());
            start = at + len;
            from = start;
        }
        parts.push(rest.get(start..).unwrap_or_default().to_vec());
        self.parts_value(ex, parts)
    }

    fn split_parts(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let n = arg_i64(ex, args.first(), 1)?;
        let n = usize::try_from(n)
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| value_mismatch(1))?;
        let rest = self.take(ex, u64::MAX)?;
        let parts = rest.chunks(n).map(<[u8]>::to_vec).collect();
        self.parts_value(ex, parts)
    }

    fn parts_value(&self, ex: &mut Execution, parts: Vec<Vec<u8>>) -> CallResult<Value> {
        reserve_values(ex, parts.len() as u64)?;
        ex.charge(parts.len() as u64)?;
        let big = self.big.get();
        Ok(new_array(
            parts.into_iter().map(|p| result_value(p, big)).collect(),
        ))
    }

    fn read_buffer(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let first = arg_value(ex, args.first())?;
        let Some(buf) = BinBuf::of(&first) else {
            let n = self.count(ex, args.first(), 1)?;
            let bytes = self.take(ex, n)?;
            return Ok(new_ordered(ex, bytes, self.big.get())?);
        };
        let pos = strict_i64(ex, args.get(1), 2, true)?;
        let count = strict_count(ex, args.get(2), 3, true)?;
        buf.stream_span(pos, count, true)?;
        let bytes = self.take(ex, count)?;
        buf.put_range(pos, &bytes)?;
        Ok(number(bytes.len() as u64))
    }

    fn copy_to(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let v = arg_value(ex, args.first())?;
        let Some(dst) = stream_of(&v) else {
            return Err(type_mismatch(1).into());
        };
        let count = if is_given(args.get(1)) {
            Some(self.count(ex, args.get(1), 2)?)
        } else {
            None
        };
        let n = copy_stream(ex, &self.s, &dst, count)?;
        Ok(number(n))
    }
}

impl HostObject for DataReader {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(stream_types().reader)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let t = stream_types().reader;
        let text = |u: &[u16]| Value::String(Str::from_vec(u.to_vec()));
        match property_name(t, m) {
            Some("ReadCompleted") => {
                self.open(ex)?;
                Ok(Value::Bool(available(ex, &self.s)? == 0))
            }
            Some("TextEncoding") => Ok(match &*self.enc_value.borrow() {
                Value::Undefined => type_by_id("TextEncoding")
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
                enum_value(types().byte_order, name).ok_or_else(|| not_implemented(t, m).into())
            }
            Some("LineSplitter") => Ok(text(&self.sep.borrow())),
            Some("ConvertedLineDelimiter") => Ok(text(&self.conv.borrow())),
            _ => Err(not_implemented(t, m).into()),
        }
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        let t = stream_types().reader;
        match property_name(t, m) {
            Some(_) => Err(HostError::key(msg::PROP_NOT_WRITABLE)
                .with_arg(property_ru(t, m))
                .raw()
                .into()),
            None => Err(not_implemented(t, m).into()),
        }
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        let t = stream_types().reader;
        let args: &[Arg] = args;
        let name = method_name(t, m);
        match name {
            Some("Close") => {
                self.closed.set(true);
                return Ok(Value::Undefined);
            }
            Some("SourceStream") => {
                self.open(ex)?;
                return Ok(self.source.clone());
            }
            None => return Err(not_implemented(t, m).into()),
            Some(_) => {}
        }
        if self.closed.get() {
            return Ok(Value::Undefined);
        }
        self.open(ex)?;
        match name {
            Some("ReadByte") => self.byte(ex),
            Some("ReadInt16") => self.int(ex, args, 2),
            Some("ReadInt32") => self.int(ex, args, 4),
            Some("ReadInt64") => self.int(ex, args, 8),
            Some("ReadLine") => self.line(ex, args),
            Some("ReadChars") => self.chars(ex, args),
            Some("Read") => self.read(ex, args),
            Some("ReadTo") => self.read_to(ex, args),
            Some("ReadIntoBinaryDataBuffer") => self.read_buffer(ex, args),
            Some("Skip") => self.skip(ex, args),
            Some("SkipTo") => self.skip_to(ex, args),
            Some("Split") => self.split(ex, args),
            Some("SplitInPartsOf") => self.split_parts(ex, args),
            Some("CopyTo") => self.copy_to(ex, args),
            _ => Err(not_implemented(t, m).into()),
        }
    }
}

impl HostObject for ReadResult {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(stream_types().result)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        Err(not_implemented(stream_types().result, m).into())
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        Err(not_implemented(stream_types().result, m).into())
    }

    fn call(&self, ex: &mut Execution, m: MemberId, _args: &mut [Arg]) -> CallResult<Value> {
        let t = stream_types().result;
        match method_name(t, m) {
            Some("GetBinaryData") => Ok(new_binary_data_arc(self.bytes.clone())),
            Some("GetBinaryDataBuffer") => Ok(new_ordered(ex, self.bytes.to_vec(), self.big)?),
            Some("OpenStreamForRead") => Ok(new_read_only_stream(self.bytes.clone())),
            _ => Err(not_implemented(t, m).into()),
        }
    }
}
