mod read;
pub(crate) mod text;
mod write;

use super::codec::{InternalReader, InternalWriter};
use crate::bind::{arg_str, arg_value, global};
use brace::Node;
use bsl_prim::Str;
use bsl_vm::{Arg, CallError, CallResult, Cause, Exception, Execution, HostError, Registry, Value};
pub use write::value_to_internal;

pub fn format_host_error() -> HostError {
    HostError::key("backend:IDS_ERR_CONVERSION_ERROR").caused_by(Cause::Host(Box::new(
        HostError::key("core85:IDS_LISTSTREAMERROR"),
    )))
}

pub fn value_from_internal(ex: &mut Execution, text: &[u16]) -> Result<Value, Exception> {
    let mut r = InternalReader::new();
    read_text(ex, text, &mut r)
}

pub fn read_text(
    ex: &mut Execution,
    text: &[u16],
    r: &mut InternalReader,
) -> Result<Value, Exception> {
    match read::parse(ex, text) {
        Ok(node) => r.read_value(ex, &node),
        Err(read::ParseError::Format) => Err(r.format_error(ex)),
        Err(read::ParseError::Exc(e)) => Err(e),
    }
}

pub fn value_to_node(ex: &mut Execution, v: &Value) -> Result<Node, Exception> {
    InternalWriter::new().write_value(ex, v)
}

pub fn value_from_node(ex: &mut Execution, n: &Node) -> Result<Value, Exception> {
    InternalReader::new().read_value(ex, n)
}

fn read_error(r: &InternalReader, e: Exception) -> CallError {
    if r.failed() {
        CallError::Host(format_host_error())
    } else {
        CallError::Exc(e)
    }
}

fn from_string(ex: &mut Execution, a: &mut [Arg]) -> CallResult<Value> {
    let s = arg_str(ex, a.first())?;
    let mut r = InternalReader::new();
    read_text(ex, s.units(), &mut r).map_err(|e| read_error(&r, e))
}

fn to_string(ex: &mut Execution, a: &mut [Arg]) -> CallResult<Value> {
    let v = arg_value(ex, a.first())?;
    let text = value_to_internal(ex, &v)?;
    ex.reserve((text.len() as u64).saturating_mul(2))?;
    Ok(Value::String(Str::from_vec(text.encode_utf16().collect())))
}

const BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];

fn to_file(ex: &mut Execution, a: &mut [Arg]) -> CallResult<Value> {
    let path = arg_str(ex, a.first())?.to_string_lossy();
    let v = arg_value(ex, a.get(1))?;
    let text = value_to_internal(ex, &v)?;
    let lfs = text.bytes().filter(|&b| b == b'\n').count();
    ex.reserve((text.len() + lfs + BOM.len()) as u64)?;
    let mut bytes = Vec::with_capacity(text.len() + lfs + BOM.len());
    bytes.extend_from_slice(&BOM);
    for b in text.bytes() {
        if b == b'\n' {
            bytes.push(b'\r');
        }
        bytes.push(b);
    }
    let f = crate::fs::fs(ex, &path)?;
    ex.charge(bytes.len() as u64 / 64)?;
    f.get()
        .write(&path, &bytes, false)
        .map_err(crate::fs::fs_error)?;
    Ok(Value::Undefined)
}

fn from_file(ex: &mut Execution, a: &mut [Arg]) -> CallResult<Value> {
    let path = arg_str(ex, a.first())?.to_string_lossy();
    let bytes = crate::text::read_file(ex, &path)?;
    let body = bytes.strip_prefix(&BOM).unwrap_or(&bytes);
    let Ok(text) = std::str::from_utf8(body) else {
        return Err(CallError::Host(format_host_error()));
    };
    ex.reserve((text.len() as u64).saturating_mul(2))?;
    let units: Vec<u16> = text.replace("\r\n", "\n").encode_utf16().collect();
    let mut r = InternalReader::new();
    read_text(ex, &units, &mut r).map_err(|e| read_error(&r, e))
}

pub(crate) fn register(reg: &mut Registry) {
    global(
        reg,
        "ЗначениеВСтрокуВнутр",
        "ValueToStringInternal",
        to_string,
    );
    global(
        reg,
        "ЗначениеИзСтрокиВнутр",
        "ValueFromStringInternal",
        from_string,
    );
    global(reg, "ЗначениеВФайл", "ValueToFile", to_file);
    global(reg, "ЗначениеИзФайла", "ValueFromFile", from_file);
}
