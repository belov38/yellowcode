use super::array::Array;
use super::structure::{Fields, Structure};
use super::{KeyValue, Snapshot, is_name, reserve_values, snapshot, types, vec_with_capacity};
use crate::serial::codec::{InternalReader, InternalWriter, ValueCodec};
use crate::serial::internal::text::decode_str;
use brace::Node;
use bsl_prim::Str;
use bsl_vm::{CallError, Exception, Execution, ObjRef, Value};
use platform_catalog::TypeIdx;
use std::sync::Arc;

#[derive(Clone, Copy)]
enum Kind {
    Array,
    Structure,
    Map,
}

struct Collection {
    kind: Kind,
    fixed: bool,
}

pub(crate) fn codecs() -> Vec<(TypeIdx, Arc<dyn ValueCodec>)> {
    let t = types();
    let c = |kind, fixed| Arc::new(Collection { kind, fixed }) as Arc<dyn ValueCodec>;
    vec![
        (t.array, c(Kind::Array, false)),
        (t.fixed_array, c(Kind::Array, true)),
        (t.structure, c(Kind::Structure, false)),
        (t.fixed_structure, c(Kind::Structure, true)),
        (t.map, c(Kind::Map, false)),
        (t.fixed_map, c(Kind::Map, true)),
        (t.pair, Arc::new(Pair) as Arc<dyn ValueCodec>),
    ]
}

fn call_exception(ex: &mut Execution, e: CallError) -> Exception {
    match e {
        CallError::Host(h) => ex.error(h),
        CallError::Exc(x) => x,
    }
}

fn key_node(k: &Str) -> Node {
    Node::List(vec![
        Node::str("S"),
        crate::serial::internal::text::encode_str(k.units()),
    ])
}

fn write_all(
    ex: &mut Execution,
    w: &mut InternalWriter,
    snap: Snapshot,
) -> Result<Vec<Node>, Exception> {
    let n = match &snap {
        Snapshot::Array(a) => a.len(),
        Snapshot::Structure(f) => f.len(),
        Snapshot::Map(m) => m.len(),
    };
    w.hold(ex, n)?;
    let r = write_items(ex, w, snap);
    w.release(n);
    r
}

fn write_items(
    ex: &mut Execution,
    w: &mut InternalWriter,
    snap: Snapshot,
) -> Result<Vec<Node>, Exception> {
    let mut out = Vec::new();
    match snap {
        Snapshot::Array(items) => {
            out.push(Node::int(items.len() as i64));
            for v in &items {
                out.push(w.write_value(ex, v)?);
            }
        }
        Snapshot::Structure(fields) => {
            out.push(Node::int(fields.len() as i64));
            for (k, v) in &fields {
                let value = w.write_value(ex, v)?;
                out.push(Node::List(vec![key_node(k), value]));
            }
        }
        Snapshot::Map(pairs) => {
            out.push(Node::int(pairs.len() as i64));
            for (k, v) in &pairs {
                let key = w.write_value(ex, k)?;
                let value = w.write_value(ex, v)?;
                out.push(Node::List(vec![key, value]));
            }
        }
    }
    Ok(out)
}

impl ValueCodec for Collection {
    fn internal_write(
        &self,
        ex: &mut Execution,
        v: &ObjRef,
        w: &mut InternalWriter,
    ) -> Result<Option<Vec<Node>>, Exception> {
        let snap = match (self.kind, snapshot(&Value::Object(v.clone()))) {
            (Kind::Array, Some(s @ Snapshot::Array(_)))
            | (Kind::Structure, Some(s @ Snapshot::Structure(_)))
            | (Kind::Map, Some(s @ Snapshot::Map(_))) => s,
            _ => return Ok(None),
        };
        Ok(Some(vec![Node::List(write_all(ex, w, snap)?)]))
    }

    fn internal_read(
        &self,
        ex: &mut Execution,
        items: &[Node],
        r: &mut InternalReader,
    ) -> Result<Value, Exception> {
        let Some(data) = items.first().and_then(Node::as_list) else {
            return Err(r.format_error(ex));
        };
        let elems = data.get(1..).unwrap_or_default();
        match self.kind {
            Kind::Array => {
                let mut out = vec_with_capacity(ex, elems.len() as u64)?;
                for e in elems {
                    out.push(r.read_value(ex, e)?);
                }
                Ok(Array::new_value(out, self.fixed))
            }
            Kind::Structure => {
                reserve_values(ex, elems.len() as u64)?;
                let mut fields = Fields::default();
                for e in elems {
                    let Some(pair) = e.as_list() else {
                        return Err(r.format_error(ex));
                    };
                    let Some(key) = key_of(pair.first()) else {
                        continue;
                    };
                    let Some(value) = pair.get(1) else {
                        return Err(r.format_error(ex));
                    };
                    let value = r.read_value(ex, value)?;
                    drop(fields.insert(Str::from_vec(key), value));
                }
                Ok(Structure::new_value(fields, self.fixed))
            }
            Kind::Map => {
                reserve_values(ex, (elems.len() as u64).saturating_mul(2))?;
                let mut pairs = Vec::new();
                for e in elems {
                    let pair = e.as_list().unwrap_or_default();
                    let (Some(k), Some(v)) = (pair.first(), pair.get(1)) else {
                        return Err(r.format_error(ex));
                    };
                    let k = r.read_value(ex, k)?;
                    let v = r.read_value(ex, v)?;
                    pairs.push((k, v));
                }
                if self.fixed {
                    return Ok(super::new_fixed_map(ex, pairs));
                }
                let m = super::new_map();
                for (k, v) in pairs {
                    super::map_put(ex, &m, k, v).map_err(|e| call_exception(ex, e))?;
                }
                Ok(m)
            }
        }
    }
}

fn key_of(n: Option<&Node>) -> Option<Vec<u16>> {
    let l = n?.as_list()?;
    if l.first()?.as_str()? != "S" {
        return None;
    }
    let units = decode_str(l.get(1)?)?;
    is_name(&units).then_some(units)
}

struct Pair;

impl ValueCodec for Pair {
    fn internal_write(
        &self,
        ex: &mut Execution,
        v: &ObjRef,
        w: &mut InternalWriter,
    ) -> Result<Option<Vec<Node>>, Exception> {
        let Some(kv) = v.as_any().downcast_ref::<KeyValue>() else {
            return Ok(None);
        };
        let [k, val] = kv.pair();
        let k = w.write_value(ex, &k)?;
        let val = w.write_value(ex, &val)?;
        Ok(Some(vec![Node::List(vec![k, val])]))
    }

    fn internal_read(
        &self,
        ex: &mut Execution,
        items: &[Node],
        r: &mut InternalReader,
    ) -> Result<Value, Exception> {
        let pair = items.first().and_then(Node::as_list).unwrap_or_default();
        let (Some(k), Some(v)) = (pair.first(), pair.get(1)) else {
            return Err(r.format_error(ex));
        };
        let k = r.read_value(ex, k)?;
        let v = r.read_value(ex, v)?;
        Ok(KeyValue::new_value(k, v))
    }
}
