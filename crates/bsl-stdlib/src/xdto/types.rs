use super::data::FactoryRef;
use super::{EProp, is_descendant, object_type, types, value_type};
use crate::bind::{arg_i64, arg_str, arg_value, msg, type_mismatch};
use crate::collections::{method_name, not_implemented, property_name, property_ru};
use crate::enums::enum_value;
use bsl_lib::xdto::{Form, QName};
use bsl_vm::{Arg, CallResult, Execution, HostError, HostObject, MemberId, ObjRef, TypeId, Value};
use std::any::Any;
use std::rc::Rc;

pub(crate) fn register(_reg: &mut bsl_vm::Registry) {}

pub(crate) fn type_value(f: &FactoryRef, q: &QName) -> Value {
    if value_type(&f.model, q).is_some() {
        return Value::Object(Rc::new(ValueTypeObj {
            f: f.clone(),
            q: q.clone(),
        }));
    }
    if object_type(&f.model, q).is_some() {
        return Value::Object(Rc::new(ObjectTypeObj {
            f: f.clone(),
            q: q.clone(),
        }));
    }
    Value::Undefined
}

pub(crate) fn property_name_of(o: &ObjRef) -> Option<String> {
    let p = o.as_any().downcast_ref::<PropertyObj>()?;
    Some(p.ep().prop.name)
}

pub(crate) fn type_name_of(v: &Value) -> Option<QName> {
    let o = v.as_object()?;
    if let Some(t) = o.as_any().downcast_ref::<ValueTypeObj>() {
        return Some(t.q.clone());
    }
    o.as_any()
        .downcast_ref::<ObjectTypeObj>()
        .map(|t| t.q.clone())
}

pub(crate) fn comparison_input_len(value: &Value) -> Option<u64> {
    let object = value.as_object()?;
    let name = if let Some(ty) = object.as_any().downcast_ref::<ValueTypeObj>() {
        &ty.q
    } else {
        &object.as_any().downcast_ref::<ObjectTypeObj>()?.q
    };
    Some((name.ns.len() as u64).saturating_add(name.name.len() as u64))
}

fn read_only(t: bsl_vm::TypeIdx, m: MemberId) -> bsl_vm::CallError {
    match property_ru(t, m) {
        "" => not_implemented(t, m).into(),
        ru => HostError::key(msg::PROP_NOT_WRITABLE)
            .with_arg(ru)
            .raw()
            .into(),
    }
}

fn base_value(f: &FactoryRef, q: &QName) -> Value {
    match f.model.type_def(q).and_then(|d| d.base()) {
        Some(b) => type_value(f, b),
        None => Value::Undefined,
    }
}

pub(crate) struct ValueTypeObj {
    f: FactoryRef,
    q: QName,
}

impl HostObject for ValueTypeObj {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(types().value_type)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let t = types().value_type;
        match property_name(t, m) {
            Some("Name") => Ok(Value::from(self.q.name.as_str())),
            Some("NamespaceURI") => Ok(Value::from(self.q.ns.as_str())),
            Some("BaseType") => Ok(base_value(&self.f, &self.q)),
            _ => Err(not_implemented(t, m).into()),
        }
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        Err(read_only(types().value_type, m))
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        let t = types().value_type;
        match method_name(t, m) {
            Some("IsDescendant") => {
                let v = arg_value(ex, args.first())?;
                let other = type_name_of(&v).ok_or_else(|| type_mismatch(1))?;
                Ok(Value::Bool(is_descendant(&self.f.model, &self.q, &other)))
            }
            _ => Err(not_implemented(t, m).into()),
        }
    }

    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        let o = other.as_any().downcast_ref::<ValueTypeObj>()?;
        Some(o.q == self.q)
    }
}

pub(crate) struct ObjectTypeObj {
    f: FactoryRef,
    q: QName,
}

impl HostObject for ObjectTypeObj {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(types().object_type)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let t = types().object_type;
        let def = object_type(&self.f.model, &self.q);
        match property_name(t, m) {
            Some("Name") => Ok(Value::from(self.q.name.as_str())),
            Some("NamespaceURI") => Ok(Value::from(self.q.ns.as_str())),
            Some("BaseType") => Ok(base_value(&self.f, &self.q)),
            Some("Properties") => Ok(Value::Object(Rc::new(PropsColl {
                f: self.f.clone(),
                owner: self.q.clone(),
            }))),
            Some("Abstract") => Ok(Value::Bool(def.is_some_and(|d| d.abstract_))),
            Some("Open") => Ok(Value::Bool(def.is_some_and(|d| d.open))),
            Some("Mixed") => Ok(Value::Bool(def.is_some_and(|d| d.mixed))),
            Some("Sequenced") => Ok(Value::Bool(def.is_some_and(|d| d.sequenced || !d.ordered))),
            Some("Ordered") => Ok(Value::Bool(def.is_none_or(|d| d.ordered))),
            _ => Err(not_implemented(t, m).into()),
        }
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        Err(read_only(types().object_type, m))
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        let t = types().object_type;
        match method_name(t, m) {
            Some("IsDescendant") => {
                let v = arg_value(ex, args.first())?;
                let other = type_name_of(&v).ok_or_else(|| type_mismatch(1))?;
                Ok(Value::Bool(is_descendant(&self.f.model, &self.q, &other)))
            }
            _ => Err(not_implemented(t, m).into()),
        }
    }

    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        let o = other.as_any().downcast_ref::<ObjectTypeObj>()?;
        Some(o.q == self.q)
    }
}

pub(crate) struct PropsColl {
    f: FactoryRef,
    owner: QName,
}

impl PropsColl {
    fn props(&self) -> std::sync::Arc<Vec<EProp>> {
        self.f.props.peek(&self.f.model, &self.owner)
    }

    fn at(&self, i: usize) -> Value {
        Value::Object(Rc::new(PropertyObj {
            f: self.f.clone(),
            owner: self.owner.clone(),
            idx: i,
        }))
    }
}

impl HostObject for PropsColl {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(types().properties)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        Err(not_implemented(types().properties, m).into())
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        Err(read_only(types().properties, m))
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        let t = types().properties;
        match method_name(t, m) {
            Some("Count") => Ok(Value::from(self.props().len() as i64)),
            Some("Get") => {
                let v = arg_value(ex, args.first())?;
                if let Value::Number(_) = v {
                    let i = arg_i64(ex, args.first(), 1)?;
                    return match usize::try_from(i).ok().filter(|&i| i < self.props().len()) {
                        Some(i) => Ok(self.at(i)),
                        None => Err(HostError::key(msg::RANGE).into()),
                    };
                }
                let name = arg_str(ex, args.first())?.to_string_lossy();
                Ok(
                    match self.props().iter().position(|p| p.prop.name == name) {
                        Some(i) => self.at(i),
                        None => Value::Undefined,
                    },
                )
            }
            _ => Err(not_implemented(t, m).into()),
        }
    }

    fn iter_len(&self, _ex: &mut Execution) -> Option<usize> {
        Some(self.props().len())
    }

    fn iter_get(&self, _ex: &mut Execution, i: usize) -> CallResult<Value> {
        Ok(self.at(i))
    }

    fn index_get(&self, ex: &mut Execution, key: &Value) -> CallResult<Value> {
        let i = crate::bind::to_number(ex, key)?.trunc_to_i64_saturating();
        match usize::try_from(i).ok().filter(|&i| i < self.props().len()) {
            Some(i) => Ok(self.at(i)),
            None => Err(HostError::key(msg::INDEX_OFFBOUND).raw().into()),
        }
    }
}

pub(crate) struct PropertyObj {
    f: FactoryRef,
    owner: QName,
    idx: usize,
}

impl PropertyObj {
    fn ep(&self) -> EProp {
        self.f
            .props
            .peek(&self.f.model, &self.owner)
            .get(self.idx)
            .cloned()
            .unwrap_or_else(|| EProp {
                prop: bsl_lib::xdto::Property {
                    name: String::new(),
                    ty: super::any_type(),
                    lower: 0,
                    upper: 1,
                    nillable: false,
                    form: Form::Element,
                    qualified: false,
                    default: None,
                    local_name: None,
                    fixed: None,
                },
                owner_ns: String::new(),
            })
    }
}

impl HostObject for PropertyObj {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(types().property)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let t = types().property;
        let ep = self.ep();
        let p = &ep.prop;
        let text = |s: &str| Ok(Value::from(s));
        match property_name(t, m) {
            Some("Name") => text(&p.name),
            Some("LocalName") if p.form == Form::Text => text(""),
            Some("LocalName") => text(ep.xml_name()),
            Some("NamespaceURI") => text(ep.xml_ns().unwrap_or("")),
            Some("Type") => Ok(type_value(&self.f, &p.ty)),
            Some("LowerBound") => Ok(Value::from(i64::from(p.lower))),
            Some("UpperBound") => Ok(Value::from(p.upper)),
            Some("Nillable") => Ok(Value::Bool(p.nillable)),
            Some("Qualified") => Ok(Value::Bool(p.qualified)),
            Some("Form") => {
                let n = match p.form {
                    Form::Attribute => "Attribute",
                    Form::Element => "Element",
                    Form::Text => "Text",
                };
                enum_value(types().form, n).ok_or_else(|| not_implemented(t, m).into())
            }
            Some("DefaultValue") => {
                let Some(d) = p.default.as_deref() else {
                    return Ok(Value::Undefined);
                };
                match super::data::value_from_text(ex, &self.f.model, &p.ty, d, false)? {
                    super::data::XdtoData::Value {
                        ty, lexical, value, ..
                    } => Ok(Value::Object(Rc::new(super::data::DataValue {
                        factory: self.f.clone(),
                        ty,
                        lexical,
                        value,
                    }))),
                    _ => Ok(Value::from(d)),
                }
            }
            Some("Fixed") => Ok(Value::Bool(p.fixed.is_some())),
            Some("OwnerType") => Ok(type_value(&self.f, &self.owner)),
            _ => Err(not_implemented(t, m).into()),
        }
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        Err(read_only(types().property, m))
    }

    fn call(&self, _ex: &mut Execution, m: MemberId, _args: &mut [Arg]) -> CallResult<Value> {
        Err(not_implemented(types().property, m).into())
    }
}

pub(crate) struct PackagesColl {
    pub f: FactoryRef,
}

impl PackagesColl {
    fn at(&self, i: usize) -> Value {
        Value::Object(Rc::new(PackageObj {
            f: self.f.clone(),
            idx: i,
        }))
    }
}

impl HostObject for PackagesColl {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(types().packages)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        Err(not_implemented(types().packages, m).into())
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        Err(read_only(types().packages, m))
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        let t = types().packages;
        match method_name(t, m) {
            Some("Count") => Ok(Value::from(self.f.model.packages.len() as i64)),
            Some("Get") => {
                let v = arg_value(ex, args.first())?;
                if let Value::Number(_) = v {
                    let i = arg_i64(ex, args.first(), 1)?;
                    return match usize::try_from(i)
                        .ok()
                        .filter(|&i| i < self.f.model.packages.len())
                    {
                        Some(i) => Ok(self.at(i)),
                        None => Err(HostError::key(msg::RANGE).into()),
                    };
                }
                let ns = arg_str(ex, args.first())?.to_string_lossy();
                Ok(
                    match self.f.model.packages.iter().position(|p| p.ns == ns) {
                        Some(i) => self.at(i),
                        None => Value::Undefined,
                    },
                )
            }
            _ => Err(not_implemented(t, m).into()),
        }
    }

    fn iter_len(&self, _ex: &mut Execution) -> Option<usize> {
        Some(self.f.model.packages.len())
    }

    fn iter_get(&self, _ex: &mut Execution, i: usize) -> CallResult<Value> {
        Ok(self.at(i))
    }

    fn index_get(&self, ex: &mut Execution, key: &Value) -> CallResult<Value> {
        let i = crate::bind::to_number(ex, key)?.trunc_to_i64_saturating();
        match usize::try_from(i)
            .ok()
            .filter(|&i| i < self.f.model.packages.len())
        {
            Some(i) => Ok(self.at(i)),
            None => Err(HostError::key(msg::INDEX_OFFBOUND).raw().into()),
        }
    }
}

pub(crate) struct PackageObj {
    f: FactoryRef,
    idx: usize,
}

impl PackageObj {
    fn pkg(&self) -> &bsl_lib::xdto::Package {
        &self.f.model.packages[self.idx]
    }

    fn at(&self, i: usize) -> Value {
        match self.f.named_types[self.idx]
            .get(i)
            .and_then(|&j| self.pkg().types.get(j))
        {
            Some(d) => type_value(&self.f, d.name()),
            None => Value::Undefined,
        }
    }
}

impl HostObject for PackageObj {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(types().package)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let t = types().package;
        match property_name(t, m) {
            Some("NamespaceURI") => Ok(Value::from(self.pkg().ns.as_str())),
            Some("ElementFormQualified") => Ok(Value::Bool(self.pkg().element_qualified)),
            Some("AttributeFormQualified") => Ok(Value::Bool(self.pkg().attribute_qualified)),
            _ => Err(not_implemented(t, m).into()),
        }
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        Err(read_only(types().package, m))
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        let t = types().package;
        match method_name(t, m) {
            Some("Count") => Ok(Value::from(self.f.named_types[self.idx].len() as i64)),
            Some("Get") => {
                let v = arg_value(ex, args.first())?;
                if let Value::Number(_) = v {
                    let i = arg_i64(ex, args.first(), 1)?;
                    return match usize::try_from(i)
                        .ok()
                        .filter(|&i| i < self.f.named_types[self.idx].len())
                    {
                        Some(i) => Ok(self.at(i)),
                        None => Err(HostError::key(msg::RANGE).into()),
                    };
                }
                let name = arg_str(ex, args.first())?.to_string_lossy();
                Ok(type_value(&self.f, &QName::new(&self.pkg().ns, &name)))
            }
            _ => Err(not_implemented(t, m).into()),
        }
    }

    fn iter_len(&self, _ex: &mut Execution) -> Option<usize> {
        Some(self.f.named_types[self.idx].len())
    }

    fn iter_get(&self, _ex: &mut Execution, i: usize) -> CallResult<Value> {
        Ok(self.at(i))
    }

    fn index_get(&self, ex: &mut Execution, key: &Value) -> CallResult<Value> {
        let i = crate::bind::to_number(ex, key)?.trunc_to_i64_saturating();
        match usize::try_from(i)
            .ok()
            .filter(|&i| i < self.f.named_types[self.idx].len())
        {
            Some(i) => Ok(self.at(i)),
            None => Err(HostError::key(msg::INDEX_OFFBOUND).raw().into()),
        }
    }
}
