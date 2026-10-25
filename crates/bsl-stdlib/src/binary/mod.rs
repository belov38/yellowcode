mod buffer;
mod data_reader;
mod data_writer;
mod hashing;
mod stream;

use crate::bind::msg;
use crate::collections::{ctor_not_implemented, types};
use bsl_prim::Str;
use bsl_vm::{Arg, CallResult, Exception, Execution, Registry, TypeImpl, Value};
use platform_catalog::TypeIdx;
use platform_catalog::messages::{Lang, text_key};
use std::sync::Arc;

pub(crate) use buffer::BinBuf;
pub(crate) use hashing::function_presentation;
pub use stream::stream_bytes;
pub(crate) use stream::{
    SharedStream, new_read_only_stream, open_file, read_stream, stream_of, write_stream,
};

const MAX_BYTES: u64 = 1 << 31;

pub(crate) fn guard_bytes(ex: &mut Execution, n: u64) -> Result<(), Exception> {
    if n > MAX_BYTES {
        return Err(ex.out_of_memory());
    }
    ex.charge(n / 64)?;
    ex.reserve(n)
}

pub fn new_binary_buffer(ex: &mut Execution, bytes: Vec<u8>) -> Result<Value, Exception> {
    new_ordered(ex, bytes, false)
}

pub fn binary_buffer_bytes(v: &Value) -> Option<Vec<u8>> {
    BinBuf::of(v).map(BinBuf::bytes)
}

pub(crate) fn new_ordered(
    ex: &mut Execution,
    bytes: Vec<u8>,
    big: bool,
) -> Result<Value, Exception> {
    guard_bytes(ex, bytes.len() as u64)?;
    Ok(BinBuf::new_value(bytes, big))
}

fn ctor(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    if args.is_empty() || args.len() > 2 {
        return Err(ctor_not_implemented(types().binary_buffer).into());
    }
    let size = crate::bind::strict_count(ex, args.first(), 1, true)?;
    let big = buffer::order_arg(ex, args.get(1), 2, false)?;
    guard_bytes(ex, size)?;
    let size = usize::try_from(size).map_err(|_| ex.out_of_memory())?;
    let mut v = Vec::new();
    v.try_reserve_exact(size).map_err(|_| ex.out_of_memory())?;
    v.resize(size, 0);
    Ok(BinBuf::new_value(v, big))
}

pub(crate) fn byte_order_presentation(t: TypeIdx, name_en: &str, lang: Lang) -> Option<Str> {
    if t != types().byte_order {
        return None;
    }
    let key = match name_en {
        "BigEndian" => msg::BIG_ENDIAN,
        "LittleEndian" => msg::LITTLE_ENDIAN,
        _ => return None,
    };
    text_key(key, lang).map(Str::from)
}

pub(crate) fn register(reg: &mut Registry) {
    reg.type_impl(
        types().binary_buffer,
        TypeImpl {
            ctor: Some(Arc::new(ctor)),
        },
    );
    stream::register(reg);
    hashing::register(reg);
    let t = stream::stream_types();
    reg.type_impl(
        t.writer,
        TypeImpl {
            ctor: Some(Arc::new(data_writer::ctor)),
        },
    );
    reg.type_impl(
        t.reader,
        TypeImpl {
            ctor: Some(Arc::new(data_reader::ctor)),
        },
    );
}
