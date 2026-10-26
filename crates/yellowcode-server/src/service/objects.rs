use super::{RequestData, ResponseData};
use crate::process::ProcessError;
use bsl_stdlib::{
    bind::{arg_i64, arg_str, arg_value},
    enums::as_enum_value,
};
use bsl_vm::{
    Arg, CallResult, Execution, HostError, HostObject, MemberId, Registry, TypeId, TypeIdx,
    TypeImpl, Value,
};
use std::{any::Any, cell::RefCell, rc::Rc, sync::Arc};

fn unsupported() -> HostError {
    HostError::text(
        "Не реализовано в реплике (HTTP-сервис)",
        "Not implemented in the replica (HTTP service)",
    )
}
fn invalid() -> HostError {
    HostError::text(
        "Недопустимое значение аргумента функции",
        "Invalid argument value",
    )
}
fn member(t: TypeIdx, m: MemberId, method: bool) -> Option<&'static str> {
    let info = platform_catalog::catalog().types().get(usize::from(t.0))?;
    let index = usize::try_from(m.0).ok()?;
    if method {
        info.methods
            .get(index.checked_sub(info.properties.len())?)
            .map(|p| p.name_en.as_str())
    } else {
        info.properties.get(index).map(|p| p.name_en.as_str())
    }
}
fn charset(headers: &[(String, String)]) -> Option<&str> {
    let (_, content_type) = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("Content-Type"))?;
    let (mut quoted, mut escaped) = (false, false);
    content_type
        .split(|c| {
            if escaped {
                escaped = false;
            } else if quoted && c == '\\' {
                escaped = true;
            } else if c == '"' {
                quoted = !quoted;
            }
            c == ';' && !quoted && !escaped
        })
        .skip(1)
        .filter_map(|p| p.trim().split_once('='))
        .find_map(|(k, v)| {
            if !k.trim().eq_ignore_ascii_case("charset") {
                return None;
            }
            Some(v.trim())
        })
}
fn pairs(ex: &mut Execution, values: &[(String, String)]) -> Result<Value, bsl_vm::Exception> {
    let bytes: usize = values
        .iter()
        .map(|(k, v)| k.len().saturating_add(v.len()))
        .sum();
    ex.reserve((bytes as u64).saturating_mul(2))?;
    Ok(bsl_stdlib::new_fixed_map(
        ex,
        values
            .iter()
            .map(|(k, v)| (Value::from(k.as_str()), Value::from(v.as_str())))
            .collect(),
    ))
}
struct Request {
    t: TypeIdx,
    method: String,
    base: String,
    relative: String,
    url: Value,
    query: Value,
    headers: Value,
    body: Vec<u8>,
    charset: Value,
}
impl HostObject for Request {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.t)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        Ok(match member(self.t, m, false) {
            Some("HTTPMethod") => Value::from(self.method.as_str()),
            Some("BaseURL") => Value::from(self.base.as_str()),
            Some("RelativeURL") => Value::from(self.relative.as_str()),
            Some("URLParameters") => self.url.clone(),
            Some("QueryOptions") => self.query.clone(),
            Some("Headers") => self.headers.clone(),
            _ => return Err(unsupported().into()),
        })
    }
    fn set(&self, _: &mut Execution, _: MemberId, _: Value) -> CallResult<()> {
        Err(unsupported().into())
    }
    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        match member(self.t, m, true) {
            Some("GetBodyAsString") => {
                let explicit = arg_value(ex, args.first())?;
                let selected = if matches!(explicit, Value::Undefined) {
                    &self.charset
                } else {
                    &explicit
                };
                let (encoding, _) = bsl_stdlib::encoding_from_value(ex, selected, 1)?;
                ex.reserve((self.body.len() as u64).saturating_mul(8))?;
                Ok(Value::from(bsl_vm::Str::from_vec(
                    bsl_lib::codec::bytes_to_string(&self.body, encoding),
                )))
            }
            Some("GetBodyAsBinaryData" | "GetBodyAsStream") => {
                ex.reserve(self.body.len() as u64)?;
                let binary = bsl_stdlib::values::new_binary_data(self.body.clone());
                if member(self.t, m, true) == Some("GetBodyAsStream") {
                    Ok(ex.call_method(&binary, "OpenStreamForRead", &mut [], true)?)
                } else {
                    Ok(binary)
                }
            }
            _ => Err(unsupported().into()),
        }
    }
}

pub fn request(
    ex: &mut Execution,
    data: &RequestData,
    base: &str,
    relative: &str,
    url: &[(String, String)],
) -> Result<Value, bsl_vm::Exception> {
    let t = platform_catalog::catalog()
        .find_type("HTTPServiceRequest")
        .ok_or_else(|| ex.error(invalid()))?;
    let url = pairs(ex, url)?;
    let query = pairs(ex, &data.query)?;
    let headers = pairs(ex, &data.headers)?;
    ex.reserve(data.body.len() as u64)?;
    let charset = if let Some(label) = charset(&data.headers) {
        ex.reserve((label.len() as u64).saturating_mul(6))?;
        if let Some(quoted) = label.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
            let mut normalized = String::with_capacity(quoted.len());
            let mut chars = quoted.chars();
            while let Some(c) = chars.next() {
                normalized.push(if c == '\\' {
                    chars.next().unwrap_or(c)
                } else {
                    c
                });
            }
            Value::from(normalized.as_str())
        } else {
            Value::from(label)
        }
    } else {
        Value::Undefined
    };
    Ok(Value::Object(Rc::new(Request {
        t,
        method: data.method.clone(),
        base: base.into(),
        relative: relative.into(),
        url,
        query,
        headers,
        body: data.body.clone(),
        charset,
    })))
}

struct Response {
    t: TypeIdx,
    status: RefCell<u16>,
    headers: Value,
    body: RefCell<Vec<u8>>,
}
impl HostObject for Response {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.t)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        match member(self.t, m, false) {
            Some("StatusCode") => Ok(Value::from(i64::from(*self.status.borrow()))),
            Some("Headers") => Ok(self.headers.clone()),
            Some("Reason") => Ok(Value::from("")),
            _ => Err(unsupported().into()),
        }
    }
    fn set(&self, ex: &mut Execution, m: MemberId, v: Value) -> CallResult<()> {
        match member(self.t, m, false) {
            Some("StatusCode") => {
                *self.status.borrow_mut() = status(ex, Some(&Arg::Val(v)))?;
                Ok(())
            }
            Some("Reason") => Ok(()),
            _ => Err(unsupported().into()),
        }
    }
    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        match member(self.t, m, true) {
            Some("SetBodyFromString") => {
                let encoding = arg_value(ex, args.get(1))?;
                let (encoding, _) = bsl_stdlib::encoding_from_value(ex, &encoding, 2)?;
                let text = arg_str(ex, args.first())?;
                let mode = arg_value(ex, args.get(2))?;
                let bom = match mode {
                    Value::Undefined => false,
                    _ => {
                        let mode = as_enum_value(&mode).ok_or_else(invalid)?;
                        if Some(mode.type_idx())
                            != platform_catalog::catalog().find_type("ByteOrderMarkUse")
                        {
                            return Err(invalid().into());
                        }
                        match mode.name_en() {
                            "Use" => true,
                            "Auto" | "DontUse" => false,
                            _ => return Err(invalid().into()),
                        }
                    }
                };
                ex.reserve(
                    (text.units().len() as u64)
                        .saturating_mul(16)
                        .saturating_add(8),
                )?;
                *self.body.borrow_mut() =
                    bsl_lib::codec::string_to_bytes(text.units(), encoding, bom);
            }
            Some("SetBodyFromBinaryData") => {
                let data = arg_value(ex, args.first())?;
                let bytes = bsl_stdlib::values::binary_data_bytes(&data).ok_or_else(invalid)?;
                ex.reserve(bytes.len() as u64)?;
                *self.body.borrow_mut() = bytes.to_vec();
            }
            _ => return Err(unsupported().into()),
        }
        Ok(Value::Undefined)
    }
}
fn status(ex: &mut Execution, arg: Option<&Arg>) -> CallResult<u16> {
    let n = arg_i64(ex, arg, 1)?;
    if !(200..=599).contains(&n) || matches!(n, 204 | 205 | 304) {
        return Err(invalid().into());
    }
    u16::try_from(n).map_err(|_| invalid().into())
}
pub fn register(registry: &mut Registry) {
    if let Some(t) = platform_catalog::catalog().find_type("HTTPServiceResponse") {
        registry.type_impl(
            t,
            TypeImpl {
                ctor: Some(Arc::new(move |ex, args| {
                    let status = status(ex, args.first())?;
                    let supplied = arg_value(ex, args.get(2))?;
                    let headers = bsl_stdlib::new_map();
                    if !matches!(supplied, Value::Undefined) {
                        let pairs = bsl_stdlib::map_pairs(&supplied).ok_or_else(invalid)?;
                        for (key, value) in pairs {
                            if !matches!((&key, &value), (Value::String(_), Value::String(_))) {
                                return Err(invalid().into());
                            }
                            bsl_stdlib::map_put(ex, &headers, key, value)?;
                        }
                    }
                    Ok(Value::Object(Rc::new(Response {
                        t,
                        status: RefCell::new(status),
                        headers,
                        body: RefCell::new(Vec::new()),
                    })))
                })),
            },
        );
    }
}
pub fn response(ex: &mut Execution, value: &Value) -> Result<ResponseData, ProcessError> {
    let response = value
        .as_object()
        .and_then(|v| v.as_any().downcast_ref::<Response>())
        .ok_or(ProcessError::Call)?;
    let headers = bsl_stdlib::map_pairs(&response.headers)
        .ok_or(ProcessError::Call)?
        .into_iter()
        .map(|(k, v)| match (k, v) {
            (Value::String(k), Value::String(v)) => Ok((k.to_string_lossy(), v.to_string_lossy())),
            _ => Err(ProcessError::Call),
        })
        .collect::<Result<Vec<_>, _>>()?;
    ex.reserve(response.body.borrow().len() as u64)
        .map_err(|_| ProcessError::Call)?;
    Ok(ResponseData {
        status: *response.status.borrow(),
        headers,
        body: response.body.borrow().clone(),
    })
}
