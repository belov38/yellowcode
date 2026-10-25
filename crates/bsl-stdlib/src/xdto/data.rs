use super::{EProp, PropCache, any_type, builtin_root, check_error, mismatch_error};
use crate::bind::{msg, type_mismatch};
use crate::serial::cycle_fatal;
use crate::serial::depth::DepthGuard;
use bsl_lib::xdto::{Form, Model, QName, check_lexical};
use bsl_prim::Str;
use bsl_vm::{CallResult, Execution, HostError, TypeId, Value};
use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScalarProjection {
    Host {
        typed: bool,
    },
    Read {
        explicit_type: bool,
        source_text: String,
    },
}

#[derive(Clone, Debug)]
pub enum XdtoData {
    Value {
        projection: Option<ScalarProjection>,
        ty: QName,
        lexical: String,
        value: Value,
    },
    Object {
        ty: QName,
        props: Vec<(String, Vec<XdtoData>)>,
    },
    Nil,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct XdtoWriteFlags {
    pub explicit_type: bool,
    pub untyped: bool,
}

pub(crate) struct Factory {
    pub model: Arc<Model>,
    pub props: PropCache,
    pub named_types: Vec<Vec<usize>>,
}

impl Factory {
    pub(crate) fn new(model: Arc<Model>) -> Factory {
        Factory {
            named_types: model
                .packages
                .iter()
                .map(|p| p.named_type_indices().collect())
                .collect(),
            model,
            props: PropCache::default(),
        }
    }
}

pub(crate) type FactoryRef = Rc<Factory>;

pub(crate) enum Item {
    Data(XdtoData),
    Obj(Rc<DataObject>),
}

impl Item {
    pub(crate) fn to_value(&self) -> Value {
        match self {
            Item::Data(XdtoData::Value { value, .. }) => value.clone(),
            Item::Data(_) => Value::Undefined,
            Item::Obj(o) => Value::Object(o.clone()),
        }
    }
}

pub(crate) struct DataObject {
    pub factory: FactoryRef,
    pub ty: QName,
    pub props: RefCell<Vec<(String, Vec<Item>)>>,
    pub eps: Arc<Vec<EProp>>,
    pub me: Weak<DataObject>,
}

impl DataObject {
    pub(crate) fn new(
        ex: &mut Execution,
        factory: FactoryRef,
        ty: QName,
    ) -> CallResult<Rc<DataObject>> {
        let eps = factory.props.get(ex, &factory.model, &ty)?;
        ex.reserve(256)?;
        Ok(Rc::new_cyclic(|me| DataObject {
            factory,
            ty,
            props: RefCell::new(Vec::new()),
            eps,
            me: me.clone(),
        }))
    }

    pub(crate) fn rc(&self) -> Option<Rc<DataObject>> {
        self.me.upgrade()
    }

    pub(crate) fn count_of(&self, name: &str) -> usize {
        self.props
            .borrow()
            .iter()
            .find(|(n, _)| n == name)
            .map_or(0, |(_, v)| v.len())
    }
}

impl Drop for DataObject {
    fn drop(&mut self) {
        fn children(o: &mut DataObject) -> Vec<Rc<DataObject>> {
            let mut out = Vec::new();
            for (_, items) in o.props.get_mut().drain(..) {
                for it in items {
                    if let Item::Obj(c) = it {
                        out.push(c);
                    }
                }
            }
            out
        }
        let mut stack = children(self);
        while let Some(rc) = stack.pop() {
            if let Ok(mut o) = Rc::try_unwrap(rc) {
                stack.extend(children(&mut o));
            }
        }
    }
}

pub(crate) struct DataValue {
    pub factory: FactoryRef,
    pub ty: QName,
    pub lexical: String,
    pub value: Value,
}

pub(crate) struct DataList {
    pub owner: Rc<DataObject>,
    pub prop: String,
}

fn variety_of(m: &Model, ty: &QName) -> Option<bsl_lib::xdto::Variety> {
    let mut at = ty.clone();
    for _ in 0..=super::MAX_BASES {
        let Some(bsl_lib::xdto::TypeDef::Value(v)) = m.type_def(&at) else {
            return None;
        };
        if !matches!(v.variety, bsl_lib::xdto::Variety::Atomic) {
            return Some(v.variety.clone());
        }
        at = v.base.clone()?;
    }
    None
}

pub(crate) fn lexical_value(
    ex: &mut Execution,
    m: &Model,
    ty: &QName,
    canon: &str,
) -> CallResult<Value> {
    lexical_value_at(ex, m, ty, canon, 0)
}

fn lexical_value_at(
    ex: &mut Execution,
    m: &Model,
    ty: &QName,
    canon: &str,
    depth: usize,
) -> CallResult<Value> {
    if depth > 16 {
        return Ok(Value::from(canon));
    }
    match variety_of(m, ty) {
        Some(bsl_lib::xdto::Variety::List { item }) => {
            let parts: Vec<&str> = canon.split(' ').filter(|p| !p.is_empty()).collect();
            ex.reserve(parts.len() as u64 * 64)?;
            let mut out = Vec::with_capacity(parts.len());
            for p in parts {
                out.push(lexical_value_at(ex, m, &item, p, depth + 1)?);
            }
            return Ok(crate::collections::new_fixed_array(out));
        }
        Some(bsl_lib::xdto::Variety::Union { members }) => {
            for mem in &members {
                if let Ok(c) = check_lexical(m, mem, canon) {
                    return lexical_value_at(ex, m, mem, &c, depth + 1);
                }
            }
            return Ok(Value::from(canon));
        }
        _ => {}
    }
    let kind = builtin_root(m, ty);
    let t = match kind.as_deref() {
        Some(
            "decimal" | "integer" | "int" | "long" | "short" | "byte" | "nonNegativeInteger"
            | "positiveInteger" | "nonPositiveInteger" | "negativeInteger" | "unsignedLong"
            | "unsignedInt" | "unsignedShort" | "unsignedByte" | "float" | "double",
        ) => TypeId::NUMBER,
        Some("boolean") => TypeId::BOOL,
        Some("dateTime" | "date") => TypeId::DATE,
        Some("base64Binary") => TypeId::of_catalog(crate::collections::types().binary),
        _ => return Ok(Value::from(canon)),
    };
    match crate::convert::xml_from_text(ex, t, &Str::from(canon)) {
        Ok(v) => Ok(v),
        Err(bsl_vm::CallError::Host(_)) => Ok(Value::from(canon)),
        Err(e) => Err(e),
    }
}

pub(crate) fn value_from_text(
    ex: &mut Execution,
    m: &Model,
    ty: &QName,
    text: &str,
    wrap_mismatch: bool,
) -> CallResult<XdtoData> {
    ex.charge(1 + text.len() as u64 / 64)?;
    let mut canon = check_lexical(m, ty, text).map_err(|e| {
        if wrap_mismatch {
            mismatch_error(e)
        } else {
            check_error(e)
        }
    })?;
    let value = lexical_value(ex, m, ty, &canon)?;
    if matches!(builtin_root(m, ty).as_deref(), Some("dateTime"))
        && matches!(value, Value::Date(_))
        && (canon.contains('.')
            || canon.ends_with('Z')
            || canon.rfind(['+', '-']).is_some_and(|i| i > 10))
        && let Ok(t) = crate::convert::xml_text(ex, &value)
    {
        canon = t;
    }
    Ok(XdtoData::Value {
        projection: None,
        ty: ty.clone(),
        lexical: canon,
        value,
    })
}

pub(crate) fn natural_type(v: &Value) -> Option<QName> {
    let n = match v {
        Value::Number(_) => "decimal",
        Value::String(_) => "string",
        Value::Bool(_) => "boolean",
        Value::Date(_) => "dateTime",
        _ => return None,
    };
    Some(QName::new(super::XS, n))
}

pub(crate) fn value_to_data(
    ex: &mut Execution,
    m: &Model,
    ty: &QName,
    v: &Value,
) -> CallResult<XdtoData> {
    let target = if super::is_any(ty) {
        natural_type(v).ok_or_else(|| type_mismatch(2))?
    } else {
        ty.clone()
    };
    let text = crate::convert::xml_text(ex, v)?;
    value_from_text(ex, m, &target, &text, matches!(v, Value::String(_)))
}

fn eprop_of<'a>(props: &'a [EProp], name: &str) -> Option<&'a EProp> {
    props.iter().find(|p| p.prop.name == name)
}

pub(crate) struct Walk {
    guard: DepthGuard,
    bytes: u64,
}

impl Walk {
    pub(crate) fn new() -> Walk {
        Walk {
            guard: DepthGuard::new(),
            bytes: 0,
        }
    }

    fn grow(&mut self, ex: &mut Execution, add: usize) -> Result<(), bsl_vm::Exception> {
        self.bytes = self.bytes.saturating_add(add as u64 + 96);
        ex.reserve(self.bytes.saturating_mul(2))
    }
}

pub(crate) fn object_to_data(
    ex: &mut Execution,
    root: &Rc<DataObject>,
    walk: &mut Walk,
) -> Result<XdtoData, bsl_vm::Exception> {
    ex.charge(1)?;
    if walk.guard.enter().is_err() {
        return Err(cycle_fatal(ex));
    }
    let eps = root.eps.clone();
    let n_items: usize = root.props.borrow().iter().map(|(_, v)| v.len() + 1).sum();
    walk.grow(ex, n_items * std::mem::size_of::<XdtoData>())?;
    let mut snapshot: Vec<(String, Vec<SnapshotItem>)> = root
        .props
        .borrow()
        .iter()
        .map(|(n, items)| {
            (
                n.clone(),
                items
                    .iter()
                    .map(|i| match i {
                        Item::Data(d) => SnapshotItem::Data(d.clone()),
                        Item::Obj(o) => SnapshotItem::Obj(o.clone()),
                    })
                    .collect(),
            )
        })
        .collect();
    let order = property_order(root, &eps, &snapshot);
    let mut out: Vec<(String, Vec<XdtoData>)> = Vec::with_capacity(order.len());
    for name in order {
        let Some(pos) = snapshot.iter().position(|(n, _)| n == &name) else {
            continue;
        };
        let (_, items) = snapshot.swap_remove(pos);
        let mut v = Vec::with_capacity(items.len());
        for it in items {
            v.push(match it {
                SnapshotItem::Data(d) => d,
                SnapshotItem::Obj(o) => object_to_data(ex, &o, walk)?,
            });
        }
        out.push((name, v));
    }
    walk.guard.leave();
    Ok(XdtoData::Object {
        ty: root.ty.clone(),
        props: out,
    })
}

#[inline(never)]
fn property_order(
    root: &DataObject,
    eps: &[EProp],
    snapshot: &[(String, Vec<SnapshotItem>)],
) -> Vec<String> {
    let ordered = super::object_type(&root.factory.model, &root.ty).is_none_or(|d| d.ordered);
    if !ordered {
        return snapshot.iter().map(|(n, _)| n.clone()).collect();
    }
    let mut order: Vec<String> = eps
        .iter()
        .map(|p| p.prop.name.clone())
        .filter(|n| snapshot.iter().any(|(s, _)| s == n))
        .collect();
    for (n, _) in snapshot {
        if eprop_of(eps, n).is_none() {
            order.push(n.clone());
        }
    }
    order
}

enum SnapshotItem {
    Data(XdtoData),
    Obj(Rc<DataObject>),
}

pub(crate) fn host_child_explicit(
    d: &XdtoData,
    declared: Option<&QName>,
    untyped: bool,
) -> Result<bool, HostError> {
    let XdtoData::Value {
        projection: Some(ScalarProjection::Host { typed }),
        ty,
        ..
    } = d
    else {
        return Ok(false);
    };
    if untyped && (*typed || declared != Some(ty)) {
        return Err(property_type_error());
    }
    Ok(*typed)
}

pub(crate) fn validate_host_typing(
    ex: &mut Execution,
    model: &Model,
    data: &XdtoData,
    flags: XdtoWriteFlags,
) -> CallResult<()> {
    fn contains(
        ex: &mut Execution,
        data: &XdtoData,
        guard: &mut DepthGuard,
    ) -> Result<bool, bsl_vm::Exception> {
        ex.charge(1)?;
        match data {
            XdtoData::Value {
                projection: Some(ScalarProjection::Host { .. }),
                ..
            } => Ok(true),
            XdtoData::Object { props, .. } => {
                if guard.enter().is_err() {
                    return Err(cycle_fatal(ex));
                }
                let mut found = false;
                'props: for (_, items) in props {
                    for item in items {
                        if contains(ex, item, guard)? {
                            found = true;
                            break 'props;
                        }
                    }
                }
                guard.leave();
                Ok(found)
            }
            _ => Ok(false),
        }
    }
    fn property<'a>(
        ex: &mut Execution,
        model: &'a Model,
        ty: &QName,
        name: &str,
    ) -> Result<Option<&'a bsl_lib::xdto::Property>, bsl_vm::Exception> {
        let mut at = Some(ty);
        let mut found = None;
        for _ in 0..super::MAX_BASES {
            let Some(object) = at.and_then(|q| super::object_type(model, q)) else {
                break;
            };
            ex.charge(1 + object.properties.len() as u64)?;
            if let Some(prop) = object.properties.iter().find(|p| p.name == name) {
                found = Some(prop);
            }
            at = object.base.as_ref();
        }
        Ok(found)
    }
    struct Check<'a> {
        model: &'a Model,
        flags: XdtoWriteFlags,
        guard: DepthGuard,
    }
    impl Check<'_> {
        fn node(
            &mut self,
            ex: &mut Execution,
            data: &XdtoData,
            declared: Option<&QName>,
            form: Form,
            root: bool,
        ) -> CallResult<()> {
            ex.charge(1)?;
            match data {
                XdtoData::Value {
                    projection: Some(ScalarProjection::Host { typed }),
                    ty,
                    ..
                } if !root => {
                    if form != Form::Element && (*typed || declared != Some(ty)) {
                        return Err(property_type_error().into());
                    }
                    host_child_explicit(data, declared, self.flags.untyped)?;
                }
                XdtoData::Object { ty, props } => {
                    if self.guard.enter().is_err() {
                        return Err(cycle_fatal(ex).into());
                    }
                    for (name, items) in props {
                        let prop = property(ex, self.model, ty, name)?;
                        for item in items {
                            self.node(
                                ex,
                                item,
                                prop.map(|p| &p.ty),
                                prop.map_or(Form::Element, |p| p.form),
                                false,
                            )?;
                        }
                    }
                    self.guard.leave();
                }
                _ => {}
            }
            Ok(())
        }
    }
    if contains(ex, data, &mut DepthGuard::new())? {
        Check {
            model,
            flags,
            guard: DepthGuard::new(),
        }
        .node(ex, data, None, Form::Element, true)?;
    }
    Ok(())
}

pub(crate) fn data_copy_sizes(
    ex: &mut Execution,
    d: &XdtoData,
) -> Result<(u64, u64), bsl_vm::Exception> {
    fn sizes(
        ex: &mut Execution,
        d: &XdtoData,
        guard: &mut DepthGuard,
    ) -> Result<(u64, u64), bsl_vm::Exception> {
        ex.charge(1)?;
        let node = std::mem::size_of::<XdtoData>() as u64;
        let text_bytes =
            |q: &QName| (q.ns.capacity() as u64).saturating_add(q.name.capacity() as u64);
        match d {
            XdtoData::Nil => Ok((node, node)),
            XdtoData::Value {
                projection,
                ty,
                lexical,
                value,
            } => {
                let shared = match value {
                    Value::String(s) => (s.len() as u64).saturating_mul(2),
                    Value::Number(n) => n.size_hint_limbs().saturating_mul(4),
                    _ => 0,
                };
                let canonical = node
                    .saturating_add(text_bytes(ty))
                    .saturating_add(lexical.capacity() as u64)
                    .saturating_add(shared);
                let source = match projection {
                    Some(ScalarProjection::Read { source_text, .. }) => {
                        source_text.capacity() as u64
                    }
                    Some(ScalarProjection::Host { .. }) | None => 0,
                };
                Ok((canonical.saturating_add(source), canonical))
            }
            XdtoData::Object { ty, props } => {
                if guard.enter().is_err() {
                    return Err(cycle_fatal(ex));
                }
                let base = node.saturating_add(text_bytes(ty)).saturating_add(
                    (props.capacity() as u64)
                        .saturating_mul(std::mem::size_of::<(String, Vec<XdtoData>)>() as u64),
                );
                let mut retained = base;
                let mut canonical = base.saturating_add(256);
                for (name, items) in props {
                    let buffers = (name.capacity() as u64)
                        .saturating_add((items.capacity() as u64).saturating_mul(node));
                    retained = retained.saturating_add(buffers);
                    canonical = canonical.saturating_add(buffers);
                    for item in items {
                        let (r, c) = sizes(ex, item, guard)?;
                        retained = retained.saturating_add(r);
                        canonical = canonical.saturating_add(c);
                    }
                }
                guard.leave();
                Ok((retained, canonical))
            }
        }
    }
    sizes(ex, d, &mut DepthGuard::new())
}

pub(crate) fn factory_data_copy(
    ex: &mut Execution,
    d: &XdtoData,
) -> Result<XdtoData, bsl_vm::Exception> {
    fn copy(
        ex: &mut Execution,
        d: &XdtoData,
        guard: &mut DepthGuard,
    ) -> Result<XdtoData, bsl_vm::Exception> {
        ex.charge(1)?;
        Ok(match d {
            XdtoData::Nil => XdtoData::Nil,
            XdtoData::Value {
                ty, lexical, value, ..
            } => XdtoData::Value {
                projection: None,
                ty: ty.clone(),
                lexical: lexical.clone(),
                value: value.clone(),
            },
            XdtoData::Object { ty, props } => {
                if guard.enter().is_err() {
                    return Err(cycle_fatal(ex));
                }
                let mut out = Vec::with_capacity(props.len());
                for (name, items) in props {
                    let mut values = Vec::with_capacity(items.len());
                    for item in items {
                        values.push(copy(ex, item, guard)?);
                    }
                    out.push((name.clone(), values));
                }
                guard.leave();
                XdtoData::Object {
                    ty: ty.clone(),
                    props: out,
                }
            }
        })
    }
    let (retained, canonical) = data_copy_sizes(ex, d)?;
    ex.reserve(retained.saturating_add(canonical.saturating_mul(2)))?;
    copy(ex, d, &mut DepthGuard::new())
}

pub(crate) fn data_to_value(
    ex: &mut Execution,
    factory: &FactoryRef,
    d: XdtoData,
) -> Result<Value, bsl_vm::Exception> {
    Ok(match d {
        XdtoData::Nil => Value::Undefined,
        XdtoData::Value {
            ty, lexical, value, ..
        } => Value::Object(Rc::new(DataValue {
            factory: factory.clone(),
            ty,
            lexical,
            value,
        })),
        XdtoData::Object { .. } => {
            let mut walk = Walk::new();
            Value::Object(data_to_object(ex, factory, d, &mut walk)?)
        }
    })
}

fn data_to_object(
    ex: &mut Execution,
    factory: &FactoryRef,
    d: XdtoData,
    walk: &mut Walk,
) -> Result<Rc<DataObject>, bsl_vm::Exception> {
    let XdtoData::Object { ty, props } = d else {
        return DataObject::new(ex, factory.clone(), any_type()).map_err(|e| ex.call_error(e));
    };
    ex.charge(1)?;
    if walk.guard.enter().is_err() {
        return Err(cycle_fatal(ex));
    }
    let o = DataObject::new(ex, factory.clone(), ty).map_err(|e| ex.call_error(e))?;
    let mut slots = Vec::with_capacity(props.len());
    for (name, items) in props {
        let mut v = Vec::with_capacity(items.len());
        for i in items {
            v.push(match i {
                XdtoData::Object { .. } => Item::Obj(data_to_object(ex, factory, i, walk)?),
                XdtoData::Value {
                    ty, lexical, value, ..
                } => Item::Data(XdtoData::Value {
                    projection: None,
                    ty,
                    lexical,
                    value,
                }),
                XdtoData::Nil => Item::Data(XdtoData::Nil),
            });
        }
        slots.push((name, v));
    }
    *o.props.borrow_mut() = slots;
    walk.guard.leave();
    Ok(o)
}

pub(crate) fn property_type_error() -> HostError {
    super::plain_error(msg::XDTO_PROPERTY_TYPE)
}
