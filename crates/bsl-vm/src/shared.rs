use crate::error::Exception;
use crate::runtime::Execution;
use crate::types::TypeId;
use crate::value::Value;
use bsl_prim::{BslDate, Number, Str};
use std::sync::Arc;

#[derive(Clone)]
pub enum SharedValue {
    Undefined,
    Null,
    Bool(bool),
    Number(Number),
    String(Arc<[u16]>),
    Date(BslDate),
    Type(TypeId),
    Object(Arc<dyn SharedObject>),
}

pub trait SharedObject: Send + Sync {
    fn to_value(&self, ex: &mut Execution) -> Result<Value, Exception>;
}

impl SharedValue {
    pub fn from_value(v: &Value) -> Option<SharedValue> {
        Some(match v {
            Value::Undefined => SharedValue::Undefined,
            Value::Null => SharedValue::Null,
            Value::Bool(b) => SharedValue::Bool(*b),
            Value::Number(n) => SharedValue::Number(n.clone()),
            Value::String(s) => SharedValue::String(Arc::from(s.units())),
            Value::Date(d) => SharedValue::Date(*d),
            Value::Type(t) => SharedValue::Type(*t),
            Value::Object(o) => return o.to_shared(),
        })
    }

    pub fn to_value(&self, ex: &mut Execution) -> Result<Value, Exception> {
        Ok(match self {
            SharedValue::Undefined => Value::Undefined,
            SharedValue::Null => Value::Null,
            SharedValue::Bool(b) => Value::Bool(*b),
            SharedValue::Number(n) => Value::Number(n.clone()),
            SharedValue::String(s) => Value::String(Str::from_units(s)),
            SharedValue::Date(d) => Value::Date(*d),
            SharedValue::Type(t) => Value::Type(*t),
            SharedValue::Object(o) => return o.to_value(ex),
        })
    }
}
