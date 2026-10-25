use super::data::{binary_arg, encoding_arg};
use super::{reserve_units, str_value};
use crate::binary::{binary_buffer_bytes, new_ordered};
use crate::bind::{
    arg_bool, arg_i64, arg_str, arg_value, global, is_given, type_mismatch, value_mismatch,
};
use crate::collections::{array_items, new_array, reserve_values};
use crate::globals::enum_arg;
use crate::values::{binary_data_bytes, new_binary_data};
use bsl_lib::codec::{self, Encoding};
use bsl_lib::{b64, hex};
use bsl_vm::{Arg, CallResult, Execution, Registry, Value};

fn buffer_arg(ex: &mut Execution, a: Option<&Arg>, param: usize) -> CallResult<Vec<u8>> {
    let v = arg_value(ex, a)?;
    match binary_buffer_bytes(&v) {
        Some(b) => {
            ex.charge(b.len() as u64 / 64)?;
            Ok(b)
        }
        None => Err(type_mismatch(param).into()),
    }
}

fn buffer(ex: &mut Execution, bytes: Vec<u8>) -> CallResult<Value> {
    Ok(new_ordered(ex, bytes, false)?)
}

pub(super) fn register(reg: &mut Registry) {
    global(
        reg,
        "ПолучитьБуферДвоичныхДанныхИзСтроки",
        "GetBinaryDataBufferFromString",
        |ex, a| {
            let s = arg_str(ex, a.first())?;
            let enc = encoding_arg(ex, a.get(1), 2)?.unwrap_or(Encoding::Utf8);
            let bom = is_given(a.get(2)) && arg_bool(ex, a.get(2))?;
            reserve_units(ex, s.len(), 2)?;
            buffer(ex, codec::string_to_bytes(s.units(), enc, bom))
        },
    );
    global(
        reg,
        "ПолучитьБуферДвоичныхДанныхИзHexСтроки",
        "GetBinaryDataBufferFromHexString",
        |ex, a| {
            let b = super::data::hex_arg(ex, a.first())?;
            buffer(ex, b)
        },
    );
    global(
        reg,
        "ПолучитьБуферДвоичныхДанныхИзBase64Строки",
        "GetBinaryDataBufferFromBase64String",
        |ex, a| {
            let s = arg_str(ex, a.first())?;
            match b64::base64_value(s.units()) {
                Some(b) => buffer(ex, b),
                None => Ok(Value::Undefined),
            }
        },
    );
    global(
        reg,
        "ПолучитьБуферДвоичныхДанныхИзДвоичныхДанных",
        "GetBinaryDataBufferFromBinaryData",
        |ex, a| {
            let b = binary_arg(ex, a.first(), 1)?;
            buffer(ex, b)
        },
    );
    global(
        reg,
        "ПолучитьДвоичныеДанныеИзБуфераДвоичныхДанных",
        "GetBinaryDataFromBinaryDataBuffer",
        |ex, a| Ok(new_binary_data(buffer_arg(ex, a.first(), 1)?)),
    );
    global(
        reg,
        "ПолучитьHexСтрокуИзБуфераДвоичныхДанных",
        "GetHexStringFromBinaryDataBuffer",
        |ex, a| {
            let b = buffer_arg(ex, a.first(), 1)?;
            reserve_units(ex, b.len(), 2)?;
            Ok(Value::from(hex::hex_string(&b).as_str()))
        },
    );
    global(
        reg,
        "ПолучитьBase64СтрокуИзБуфераДвоичныхДанных",
        "GetBase64StringFromBinaryDataBuffer",
        |ex, a| {
            let b = buffer_arg(ex, a.first(), 1)?;
            reserve_units(ex, b.len(), 3)?;
            Ok(Value::from(
                crate::serial::xmlvalue::wrapped_base64(&b).as_str(),
            ))
        },
    );
    global(
        reg,
        "ПолучитьСтрокуИзБуфераДвоичныхДанных",
        "GetStringFromBinaryDataBuffer",
        |ex, a| {
            let b = buffer_arg(ex, a.first(), 1)?;
            let enc = encoding_arg(ex, a.get(1), 2)?.unwrap_or(Encoding::Utf8);
            reserve_units(ex, b.len(), 1)?;
            Ok(str_value(codec::bytes_to_string(&b, enc)))
        },
    );
    global(
        reg,
        "СоединитьБуферыДвоичныхДанных",
        "ConcatBinaryDataBuffers",
        |ex, a| {
            let v = arg_value(ex, a.first())?;
            let Some(items) = array_items(&v) else {
                return Err(type_mismatch(1).into());
            };
            let sep = if is_given(a.get(1)) {
                let s = arg_value(ex, a.get(1))?;
                match binary_buffer_bytes(&s) {
                    Some(b) => b,
                    None if matches!(s, Value::Undefined) => Vec::new(),
                    None => return Err(type_mismatch(2).into()),
                }
            } else {
                Vec::new()
            };
            let big = matches!(
                enum_arg(ex, a.get(2), "ПорядокБайтов", 3)?,
                Some("BigEndian")
            );
            let mut parts = Vec::with_capacity(items.len());
            let mut total = (items.len().saturating_sub(1) as u64).saturating_mul(sep.len() as u64);
            for it in &items {
                let Some(b) = binary_buffer_bytes(it) else {
                    return Err(type_mismatch(1).into());
                };
                total = total.saturating_add(b.len() as u64);
                parts.push(b);
            }
            crate::binary::guard_bytes(ex, total)?;
            let mut out = Vec::new();
            for (i, p) in parts.iter().enumerate() {
                if i > 0 {
                    out.extend_from_slice(&sep);
                }
                out.extend_from_slice(p);
            }
            Ok(new_ordered(ex, out, big)?)
        },
    );
    global(
        reg,
        "РазделитьДвоичныеДанные",
        "SplitBinaryData",
        |ex, a| {
            let v = arg_value(ex, a.first())?;
            let Some(data) = binary_data_bytes(&v) else {
                return Err(type_mismatch(1).into());
            };
            let size = arg_i64(ex, a.get(1), 2)?;
            let Some(size) = usize::try_from(size).ok().filter(|s| *s > 0) else {
                return Err(value_mismatch(2).into());
            };
            let count = data.len().div_ceil(size);
            reserve_values(ex, count as u64)?;
            ex.charge(count as u64 + data.len() as u64 / 64)?;
            let parts = data
                .chunks(size)
                .map(|c| new_binary_data(c.to_vec()))
                .collect();
            Ok(new_array(parts))
        },
    );
}
