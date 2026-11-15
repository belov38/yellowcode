use crate::bind::{msg, parse_number};
use crate::collections::{ctor_not_implemented, not_implemented, property_ru, type_name};
use crate::enums::{enum_value, type_by_id};
use bsl_prim::{Number, Str};
use bsl_vm::{
    Arg, CallResult, Execution, HostError, HostObject, MemberId, Registry, TypeId, TypeImpl, Value,
};
use platform_catalog::{TypeIdx, catalog};
use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

const TYPE_RU: &str = "ИнтернетПочтовыйПрофиль";
const TIMEOUT_ALIAS: MemberId = MemberId(10_000);
const TIMEOUT_ALIAS_RU: &str = "времяожидания";

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Str {
        conv: bool,
    },
    Port {
        conv: bool,
    },
    Bool {
        conv: bool,
    },
    ImapSsl,
    Timeout,
    Enum {
        ty: &'static str,
        default: Option<&'static str>,
    },
    ReadOnlyStr,
    ReadOnlyBool,
}

const SPECS: &[(&str, Kind)] = &[
    ("POP3BeforeSMTP", Kind::ReadOnlyBool),
    ("IMAPServerAddress", Kind::Str { conv: false }),
    ("POP3ServerAddress", Kind::Str { conv: false }),
    ("SMTPServerAddress", Kind::Str { conv: true }),
    (
        "POP3Authentication",
        Kind::Enum {
            ty: "POP3AuthenticationMode",
            default: None,
        },
    ),
    (
        "SMTPAuthentication",
        Kind::Enum {
            ty: "SMTPAuthenticationMode",
            default: None,
        },
    ),
    (
        "TokenAuthentication",
        Kind::Enum {
            ty: "UseInternetMailTokenAuthentication",
            default: Some("Auto"),
        },
    ),
    ("IMAPUseSSL", Kind::ImapSsl),
    ("POP3UseSSL", Kind::Bool { conv: false }),
    ("SMTPUseSSL", Kind::Bool { conv: true }),
    ("Password", Kind::Str { conv: false }),
    ("IMAPPassword", Kind::Str { conv: false }),
    ("SMTPPassword", Kind::Str { conv: false }),
    ("User", Kind::Str { conv: false }),
    ("IMAPUser", Kind::Str { conv: false }),
    ("SMTPUser", Kind::Str { conv: false }),
    ("IMAPPort", Kind::Port { conv: false }),
    ("POP3Port", Kind::Port { conv: false }),
    ("SMTPPort", Kind::Port { conv: true }),
    ("Timeout", Kind::Timeout),
    ("AccessToken", Kind::ReadOnlyStr),
    ("IMAPSecureAuthenticationOnly", Kind::Bool { conv: false }),
    ("POP3SecureAuthenticationOnly", Kind::Bool { conv: false }),
    ("SMTPSecureAuthenticationOnly", Kind::Bool { conv: false }),
];

struct Table {
    ty: TypeIdx,
    kinds: Vec<Option<Kind>>,
    timeout: usize,
    imap_port: usize,
}

fn table() -> Option<&'static Table> {
    static T: OnceLock<Option<Table>> = OnceLock::new();
    T.get_or_init(|| {
        let ty = catalog().find_type("InternetMailProfile")?;
        let props = &catalog().type_info(ty).properties;
        let kinds: Vec<Option<Kind>> = props
            .iter()
            .map(|p| SPECS.iter().find(|(n, _)| *n == p.name_en).map(|(_, k)| *k))
            .collect();
        let pos = |en: &str| props.iter().position(|p| p.name_en == en);
        Some(Table {
            ty,
            kinds,
            timeout: pos("Timeout")?,
            imap_port: pos("IMAPPort")?,
        })
    })
    .as_ref()
}

fn default_of(kind: Kind) -> Value {
    match kind {
        Kind::Str { .. } | Kind::ReadOnlyStr => Value::from(""),
        Kind::Bool { .. } | Kind::ImapSsl | Kind::ReadOnlyBool => Value::Bool(false),
        Kind::Port { .. } => Value::Number(Number::from_i64(0)),
        Kind::Timeout => Value::Number(Number::from_i64(30)),
        Kind::Enum { ty, default } => default
            .and_then(|d| enum_value(type_by_id(ty)?, d))
            .unwrap_or(Value::Undefined),
    }
}

pub(crate) struct Profile {
    values: RefCell<Vec<Value>>,
}

pub(crate) fn filled_check_refusal(v: &Value) -> Option<HostError> {
    v.as_object()?.as_any().downcast_ref::<Profile>()?;
    let ru = "Проверка мутабельных значений на заполненность не поддерживается";
    Some(HostError::text(ru, ru).raw())
}

fn mismatch() -> HostError {
    HostError::key(msg::PROPERTY_TYPE_MISMATCH).raw()
}

fn refused(t: &Table, m: MemberId, what: &str) -> HostError {
    HostError::not_implemented(TYPE_RU, &format!("{}: {what}", property_ru(t.ty, m)))
}

fn in_range(n: &Number) -> bool {
    let f = n.to_f64();
    (-1.0..=70_000.0).contains(&f)
}

fn presented_as_observed(v: &Value) -> bool {
    match v {
        Value::Number(n) => in_range(n),
        Value::Bool(b) => *b,
        Value::Date(d) => d.to_xml_string() == "2020-01-01T00:00:00",
        _ => false,
    }
}

enum StrClass {
    Digits,
    Letters,
    Other,
}

fn class_of(s: &Str) -> StrClass {
    let text = s.to_string_lossy();
    if !text.is_empty() && text.chars().all(|c| c.is_ascii_digit()) {
        StrClass::Digits
    } else if !text.is_empty() && text.chars().all(|c| c.is_ascii_alphabetic()) {
        StrClass::Letters
    } else {
        StrClass::Other
    }
}

fn number_of(ex: &Execution, v: &Value) -> Result<Number, Option<HostError>> {
    match v {
        Value::Number(n) if in_range(n) => Ok(n.clone()),
        Value::Bool(true) => Ok(Number::from_i64(1)),
        Value::String(s) => match class_of(s) {
            StrClass::Digits => parse_number(ex, s.units()).filter(in_range).ok_or(None),
            StrClass::Letters => Err(Some(mismatch())),
            StrClass::Other => Err(None),
        },
        Value::Undefined | Value::Null | Value::Date(_) => Err(Some(mismatch())),
        _ => Err(None),
    }
}

impl Profile {
    fn index(&self, t: &Table, m: MemberId) -> Option<(usize, Kind)> {
        if m == TIMEOUT_ALIAS {
            return Some((t.timeout, Kind::Timeout));
        }
        let i = usize::try_from(m.0).ok()?;
        Some((i, (*t.kinds.get(i)?)?))
    }

    fn store(&self, ex: &mut Execution, t: &Table, m: MemberId, v: Value) -> CallResult<()> {
        let Some((i, kind)) = self.index(t, m) else {
            return Err(not_implemented(t.ty, m).into());
        };
        let m = if m == TIMEOUT_ALIAS {
            MemberId(t.timeout as u32)
        } else {
            m
        };
        let other = |e: HostError| -> CallResult<()> { Err(e.into()) };
        let unobserved = || refused(t, m, "значение, которое не наблюдалось");
        let number = |ex: &Execution, v: &Value| -> Result<Number, HostError> {
            number_of(ex, v).map_err(|e| e.unwrap_or_else(unobserved))
        };
        let value = match (kind, v) {
            (Kind::Str { .. }, v @ Value::String(_)) => v,
            (Kind::Str { conv: true }, Value::Undefined | Value::Null) => Value::from(""),
            (Kind::Str { conv: true }, v) if presented_as_observed(&v) => {
                Value::String(ex.present(&v)?)
            }
            (Kind::Str { conv: true }, _) => return other(unobserved()),
            (Kind::Port { .. }, Value::Number(n)) if !in_range(&n) => return other(unobserved()),
            (Kind::Port { .. }, v @ Value::Number(_)) => v,
            (Kind::Port { conv: true }, v) => Value::Number(number(ex, &v)?),
            (Kind::Bool { .. }, v @ Value::Bool(_)) => v,
            (Kind::Bool { conv: true }, Value::Number(n)) if in_range(&n) => {
                Value::Bool(!n.is_zero())
            }
            (Kind::Bool { conv: true }, Value::String(s))
                if !matches!(class_of(&s), StrClass::Other) =>
            {
                return other(mismatch());
            }
            (Kind::Bool { conv: true }, Value::Undefined | Value::Null | Value::Date(_)) => {
                return other(mismatch());
            }
            (Kind::Bool { conv: true }, _) => return other(unobserved()),
            (Kind::ImapSsl, Value::Bool(b)) => {
                let port = if b { 993 } else { 143 };
                let mut values = self.values.borrow_mut();
                if let Some(slot) = values.get_mut(t.imap_port) {
                    *slot = Value::Number(Number::from_i64(port));
                }
                Value::Bool(b)
            }
            (Kind::Timeout, v) => {
                let n = number(ex, &v)?.trunc();
                Value::Number(if n.is_negative() {
                    Number::from_i64(0)
                } else {
                    n
                })
            }
            (Kind::Enum { .. } | Kind::ReadOnlyStr | Kind::ReadOnlyBool, _) => {
                return other(refused(t, m, "запись"));
            }
            (_, _) => return other(refused(t, m, "присваивание значения другого типа")),
        };
        if let Some(slot) = self.values.borrow_mut().get_mut(i) {
            *slot = value;
        }
        Ok(())
    }
}

impl HostObject for Profile {
    fn retained_memory(
        &self,
        visit: &mut dyn bsl_vm::RetainedMemory,
    ) -> Result<bool, bsl_vm::Exception> {
        let values = self.values.borrow();
        visit.bytes((values.capacity() * std::mem::size_of::<Value>()) as u64)?;
        for v in values.iter() {
            if let Value::String(s) = v {
                visit.bytes(s.units().len() as u64 * 2)?;
            }
        }
        Ok(true)
    }

    fn type_id(&self) -> TypeId {
        match table() {
            Some(t) => TypeId::of_catalog(t.ty),
            None => TypeId::of_catalog(TypeIdx(0)),
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let Some(t) = table() else {
            return Err(HostError::not_implemented(TYPE_RU, "Получение").into());
        };
        let Some((i, kind)) = self.index(t, m) else {
            return Err(not_implemented(t.ty, m).into());
        };
        let v = self.values.borrow().get(i).cloned();
        match (kind, v) {
            (Kind::Enum { default: None, .. }, _) | (_, None) => {
                Err(refused(t, m, "чтение").into())
            }
            (_, Some(v)) => Ok(v),
        }
    }

    fn set(&self, ex: &mut Execution, m: MemberId, v: Value) -> CallResult<()> {
        let Some(t) = table() else {
            return Err(HostError::not_implemented(TYPE_RU, "Запись").into());
        };
        self.store(ex, t, m, v)
    }

    fn call(&self, _ex: &mut Execution, m: MemberId, _args: &mut [Arg]) -> CallResult<Value> {
        match table() {
            Some(t) => Err(not_implemented(t.ty, m).into()),
            None => Err(HostError::not_implemented(TYPE_RU, "Вызов").into()),
        }
    }

    fn find_member(&self, name: &str) -> Option<MemberId> {
        (name.to_lowercase() == TIMEOUT_ALIAS_RU).then_some(TIMEOUT_ALIAS)
    }

    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<Str>> {
        Ok(table().map(|t| type_name(ex, t.ty)))
    }
}

fn new_profile() -> Option<Value> {
    let t = table()?;
    let values = t
        .kinds
        .iter()
        .map(|k| k.map_or(Value::Undefined, default_of))
        .collect();
    Some(Value::Object(Rc::new(Profile {
        values: RefCell::new(values),
    })))
}

pub(crate) fn register(reg: &mut Registry) {
    let Some(t) = table() else {
        return;
    };
    let ty = t.ty;
    reg.type_impl(
        ty,
        TypeImpl {
            ctor: Some(Arc::new(move |_, args| {
                if !args.is_empty() {
                    return Err(ctor_not_implemented(ty).into());
                }
                new_profile().ok_or_else(|| ctor_not_implemented(ty).into())
            })),
        },
    );
}
