use super::array::Array;
use super::map::{JsonMapLookup, Map, Table};
use super::structure::{Fields, Structure};
use super::{KeyValue, Snapshot, is_name, reserve_values, snapshot, types, vec_with_capacity};
use crate::xdto::{XdtoCodec, XdtoCtx, XdtoData, core};
use bsl_lib::xdto::QName;
use bsl_prim::Str;
use bsl_vm::{Exception, Execution, ObjRef, Value};
use platform_catalog::TypeIdx;
use std::sync::Arc;
use std::{cell::RefCell, rc::Rc};

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
                    s.enter(ex, false)?;
                    let value = s.to_data(ex, v);
                    s.leave();
                    let value = value?;
                    out.push(XdtoData::Object {
                        ty: ty.clone(),
                        props: vec![("name".into(), vec![key]), ("Value".into(), vec![value])],
                    });
                }
                "Property"
            }
            Snapshot::Map(pairs) => {
                for (k, v) in &pairs {
                    s.enter(ex, false)?;
                    let pair = pair_data(ex, s, k, v);
                    s.leave();
                    out.push(pair?);
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
                if s.is_json_read() {
                    return read_json_array(ex, s, list, self.fixed);
                }
                let mut out = vec_with_capacity(ex, list.len() as u64)?;
                for it in list {
                    out.push(s.from_data(ex, it)?);
                }
                Ok(Array::new_value(out, self.fixed))
            }
            Kind::Structure => {
                let list = items(props, "Property");
                if s.is_json_read() {
                    return read_json_structure(ex, s, list, self.fixed);
                }
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
                if s.is_json_read() {
                    return read_json_map(ex, s, list, self.fixed);
                }
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

fn read_json_array(
    ex: &mut Execution,
    s: &mut XdtoCtx,
    list: &[XdtoData],
    fixed: bool,
) -> Result<Value, Exception> {
    let count = u64::try_from(list.len()).map_err(|_| ex.out_of_memory())?;
    ex.charge(count)?;
    let (handle, _) = std::alloc::Layout::new::<[usize; 2]>()
        .extend(std::alloc::Layout::new::<RefCell<Vec<Value>>>())
        .map_err(|_| ex.out_of_memory())?;
    let bytes = count
        .checked_mul(std::mem::size_of::<Value>() as u64)
        .and_then(|n| n.checked_add(handle.pad_to_align().size() as u64))
        .ok_or_else(|| ex.out_of_memory())?;
    s.account(ex, bytes)?;
    let mut values = Vec::new();
    values
        .try_reserve_exact(list.len())
        .map_err(|_| ex.out_of_memory())?;
    let values = Rc::new(RefCell::new(values));
    let root = values.clone();
    let oom = ex.out_of_memory();
    s.push_json_root(ex, move |visit| {
        let values = root.try_borrow().map_err(|_| oom.clone())?;
        visit.work(values.len() as u64)?;
        let backing = (values.capacity() as u64)
            .checked_mul(std::mem::size_of::<Value>() as u64)
            .ok_or_else(|| oom.clone())?;
        visit.bytes(backing)?;
        for value in values.iter() {
            visit.value(value)?;
        }
        Ok(())
    })?;
    let result = (|| {
        for item in list {
            let value = s.from_data(ex, item)?;
            values
                .try_borrow_mut()
                .map_err(|_| ex.out_of_memory())?
                .push(value);
        }
        s.refresh_json_roots(ex)?;
        s.account(
            ex,
            (std::mem::size_of::<Array>() + 2 * std::mem::size_of::<usize>()) as u64,
        )?;
        let mut values = values.try_borrow_mut().map_err(|_| ex.out_of_memory())?;
        Ok(Array::new_value(std::mem::take(&mut *values), fixed))
    })();
    s.pop_json_root();
    result
}

fn push_json_pending(
    ex: &mut Execution,
    s: &mut XdtoCtx,
) -> Result<Rc<RefCell<Option<Value>>>, Exception> {
    let (layout, _) = std::alloc::Layout::new::<[usize; 2]>()
        .extend(std::alloc::Layout::new::<RefCell<Option<Value>>>())
        .map_err(|_| ex.out_of_memory())?;
    s.account(ex, layout.pad_to_align().size() as u64)?;
    let pending = Rc::new(RefCell::new(None));
    let root = pending.clone();
    let oom = ex.out_of_memory();
    s.push_json_root(ex, move |visit| {
        let value = root.try_borrow().map_err(|_| oom.clone())?;
        if let Some(value) = value.as_ref() {
            visit.value(value)?;
        }
        Ok(())
    })?;
    Ok(pending)
}

fn read_json_structure(
    ex: &mut Execution,
    s: &mut XdtoCtx,
    list: &[XdtoData],
    fixed: bool,
) -> Result<Value, Exception> {
    let count = u64::try_from(list.len()).map_err(|_| ex.out_of_memory())?;
    ex.charge(count)?;
    let (layout, _) = std::alloc::Layout::new::<[usize; 2]>()
        .extend(std::alloc::Layout::new::<RefCell<Fields>>())
        .map_err(|_| ex.out_of_memory())?;
    s.account(ex, layout.pad_to_align().size() as u64)?;
    let fields = Rc::new(RefCell::new(Fields::default()));
    let root = fields.clone();
    let oom = ex.out_of_memory();
    s.push_json_root(ex, move |visit| {
        let fields = root.try_borrow().map_err(|_| oom.clone())?;
        fields.visit_json_retained(visit, &oom)
    })?;
    let pending = match push_json_pending(ex, s) {
        Ok(pending) => pending,
        Err(e) => {
            s.pop_json_root();
            return Err(e);
        }
    };
    let result = (|| {
        for p in list {
            let pp = object_props(p);
            let name = match items(pp, "name").first() {
                Some(XdtoData::Value { lexical, .. }) => lexical.as_str(),
                _ => continue,
            };
            let size = u64::try_from(name.len()).map_err(|_| ex.out_of_memory())?;
            ex.charge(size)?;
            let bytes = size.checked_mul(4).ok_or_else(|| ex.out_of_memory())?;
            s.account(ex, bytes)?;
            let mut units = Vec::new();
            units
                .try_reserve_exact(name.len())
                .map_err(|_| ex.out_of_memory())?;
            units.extend(name.encode_utf16());
            if !is_name(&units) {
                continue;
            }
            let value = match items(pp, "Value").first() {
                Some(v) => s.from_data(ex, v)?,
                None => Value::Undefined,
            };
            *pending.try_borrow_mut().map_err(|_| ex.out_of_memory())? = Some(value);
            let work = fields
                .try_borrow()
                .map_err(|_| ex.out_of_memory())?
                .json_scan_work(name.len())
                .ok_or_else(|| ex.out_of_memory())?;
            ex.charge(work)?;
            let (new_entry, bytes) = fields
                .try_borrow()
                .map_err(|_| ex.out_of_memory())?
                .json_insert_estimate(name)
                .ok_or_else(|| ex.out_of_memory())?;
            s.account(ex, bytes)?;
            fields
                .try_borrow_mut()
                .map_err(|_| ex.out_of_memory())?
                .prepare_json_insert(new_entry)
                .map_err(|_| ex.out_of_memory())?;
            s.refresh_json_roots(ex)?;
            let key = Str::from(name);
            let value = pending
                .try_borrow_mut()
                .map_err(|_| ex.out_of_memory())?
                .take()
                .ok_or_else(|| ex.out_of_memory())?;
            let replaced = fields
                .try_borrow_mut()
                .map_err(|_| ex.out_of_memory())?
                .insert(key, value);
            drop(replaced);
        }
        s.refresh_json_roots(ex)?;
        s.account(
            ex,
            (std::mem::size_of::<Structure>() + 2 * std::mem::size_of::<usize>()) as u64,
        )?;
        let mut fields = fields.try_borrow_mut().map_err(|_| ex.out_of_memory())?;
        Ok(Structure::new_value(std::mem::take(&mut *fields), fixed))
    })();
    s.pop_json_root();
    s.pop_json_root();
    result
}

fn read_pair(
    ex: &mut Execution,
    s: &mut XdtoCtx,
    p: &XdtoData,
) -> Result<(Value, Value), Exception> {
    if s.is_json_read() {
        return read_json_pair(ex, s, p);
    }
    let pp = object_props(p);
    let mut one = |name: &str, ex: &mut Execution| match items(pp, name).first() {
        Some(d) => s.from_data(ex, d),
        None => Ok(Value::Undefined),
    };
    let k = one("Key", ex)?;
    let v = one("Value", ex)?;
    Ok((k, v))
}

fn read_json_pair(
    ex: &mut Execution,
    s: &mut XdtoCtx,
    p: &XdtoData,
) -> Result<(Value, Value), Exception> {
    let pp = object_props(p);
    let pending = push_json_pending(ex, s)?;
    let result = (|| {
        let key = match items(pp, "Key").first() {
            Some(d) => s.from_data(ex, d)?,
            None => Value::Undefined,
        };
        *pending.try_borrow_mut().map_err(|_| ex.out_of_memory())? = Some(key);
        let value = match items(pp, "Value").first() {
            Some(d) => s.from_data(ex, d)?,
            None => Value::Undefined,
        };
        let key = pending
            .try_borrow_mut()
            .map_err(|_| ex.out_of_memory())?
            .take()
            .ok_or_else(|| ex.out_of_memory())?;
        Ok((key, value))
    })();
    s.pop_json_root();
    result
}

fn read_json_map(
    ex: &mut Execution,
    s: &mut XdtoCtx,
    list: &[XdtoData],
    fixed: bool,
) -> Result<Value, Exception> {
    let count = u64::try_from(list.len()).map_err(|_| ex.out_of_memory())?;
    ex.charge(count)?;
    let (layout, _) = std::alloc::Layout::new::<[usize; 2]>()
        .extend(std::alloc::Layout::new::<RefCell<Vec<(Value, Value)>>>())
        .map_err(|_| ex.out_of_memory())?;
    let bytes = count
        .checked_mul(std::mem::size_of::<(Value, Value)>() as u64)
        .and_then(|n| n.checked_add(layout.pad_to_align().size() as u64))
        .ok_or_else(|| ex.out_of_memory())?;
    s.account(ex, bytes)?;
    let mut pairs = Vec::new();
    pairs
        .try_reserve_exact(list.len())
        .map_err(|_| ex.out_of_memory())?;
    let pairs = Rc::new(RefCell::new(pairs));
    let root = pairs.clone();
    let oom = ex.out_of_memory();
    s.push_json_root(ex, move |visit| {
        let pairs = root.try_borrow().map_err(|_| oom.clone())?;
        visit.work(pairs.len() as u64)?;
        visit.bytes(
            (pairs.capacity() as u64)
                .checked_mul(std::mem::size_of::<(Value, Value)>() as u64)
                .ok_or_else(|| oom.clone())?,
        )?;
        for (key, value) in pairs.iter() {
            visit.value(key)?;
            visit.value(value)?;
        }
        Ok(())
    })?;
    let staged = (|| {
        for item in list {
            let pair = read_json_pair(ex, s, item)?;
            pairs
                .try_borrow_mut()
                .map_err(|_| ex.out_of_memory())?
                .push(pair);
        }
        s.refresh_json_roots(ex)?;
        Ok(std::mem::take(
            &mut *pairs.try_borrow_mut().map_err(|_| ex.out_of_memory())?,
        ))
    })();
    s.pop_json_root();
    let staged = staged?;
    let staged_backing = u64::try_from(staged.capacity())
        .ok()
        .and_then(|n| n.checked_mul(std::mem::size_of::<(Value, Value)>() as u64))
        .ok_or_else(|| ex.out_of_memory())?;
    let mut remaining = staged.into_iter();
    let oom = ex.out_of_memory();
    let empty_bytes = Table::json_empty_bytes().ok_or_else(|| ex.out_of_memory())?;
    s.json_admit_extra(ex, empty_bytes, |visit| {
        visit.bytes(staged_backing)?;
        for (key, value) in remaining.as_slice() {
            visit.value(key)?;
            visit.value(value)?;
        }
        Ok(())
    })?;
    let mut table = Table::try_json_new().ok_or_else(|| ex.out_of_memory())?;
    s.json_admit_extra(ex, 0, |visit| {
        visit.bytes(staged_backing)?;
        table.visit_json_retained(visit, &oom)?;
        for (key, value) in remaining.as_slice() {
            visit.value(key)?;
            visit.value(value)?;
        }
        Ok(())
    })?;
    while let Some((key, value)) = remaining.next() {
        if !fixed
            && ex.env().compat().flags().map_ignores_undefined_key
            && matches!(key, Value::Undefined)
        {
            continue;
        }
        s.json_admit_extra(ex, staged_backing, |visit| {
            table.visit_json_retained(visit, &oom)?;
            for (key, value) in remaining.as_slice() {
                visit.value(key)?;
                visit.value(value)?;
            }
            visit.value(&key)?;
            visit.value(&value)
        })?;
        table.json_charge_lookup(ex, &key)?;
        let lookup = table.json_lookup(ex, key);
        let (plan, node_growth, numbers_growth) =
            table.json_plan(&lookup).ok_or_else(|| ex.out_of_memory())?;
        let mut scratch = bsl_prim::boostmap::InsertScratch::new(plan);
        for _ in 0..scratch.stage_count() {
            let live_scratch = scratch.backing_bytes().ok_or_else(|| ex.out_of_memory())?;
            let growth = scratch
                .stage_bound(plan)
                .ok_or_else(|| ex.out_of_memory())?;
            admit_json_map_stage(
                ex,
                s,
                &table,
                &lookup,
                &value,
                remaining.as_slice(),
                staged_backing,
                live_scratch,
                growth,
            )?;
            scratch
                .allocate_next(plan)
                .ok_or_else(|| ex.out_of_memory())?;
            let actual = scratch.backing_bytes().ok_or_else(|| ex.out_of_memory())?;
            admit_json_map_stage(
                ex,
                s,
                &table,
                &lookup,
                &value,
                remaining.as_slice(),
                staged_backing,
                actual,
                0,
            )?;
        }
        let scratch_bytes = scratch.backing_bytes().ok_or_else(|| ex.out_of_memory())?;
        admit_json_map_stage(
            ex,
            s,
            &table,
            &lookup,
            &value,
            remaining.as_slice(),
            staged_backing,
            scratch_bytes,
            numbers_growth,
        )?;
        table
            .json_reserve_numbers(&lookup, numbers_growth)
            .ok_or_else(|| ex.out_of_memory())?;
        admit_json_map_stage(
            ex,
            s,
            &table,
            &lookup,
            &value,
            remaining.as_slice(),
            staged_backing,
            scratch_bytes,
            0,
        )?;
        admit_json_map_stage(
            ex,
            s,
            &table,
            &lookup,
            &value,
            remaining.as_slice(),
            staged_backing,
            scratch_bytes,
            node_growth,
        )?;
        table
            .json_reserve_node(plan)
            .ok_or_else(|| ex.out_of_memory())?;
        admit_json_map_stage(
            ex,
            s,
            &table,
            &lookup,
            &value,
            remaining.as_slice(),
            staged_backing,
            scratch_bytes,
            0,
        )?;
        match table.json_insert_prepared(lookup, value, plan, scratch) {
            Ok((old, unused)) => {
                drop(old);
                drop(unused);
            }
            Err((lookup, value)) => {
                drop(lookup);
                drop(value);
                return Err(ex.out_of_memory());
            }
        }
    }
    s.json_admit_extra(
        ex,
        (std::mem::size_of::<Map>() + 2 * std::mem::size_of::<usize>()) as u64,
        |visit| {
            visit.bytes(staged_backing)?;
            table.visit_json_retained(visit, &oom)
        },
    )?;
    Ok(Map::new_value(table, fixed))
}

#[allow(clippy::too_many_arguments)]
fn admit_json_map_stage(
    ex: &mut Execution,
    s: &mut XdtoCtx,
    table: &Table,
    lookup: &JsonMapLookup,
    value: &Value,
    remaining: &[(Value, Value)],
    staged_backing: u64,
    scratch: u64,
    growth: u64,
) -> Result<(), Exception> {
    let oom = ex.out_of_memory();
    let metadata = (std::mem::size_of::<JsonMapLookup>()
        + std::mem::size_of::<bsl_prim::boostmap::InsertPlan>()
        + std::mem::size_of::<bsl_prim::boostmap::InsertScratch>()
        + std::mem::size_of::<std::vec::IntoIter<(Value, Value)>>()) as u64;
    let backing = staged_backing
        .checked_add(scratch)
        .and_then(|n| n.checked_add(metadata))
        .ok_or_else(|| ex.out_of_memory())?;
    s.json_admit_extra(ex, growth, |visit| {
        table.visit_json_retained(visit, &oom)?;
        visit.bytes(backing)?;
        visit.work(remaining.len() as u64)?;
        for (key, value) in remaining {
            visit.value(key)?;
            visit.value(value)?;
        }
        visit.value(lookup.value())?;
        visit.value(value)
    })
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
        if s.is_json_read() {
            s.json_admit_extra(
                ex,
                (std::mem::size_of::<KeyValue>() + 2 * std::mem::size_of::<usize>()) as u64,
                |visit| {
                    visit.value(&k)?;
                    visit.value(&v)
                },
            )?;
        }
        Ok(KeyValue::new_value(k, v))
    }

    fn xdto_type(&self) -> QName {
        core("KeyAndValue")
    }
}
