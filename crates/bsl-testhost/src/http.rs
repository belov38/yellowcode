use bsl_lib::codec::{self, Encoding};
use bsl_prim::Str;
use bsl_stdlib::bind::{arg_i64, arg_str, arg_value, is_given, to_number, type_mismatch};
use bsl_stdlib::enums::as_enum_value;
use bsl_stdlib::values::{binary_data_bytes, new_binary_data};
use bsl_stdlib::{encoding_from_value, map_pairs, map_put, new_fixed_map, new_map};
use bsl_vm::{
    Arg, CallResult, Execution, HostError, HostObject, MemberId, Registry, TypeId, TypeIdx,
    TypeImpl, Value,
};
use platform_catalog::catalog;
use std::any::Any;
use std::cell::{OnceCell, RefCell};
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

const MISSING: u16 = u16::MAX;

struct Types {
    request: TypeIdx,
    response: TypeIdx,
}

fn copy_bytes(ex: &mut Execution, b: &[u8]) -> CallResult<Vec<u8>> {
    let n = b.len() as u64;
    ex.charge(n / 64)?;
    ex.reserve(n)?;
    Ok(b.to_vec())
}

fn types() -> &'static Types {
    static T: OnceLock<Types> = OnceLock::new();
    T.get_or_init(|| {
        let t = |id: &str| {
            let i = catalog().types().iter().position(|t| t.id == id);
            TypeIdx(i.and_then(|i| u16::try_from(i).ok()).unwrap_or(MISSING))
        };
        Types {
            request: t("HTTPServiceRequest"),
            response: t("HTTPServiceResponse"),
        }
    })
}

pub(crate) fn http_types_registered(rt: &bsl_vm::Runtime) -> bool {
    types().request.0 != MISSING
        && types().response.0 != MISSING
        && rt.has_type_impl(types().request)
        && rt.has_type_impl(types().response)
}

pub fn register_http(reg: &mut Registry) {
    if types().request.0 == MISSING || types().response.0 == MISSING {
        debug_assert!(false, "HTTP service types are missing from the catalog");
        return;
    }
    reg.type_impl(
        types().response,
        TypeImpl {
            ctor: Some(Arc::new(response_ctor)),
        },
    );
    reg.type_impl(types().request, TypeImpl { ctor: None });
}

pub fn new_request(method: &str, headers: &[(String, String)], body: Vec<u8>) -> Value {
    Value::Object(Rc::new(Request {
        method: method.to_owned(),
        headers: headers.to_vec(),
        fixed: OnceCell::new(),
        body,
    }))
}

pub fn response_parts(v: &Value) -> Option<(u16, Vec<u8>)> {
    let r = v.as_object()?.as_any().downcast_ref::<Response>()?;
    let s = r.state.borrow();
    Some((u16::try_from(s.status).unwrap_or(u16::MAX), s.body.clone()))
}

fn member(t: TypeIdx, m: MemberId) -> (Option<&'static str>, Option<&'static str>) {
    let Some(info) = catalog().types().get(usize::from(t.0)) else {
        return (None, None);
    };
    let Ok(k) = usize::try_from(m.0) else {
        return (None, None);
    };
    match k.checked_sub(info.properties.len()) {
        None => (info.properties.get(k).map(|p| p.name_en.as_str()), None),
        Some(j) => (None, info.methods.get(j).map(|x| x.name_en.as_str())),
    }
}

fn not_implemented(t: TypeIdx, m: MemberId) -> HostError {
    let info = catalog().types().get(usize::from(t.0));
    let k = usize::try_from(m.0).unwrap_or(usize::MAX);
    let name = info.and_then(|i| match k.checked_sub(i.properties.len()) {
        None => i.properties.get(k).map(|p| p.name_ru.clone()),
        Some(j) => i.methods.get(j).map(|x| x.name_ru.clone()),
    });
    HostError::not_implemented(
        info.map_or("", |i| i.name_ru.as_str()),
        &name.unwrap_or_else(|| m.0.to_string()),
    )
}

fn str_value(s: &str) -> Value {
    Value::from(s)
}

fn body_string(ex: &mut Execution, body: &[u8], enc: Encoding) -> CallResult<Value> {
    ex.charge(body.len() as u64 / 64)?;
    ex.reserve((body.len() as u64).saturating_mul(2))?;
    Ok(Value::String(Str::from_vec(codec::bytes_to_string(
        body, enc,
    ))))
}

struct Request {
    method: String,
    headers: Vec<(String, String)>,
    fixed: OnceCell<Value>,
    body: Vec<u8>,
}

impl HostObject for Request {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(types().request)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        match member(types().request, m).0 {
            Some("HTTPMethod") => Ok(str_value(&self.method)),
            Some("Headers") => {
                if let Some(v) = self.fixed.get() {
                    return Ok(v.clone());
                }
                let pairs = self
                    .headers
                    .iter()
                    .map(|(k, v)| (str_value(k), str_value(v)))
                    .collect();
                let v = new_fixed_map(ex, pairs);
                Ok(self.fixed.get_or_init(|| v).clone())
            }
            _ => Err(not_implemented(types().request, m).into()),
        }
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        Err(not_implemented(types().request, m).into())
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        match member(types().request, m).1 {
            Some("GetBodyAsString") => {
                let v = arg_value(ex, args.first())?;
                let (enc, _) = encoding_from_value(ex, &v, 1)?;
                body_string(ex, &self.body, enc)
            }
            Some("GetBodyAsBinaryData") => Ok(new_binary_data(copy_bytes(ex, &self.body)?)),
            _ => Err(not_implemented(types().request, m).into()),
        }
    }
}

struct State {
    status: i64,
    reason: Value,
    body: Vec<u8>,
}

struct Response {
    state: RefCell<State>,
    headers: Value,
}

fn response_ctor(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    let status = arg_i64(ex, args.first(), 1)?;
    let reason = arg_value(ex, args.get(1))?;
    let headers = new_map();
    if is_given(args.get(2)) {
        let src = arg_value(ex, args.get(2))?;
        if !matches!(src, Value::Undefined) {
            let pairs = map_pairs(&src).ok_or_else(|| type_mismatch(3))?;
            for (k, v) in pairs {
                map_put(ex, &headers, k, v)?;
            }
        }
    }
    Ok(Value::Object(Rc::new(Response {
        state: RefCell::new(State {
            status,
            reason,
            body: Vec::new(),
        }),
        headers,
    })))
}

fn bom_mode(ex: &mut Execution, a: Option<&Arg>, auto: bool) -> CallResult<bool> {
    let v = arg_value(ex, a)?;
    if matches!(v, Value::Undefined) {
        return Ok(auto);
    }
    let bom_type = catalog().find_type("ИспользованиеByteOrderMark");
    let name = as_enum_value(&v)
        .filter(|e| Some(e.type_idx()) == bom_type)
        .map(|e| e.name_ru());
    match name {
        Some("Авто") => Ok(auto),
        Some("Использовать") => Ok(true),
        Some("НеИспользовать") => Ok(false),
        _ => Err(type_mismatch(3).into()),
    }
}

impl HostObject for Response {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(types().response)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        match member(types().response, m).0 {
            Some("Headers") => Ok(self.headers.clone()),
            Some("StatusCode") => Ok(Value::from(self.state.borrow().status)),
            Some("Reason") => Ok(self.state.borrow().reason.clone()),
            _ => Err(not_implemented(types().response, m).into()),
        }
    }

    fn set(&self, ex: &mut Execution, m: MemberId, v: Value) -> CallResult<()> {
        match member(types().response, m).0 {
            Some("StatusCode") => {
                let n = to_number(ex, &v)?;
                self.state.borrow_mut().status = n.to_i64().ok_or_else(|| type_mismatch(1))?;
                Ok(())
            }
            Some("Reason") => {
                self.state.borrow_mut().reason = v;
                Ok(())
            }
            _ => Err(not_implemented(types().response, m).into()),
        }
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        match member(types().response, m).1 {
            Some("SetBodyFromString") => {
                let s = arg_str(ex, args.first())?;
                let enc_arg = arg_value(ex, args.get(1))?;
                let (enc, writes_bom) = encoding_from_value(ex, &enc_arg, 2)?;
                let bom = bom_mode(ex, args.get(2), writes_bom)?;
                ex.charge(s.len() as u64 / 64)?;
                ex.reserve((s.len() as u64).saturating_mul(4))?;
                self.state.borrow_mut().body = codec::string_to_bytes(s.units(), enc, bom);
                Ok(Value::Undefined)
            }
            Some("SetBodyFromBinaryData") => {
                let v = arg_value(ex, args.first())?;
                let b = binary_data_bytes(&v).ok_or_else(|| type_mismatch(1))?;
                let b = copy_bytes(ex, b)?;
                self.state.borrow_mut().body = b;
                Ok(Value::Undefined)
            }
            Some("GetBodyAsString") => {
                let v = arg_value(ex, args.first())?;
                let (enc, _) = encoding_from_value(ex, &v, 1)?;
                let len = self.state.borrow().body.len() as u64;
                ex.reserve(len)?;
                let body = self.state.borrow().body.clone();
                body_string(ex, &body, enc)
            }
            Some("GetBodyAsBinaryData") => {
                let b = copy_bytes(ex, &self.state.borrow().body)?;
                Ok(new_binary_data(b))
            }
            _ => Err(not_implemented(types().response, m).into()),
        }
    }
}

pub fn register_no_transactions(reg: &mut Registry) {
    reg.global_fn(
        "ТранзакцияАктивна",
        "TransactionActive",
        std::sync::Arc::new(|_, _| Ok(bsl_vm::Value::Bool(false))),
    );
}
