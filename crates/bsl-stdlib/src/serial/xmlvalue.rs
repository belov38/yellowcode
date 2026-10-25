use super::storage::{is_storage_type, value_storage_bytes, value_storage_from_bytes};
use crate::values::{
    binary_data_bytes, new_binary_data, new_uuid, parse_uuid, uuid_bytes, uuid_string,
};
use bsl_prim::Str;
use bsl_vm::{AppXdto, CallResult, Exception, Execution, HostError, HostObject, TypeId, Value};
use std::sync::Arc;

const LINE: usize = 64;

fn wrapped(plain: &str) -> String {
    if plain.len() <= LINE {
        return plain.to_string();
    }
    let mut out = String::with_capacity(plain.len() + plain.len() / LINE * 2);
    let mut rest = plain;
    while !rest.is_empty() {
        let (line, tail) = rest.split_at(LINE.min(rest.len()));
        out.push_str(line);
        if line.len() == LINE {
            out.push_str("\r\n");
        }
        rest = tail;
    }
    out
}

pub fn to_xml_string(ex: &mut Execution, v: &Value) -> Result<String, Exception> {
    let r = crate::convert::xml_text(ex, v);
    r.map_err(|e| ex.call_error(e))
}

pub fn from_xml_string(ex: &mut Execution, t: TypeId, text: &str) -> Result<Value, Exception> {
    let text = Str::from(text);
    let r = crate::convert::xml_from_text(ex, t, &text);
    r.map_err(|e| ex.call_error(e))
}

pub(crate) fn strip_plus(units: &[u16]) -> &[u16] {
    let ws = |u: &u16| matches!(u, 0x20 | 0x09 | 0x0D | 0x0A);
    let start = units.iter().position(|u| !ws(u)).unwrap_or(units.len());
    match units.get(start..) {
        Some([0x2B, d, ..]) if (0x30..=0x39).contains(d) => &units[start + 1..],
        _ => units,
    }
}

pub(crate) fn ascii_lower(units: &[u16]) -> std::borrow::Cow<'_, [u16]> {
    if units.len() > 64 {
        return std::borrow::Cow::Borrowed(units);
    }
    std::borrow::Cow::Owned(
        units
            .iter()
            .map(|&u| {
                if (0x41..=0x5A).contains(&u) {
                    u + 0x20
                } else {
                    u
                }
            })
            .collect(),
    )
}

pub(crate) fn date_prefix(units: &[u16]) -> &[u16] {
    let ws = |u: &u16| matches!(u, 0x20 | 0x09 | 0x0D | 0x0A);
    let start = units.iter().position(|u| !ws(u)).unwrap_or(units.len());
    match units.get(start..start + 11) {
        Some(p) if p[10] != 0x54 => &units[start..start + 10],
        _ => units,
    }
}

pub(crate) fn wrapped_base64(bytes: &[u8]) -> String {
    wrapped(&bsl_lib::b64::base64_plain(bytes))
}

fn base64_text(ex: &mut Execution, bytes: &[u8]) -> Result<String, Exception> {
    ex.reserve((bytes.len() as u64).div_ceil(3).saturating_mul(9))?;
    ex.charge(bytes.len() as u64 / 64)?;
    Ok(wrapped(&bsl_lib::b64::base64_plain(bytes)))
}

pub(crate) fn xml_string(ex: &mut Execution, v: &Value) -> Result<Option<String>, Exception> {
    if let Some(bytes) = value_storage_bytes(v) {
        return base64_text(ex, &bytes).map(Some);
    }
    if let Some(bytes) = binary_data_bytes(v) {
        return base64_text(ex, bytes).map(Some);
    }
    if let Some(u) = uuid_bytes(v) {
        return Ok(Some(uuid_string(&u)));
    }
    let Some(o) = v.as_object() else {
        return Ok(None);
    };
    if ex
        .runtime()
        .types()
        .app_type(HostObject::type_id(&**o))
        .is_none()
    {
        return Ok(None);
    }
    let app = ex.runtime().app().clone();
    match app.serializer() {
        Some(s) => Ok(match s.xdto_write(ex, o)? {
            Some(AppXdto::Text(t)) => Some(t),
            _ => None,
        }),
        None => Ok(None),
    }
}

pub(crate) fn xml_value(ex: &mut Execution, t: TypeId, text: &[u16]) -> Option<CallResult<Value>> {
    if let Some(idx) = [
        "AccumulationRecordType",
        "AccountType",
        "AccountingRecordType",
    ]
    .iter()
    .filter_map(|id| crate::enums::type_by_id(id))
    .find(|idx| TypeId::of_catalog(*idx) == t)
    {
        let name = String::from_utf16_lossy(text);
        return Some(crate::enums::enum_value(idx, &name).ok_or_else(|| conversion().into()));
    }
    if is_storage_type(t) {
        return Some(from_base64(ex, text));
    }
    if t == TypeId::of_catalog(crate::collections::types().uuid) {
        return Some(match parse_uuid(text) {
            Some(b) => Ok(new_uuid(b)),
            None => Err(conversion().into()),
        });
    }
    if t == TypeId::of_catalog(crate::collections::types().binary) {
        return Some(binary_from_base64(ex, text));
    }
    if ex.runtime().types().app_type(t).is_some() {
        return Some(applied(ex, t, text));
    }
    None
}

fn conversion() -> HostError {
    HostError::key(crate::bind::msg::XML_CONVERSION)
}

fn from_base64(ex: &mut Execution, text: &[u16]) -> CallResult<Value> {
    ex.reserve((text.len() as u64).saturating_mul(2))?;
    ex.charge(text.len() as u64 / 64)?;
    match bsl_lib::b64::base64_value(text) {
        Some(b) => Ok(value_storage_from_bytes(Arc::from(b))),
        None => Err(conversion().into()),
    }
}

fn binary_from_base64(ex: &mut Execution, text: &[u16]) -> CallResult<Value> {
    ex.reserve((text.len() as u64).saturating_mul(2))?;
    ex.charge(text.len() as u64 / 64)?;
    Ok(new_binary_data(
        bsl_lib::b64::base64_value(text).unwrap_or_default(),
    ))
}

fn applied(ex: &mut Execution, t: TypeId, text: &[u16]) -> CallResult<Value> {
    let app = ex.runtime().app().clone();
    let Some(s) = app.serializer() else {
        return Err(conversion().into());
    };
    let text = String::from_utf16_lossy(text);
    match s.xdto_read(ex, t, AppXdto::Text(text)) {
        Ok(v) => Ok(v),
        Err(e) => Err(rewrap_conversion(ex, e)),
    }
}

fn rewrap_conversion(ex: &Execution, e: Exception) -> bsl_vm::CallError {
    if let Exception::Catchable(info) = &e {
        let own = platform_catalog::messages::text_key(crate::bind::msg::XML_CONVERSION, ex.lang());
        if info.cause.is_none() && own.is_some_and(|t| info.description.to_string_lossy() == t) {
            return conversion().into();
        }
    }
    e.into()
}
