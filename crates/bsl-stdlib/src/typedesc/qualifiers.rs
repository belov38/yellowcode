use super::idx;
use crate::bind::{arg_i64, arg_value, type_mismatch, value_mismatch};
use crate::collections::{not_implemented, property_name};
use bsl_vm::{
    Arg, CallResult, Exception, Execution, HostObject, MemberId, Registry, SharedObject,
    SharedValue, TypeId, TypeImpl, Value,
};
use std::{any::Any, rc::Rc, sync::Arc};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AllowedSign {
    #[default]
    Any,
    Nonnegative,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AllowedLength {
    #[default]
    Variable,
    Fixed,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DateParts {
    Date,
    Time,
    #[default]
    DateTime,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NumberQualifiers {
    pub digits: u32,
    pub fraction: u32,
    pub sign: AllowedSign,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StringQualifiers {
    pub length: u32,
    pub allowed: AllowedLength,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DateQualifiers {
    pub parts: DateParts,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BinaryQualifiers {
    pub length: u32,
    pub allowed: AllowedLength,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Qualifier {
    Number(NumberQualifiers),
    String(StringQualifiers),
    Date(DateQualifiers),
    Binary(BinaryQualifiers),
}
impl Qualifier {
    pub(super) fn id(self) -> &'static str {
        match self {
            Self::Number(_) => "NumberQualifiers",
            Self::String(_) => "StringQualifiers",
            Self::Date(_) => "DateQualifiers",
            Self::Binary(_) => "BinaryDataQualifiers",
        }
    }
    pub(super) fn value(self) -> Value {
        Value::Object(Rc::new(self))
    }
    pub(super) fn of(v: &Value) -> Option<&Self> {
        v.as_object()?.as_any().downcast_ref()
    }
}
fn enumeration(ty: &str, name: &str) -> Value {
    crate::enums::enum_value(idx(ty), name).unwrap_or_default()
}
impl HostObject for Qualifier {
    fn session_edges(&self, _: &mut dyn bsl_vm::SessionEdges) -> bool {
        true
    }
    fn session_transfer_supported(&self) -> bool {
        true
    }
    fn retained_memory(&self, _: &mut dyn bsl_vm::RetainedMemory) -> Result<bool, Exception> {
        Ok(true)
    }
    fn session_transfer(
        &self,
        out: &mut dyn bsl_vm::SessionExport,
    ) -> Result<Option<Box<dyn bsl_vm::SessionObject>>, Exception> {
        out.reserve(std::mem::size_of::<crate::session::Leaf<Self>>() as u64)?;
        Ok(Some(Box::new(crate::session::Leaf {
            value: *self,
            restore_bytes: crate::collections::retained_rc_size::<Self>(),
        })))
    }
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(idx(self.id()))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        let p = property_name(idx(self.id()), m);
        Ok(match (self, p) {
            (Self::Number(n), Some("Digits")) => Value::from(i64::from(n.digits)),
            (Self::Number(n), Some("FractionDigits")) => Value::from(i64::from(n.fraction)),
            (Self::Number(n), Some("AllowedSign")) => enumeration(
                "AllowedSign",
                if n.sign == AllowedSign::Any {
                    "Любой"
                } else {
                    "Неотрицательный"
                },
            ),
            (Self::String(s), Some("Length")) => Value::from(i64::from(s.length)),
            (Self::Binary(b), Some("Length")) => Value::from(i64::from(b.length)),
            (
                Self::String(StringQualifiers { allowed, .. })
                | Self::Binary(BinaryQualifiers { allowed, .. }),
                Some("AllowedLength"),
            ) => enumeration(
                "AllowedLength",
                if *allowed == AllowedLength::Fixed {
                    "Фиксированная"
                } else {
                    "Переменная"
                },
            ),
            (Self::Date(d), Some("DateFractions")) => enumeration(
                "DateFractions",
                match d.parts {
                    DateParts::Date => "Дата",
                    DateParts::Time => "Время",
                    DateParts::DateTime => "ДатаВремя",
                },
            ),
            _ => return Err(not_implemented(idx(self.id()), m).into()),
        })
    }
    fn set(&self, _: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        Err(not_implemented(idx(self.id()), m).into())
    }
    fn call(&self, _: &mut Execution, m: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        Err(not_implemented(idx(self.id()), m).into())
    }
    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        Some(
            other
                .as_any()
                .downcast_ref::<Self>()
                .is_some_and(|o| o == self),
        )
    }
    fn boost_hash(&self) -> Option<i32> {
        Some(match self {
            Self::Number(n) => n
                .digits
                .wrapping_mul(37)
                .wrapping_add(n.fraction)
                .wrapping_mul(2)
                .wrapping_add(n.sign as u32) as i32,
            Self::String(s) => s.length.wrapping_mul(2).wrapping_add(s.allowed as u32) as i32,
            Self::Date(d) => d.parts as i32,
            Self::Binary(b) => b.length.wrapping_mul(2).wrapping_add(b.allowed as u32) as i32,
        })
    }
    fn to_shared(&self) -> Option<SharedValue> {
        Some(SharedValue::Object(Arc::new(*self)))
    }
}
impl SharedObject for Qualifier {
    fn to_value(&self, _: &mut Execution) -> Result<Value, Exception> {
        Ok(self.value())
    }
}
fn integer(ex: &mut Execution, args: &[Arg], pos: usize, max: u32) -> CallResult<u32> {
    if matches!(arg_value(ex, args.get(pos))?, Value::Undefined) {
        return Ok(0);
    }
    let n = arg_i64(ex, args.get(pos), pos + 1)?;
    u32::try_from(n)
        .ok()
        .filter(|n| *n <= max)
        .ok_or_else(|| value_mismatch(pos + 1).into())
}
fn enum_name(
    ex: &mut Execution,
    args: &[Arg],
    pos: usize,
    ty: &str,
) -> CallResult<Option<&'static str>> {
    let v = arg_value(ex, args.get(pos))?;
    if matches!(v, Value::Undefined) {
        return Ok(None);
    }
    let e = crate::enums::as_enum_value(&v)
        .filter(|e| e.type_idx() == idx(ty))
        .ok_or_else(|| type_mismatch(pos + 1))?;
    Ok(Some(e.name_en()))
}
pub(super) fn register(reg: &mut Registry) {
    reg.type_impl(
        idx("NumberQualifiers"),
        TypeImpl {
            ctor: Some(Arc::new(|ex, args| {
                let digits = integer(ex, args, 0, 32)?;
                let fraction = integer(ex, args, 1, 32)?;
                if fraction > digits {
                    return Err(value_mismatch(2).into());
                }
                let sign = match enum_name(ex, args, 2, "AllowedSign")? {
                    None | Some("Any") => AllowedSign::Any,
                    Some(_) => AllowedSign::Nonnegative,
                };
                Ok(Qualifier::Number(NumberQualifiers {
                    digits,
                    fraction,
                    sign,
                })
                .value())
            })),
        },
    );
    for (id, binary) in [("StringQualifiers", false), ("BinaryDataQualifiers", true)] {
        reg.type_impl(
            idx(id),
            TypeImpl {
                ctor: Some(Arc::new(move |ex, args| {
                    let length = integer(ex, args, 0, u32::MAX)?;
                    let allowed = match enum_name(ex, args, 1, "AllowedLength")? {
                        Some("Fixed") => AllowedLength::Fixed,
                        _ => AllowedLength::Variable,
                    };
                    Ok(if binary {
                        Qualifier::Binary(BinaryQualifiers { length, allowed })
                    } else {
                        Qualifier::String(StringQualifiers { length, allowed })
                    }
                    .value())
                })),
            },
        );
    }
    reg.type_impl(
        idx("DateQualifiers"),
        TypeImpl {
            ctor: Some(Arc::new(|ex, args| {
                let parts = match enum_name(ex, args, 0, "DateFractions")? {
                    Some("Date") => DateParts::Date,
                    Some("Time") => DateParts::Time,
                    _ => DateParts::DateTime,
                };
                Ok(Qualifier::Date(DateQualifiers { parts }).value())
            })),
        },
    );
}
