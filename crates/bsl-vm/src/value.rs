use crate::object::HostObject;
use crate::types::TypeId;
use bsl_prim::{BslDate, Number, Str};
use std::fmt;
use std::rc::Rc;

pub type ObjRef = Rc<dyn HostObject>;

#[derive(Clone, Default)]
pub enum Value {
    #[default]
    Undefined,
    Null,
    Bool(bool),
    Number(Number),
    String(Str),
    Date(BslDate),
    Type(TypeId),
    Object(ObjRef),
}

impl Value {
    pub fn type_id(&self) -> TypeId {
        match self {
            Value::Undefined => TypeId::UNDEFINED,
            Value::Null => TypeId::NULL,
            Value::Bool(_) => TypeId::BOOL,
            Value::Number(_) => TypeId::NUMBER,
            Value::String(_) => TypeId::STRING,
            Value::Date(_) => TypeId::DATE,
            Value::Type(_) => TypeId::TYPE,
            Value::Object(o) => HostObject::type_id(&**o),
        }
    }

    pub fn as_object(&self) -> Option<&ObjRef> {
        match self {
            Value::Object(o) => Some(o),
            _ => None,
        }
    }
}

impl From<bool> for Value {
    fn from(v: bool) -> Value {
        Value::Bool(v)
    }
}

impl From<i64> for Value {
    fn from(v: i64) -> Value {
        Value::Number(Number::from_i64(v))
    }
}

impl From<&str> for Value {
    fn from(v: &str) -> Value {
        Value::String(Str::from(v))
    }
}

impl From<Number> for Value {
    fn from(v: Number) -> Value {
        Value::Number(v)
    }
}

impl From<Str> for Value {
    fn from(v: Str) -> Value {
        Value::String(v)
    }
}

impl From<BslDate> for Value {
    fn from(v: BslDate) -> Value {
        Value::Date(v)
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Undefined => f.write_str("Undefined"),
            Value::Null => f.write_str("Null"),
            Value::Bool(b) => write!(f, "Bool({b})"),
            Value::Number(n) => write!(f, "Number({n:?})"),
            Value::String(s) => write!(f, "String({s:?})"),
            Value::Date(d) => write!(f, "Date({d:?})"),
            Value::Type(t) => write!(f, "Type({})", t.0),
            Value::Object(o) => write!(f, "Object(type {})", HostObject::type_id(&**o).0),
        }
    }
}
