use super::array::Array;
use super::structure::{Fields, Structure};
use super::{KeyValue, Snapshot, is_name, reserve_values, snapshot, types, vec_with_capacity};
use crate::xdto::{XdtoCodec, XdtoCtx, XdtoData, core};
use bsl_lib::xdto::QName;
use bsl_prim::Str;
use bsl_vm::{Exception, Execution, ObjRef, Value};
use platform_catalog::TypeIdx;
use std::sync::Arc;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Array,
    Structure,
    Map,
}

struct Collection {
    kind: Kind,
    fixed: bool,
}

struct Pair;

pub(crate) fn xdto_codecs() -> Vec<(TypeIdx, Arc<dyn XdtoCodec>)> {
    let t = types();
    let c = |kind, fixed| Arc::new(Collection { kind, fixed }) as Arc<dyn XdtoCodec>;
    vec![
        (t.array, c(Kind::Array, false)),
        (t.fixed_array, c(Kind::Array, true)),
        (t.structure, c(Kind::Structure, false)),
        (t.fixed_structure, c(Kind::Structure, true)),
        (t.map, c(Kind::Map, false)),
        (t.fixed_map, c(Kind::Map, true)),
        (t.pair, Arc::new(Pair) as Arc<dyn XdtoCodec>),
    ]
}

fn snapshot_len(snap: &Snapshot) -> usize {
    match snap {
        Snapshot::Array(a) => a.len(),
        Snapshot::Structure(f) => f.len(),
        Snapshot::Map(m) => m.len(),
    }
}

impl Collection {
    fn name(&self) -> &'static str {
        match (self.kind, self.fixed) {
            (Kind::Array, false) => "Array",
            (Kind::Array, true) => "FixedArray",
            (Kind::Structure, false) => "Structure",
            (Kind::Structure, true) => "FixedStructure",
            (Kind::Map, false) => "Map",
            (Kind::Map, true) => "FixedMap",
        }
    }

    fn write_items(
        &self,
        ex: &mut Execution,
        s: &mut XdtoCtx,
        snap: Snapshot,
    ) -> Result<Vec<(String, Vec<XdtoData>)>, Exception> {
        let n = snapshot_len(&snap);
        s.account(
            ex,
            (n as u64).saturating_mul(std::mem::size_of::<XdtoData>() as u64),
        )?;
        let mut out: Vec<XdtoData> = Vec::with_capacity(n);
        let prop = match snap {
            Snapshot::Array(items) => {
                for v in &items {
                    out.push(s.to_data(ex, v)?);
                }
                "Value"
            }
            Snapshot::Structure(fields) => {
                let ty = core(&format!("{}#Property", self.name()));
                for (k, v) in &fields {
                    let key = XdtoData::Value {
                        projection: None,
                        ty: QName::new(crate::xdto::XS, "string"),
                        lexical: k.to_string_lossy(),
                        value: Value::String(k.clone()),
                    };
                    let value = s.to_data(ex, v)?;
                    out.push(XdtoData::Object {
                        ty: ty.clone(),
                        props: vec![("name".into(), vec![key]), ("Value".into(), vec![value])],
                    });
                }
                "Property"
            }
            Snapshot::Map(pairs) => {
                for (k, v) in &pairs {
                    out.push(pair_data(ex, s, k, v)?);
                }
                "pair"
            }
        };
        Ok(if out.is_empty() {
            Vec::new()
        } else {
            vec![(prop.to_string(), out)]
        })
    }
}

fn pair_data(
    ex: &mut Execution,
    s: &mut XdtoCtx,
    k: &Value,
    v: &Value,
) -> Result<XdtoData, Exception> {
    let key = s.to_data(ex, k)?;
    let value = s.to_data(ex, v)?;
    Ok(XdtoData::Object {
        ty: core("KeyAndValue"),
        props: vec![("Key".into(), vec![key]), ("Value".into(), vec![value])],
    })
}

fn items<'a>(props: &'a [(String, Vec<XdtoData>)], name: &str) -> &'a [XdtoData] {
    props
        .iter()
        .find(|(n, _)| n == name)
        .map_or(&[], |(_, v)| v.as_slice())
}

fn object_props(d: &XdtoData) -> &[(String, Vec<XdtoData>)] {
    match d {
        XdtoData::Object { props, .. } => props,
        _ => &[],
    }
}

impl XdtoCodec for Collection {
    fn to_xdto(
        &self,
        ex: &mut Execution,
        v: &ObjRef,
        s: &mut XdtoCtx,
    ) -> Result<XdtoData, Exception> {
        let snap = match (self.kind, snapshot(&Value::Object(v.clone()))) {
            (Kind::Array, Some(sn @ Snapshot::Array(_)))
            | (Kind::Structure, Some(sn @ Snapshot::Structure(_)))
            | (Kind::Map, Some(sn @ Snapshot::Map(_))) => sn,
            _ => return Err(s.mismatch(ex)),
        };
        let n = snapshot_len(&snap);
        s.hold(ex, n)?;
        let props = self.write_items(ex, s, snap);
        s.release(n);
        Ok(XdtoData::Object {
            ty: core(self.name()),
            props: props?,
        })
    }

    fn from_xdto(
        &self,
        ex: &mut Execution,
        d: &XdtoData,
        s: &mut XdtoCtx,
    ) -> Result<Value, Exception> {
        let props = object_props(d);
        match self.kind {
            Kind::Array => {
                let list = items(props, "Value");
                let mut out = vec_with_capacity(ex, list.len() as u64)?;
                for it in list {
                    out.push(s.from_data(ex, it)?);
                }
                Ok(Array::new_value(out, self.fixed))
            }
            Kind::Structure => {
                let list = items(props, "Property");
                reserve_values(ex, list.len() as u64)?;
                let mut fields = Fields::default();
                for p in list {
                    let pp = object_props(p);
                    let name = match items(pp, "name").first() {
                        Some(XdtoData::Value { lexical, .. }) => lexical.as_str(),
                        _ => continue,
                    };
                    let units: Vec<u16> = name.encode_utf16().collect();
                    if !is_name(&units) {
                        continue;
                    }
                    let value = match items(pp, "Value").first() {
                        Some(v) => s.from_data(ex, v)?,
                        None => Value::Undefined,
                    };
                    drop(fields.insert(Str::from(name), value));
                }
                Ok(Structure::new_value(fields, self.fixed))
            }
            Kind::Map => {
                let list = items(props, "pair");
                reserve_values(ex, (list.len() as u64).saturating_mul(2))?;
                let mut pairs = Vec::with_capacity(list.len());
                for p in list {
                    pairs.push(read_pair(ex, s, p)?);
                }
                if self.fixed {
                    return Ok(super::new_fixed_map(ex, pairs));
                }
                let m = super::new_map();
                for (k, v) in pairs {
                    if let Err(e) = super::map_put(ex, &m, k, v) {
                        return Err(s.exc(e));
                    }
                }
                Ok(m)
            }
        }
    }

    fn xdto_type(&self) -> QName {
        core(self.name())
    }
}

fn read_pair(
    ex: &mut Execution,
    s: &mut XdtoCtx,
    p: &XdtoData,
) -> Result<(Value, Value), Exception> {
    let pp = object_props(p);
    let mut one = |name: &str, ex: &mut Execution| match items(pp, name).first() {
        Some(d) => s.from_data(ex, d),
        None => Ok(Value::Undefined),
    };
    let k = one("Key", ex)?;
    let v = one("Value", ex)?;
    Ok((k, v))
}

impl XdtoCodec for Pair {
    fn to_xdto(
        &self,
        ex: &mut Execution,
        v: &ObjRef,
        s: &mut XdtoCtx,
    ) -> Result<XdtoData, Exception> {
        let Some(kv) = v.as_any().downcast_ref::<KeyValue>() else {
            return Err(s.mismatch(ex));
        };
        let [k, val] = kv.pair();
        pair_data(ex, s, &k, &val)
    }

    fn from_xdto(
        &self,
        ex: &mut Execution,
        d: &XdtoData,
        s: &mut XdtoCtx,
    ) -> Result<Value, Exception> {
        let (k, v) = read_pair(ex, s, d)?;
        Ok(KeyValue::new_value(k, v))
    }

    fn xdto_type(&self) -> QName {
        core("KeyAndValue")
    }
}
