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
    pub fn from_object(object: &dyn crate::HostObject) -> Option<Self> {
        Some(Self::Object(Arc::new(CollectionCopy(
            crate::session_graph::Graph::snapshot_object(object)?,
        ))))
    }

    pub fn from_value_in(ex: &mut Execution, value: &Value) -> Result<Option<Self>, Exception> {
        ex.charge(1)?;
        let retained = ex.measure_retained(0, |visit| visit.value(value))?;
        if let Value::Object(object) = value {
            if !object.session_transfer_supported() {
                return Ok(None);
            }
            let limit = crate::reuse::memory_limit(ex).map(|limit| limit.saturating_sub(retained));
            let Some(work) = crate::session_graph::retire::acyclic(object.as_ref(), limit) else {
                return Ok(None);
            };
            ex.charge(work)?;
        }
        let graph = crate::session_graph::Graph::export(ex, std::slice::from_ref(value), retained)?;
        if let Some(graph) = graph {
            ex.reserve(
                retained
                    .saturating_add(graph.bytes)
                    .saturating_add(crate::session_shared_bytes::<CollectionCopy>()),
            )?;
            Ok(Some(Self::Object(Arc::new(CollectionCopy(graph)))))
        } else {
            Ok(None)
        }
    }
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
            SharedValue::Number(n) => {
                ex.charge(n.size_hint_limbs() / 64)?;
                let retained = (n.backing_capacity_limbs() as u64).saturating_mul(4);
                Value::Number(
                    n.try_clone_with(|limbs| {
                        ex.reserve(retained.saturating_add((limbs as u64).saturating_mul(4)))
                    })
                    .map_err(|_| ex.out_of_memory())?,
                )
            }
            SharedValue::String(s) => Value::String(Str::from_units(s)),
            SharedValue::Date(d) => Value::Date(*d),
            SharedValue::Type(t) => Value::Type(*t),
            SharedValue::Object(o) => return o.to_value(ex),
        })
    }
}

struct CollectionCopy(crate::session_graph::Graph);
impl SharedObject for CollectionCopy {
    fn to_value(&self, ex: &mut Execution) -> Result<Value, Exception> {
        self.0.materialize_copy(ex)
    }
}
