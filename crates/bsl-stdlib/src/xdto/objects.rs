use super::data::{
    DataList, DataObject, DataValue, FactoryRef, Item, XdtoData, property_type_error,
    value_from_text, value_to_data,
};
use super::factory::factory_value;
use super::types::type_value;
use super::{EProp, args_error, check_structure, is_descendant, object_type, types};
use crate::bind::{arg_str, arg_value, msg, type_mismatch};
use crate::collections::{method_name, not_implemented, property_name};
use bsl_prim::{Str, fold_name};
use bsl_vm::{
    Arg, CallResult, Execution, HostError, HostObject, MemberId, MemberKind, TypeId, Value,
};
use std::any::Any;
use std::collections::HashSet;
use std::rc::Rc;

const PROP_BASE: u32 = 0x1000_0000;

pub(crate) fn to_item(
    ex: &mut Execution,
    f: &FactoryRef,
    ep: &EProp,
    v: &Value,
) -> CallResult<Option<Item>> {
    let m = &f.model;
    let ty = &ep.prop.ty;
    if matches!(v, Value::Undefined) {
        return Ok(ep.prop.nillable.then_some(Item::Data(XdtoData::Nil)));
    }
    if let Value::Object(o) = v {
        if let Some(obj) = o.as_any().downcast_ref::<DataObject>() {
            if !is_descendant(m, &obj.ty, ty) {
                return Err(
                    args_error(msg::XDTO_TWO_TYPES, vec![obj.ty.clark(), ty.clark()]).into(),
                );
            }
            let rc = obj.rc().ok_or_else(|| type_mismatch(2))?;
            return Ok(Some(Item::Obj(rc)));
        }
        if let Some(dv) = o.as_any().downcast_ref::<DataValue>() {
            let d = if &dv.ty == ty || super::is_any(ty) {
                XdtoData::Value {
                    projection: None,
                    ty: dv.ty.clone(),
                    lexical: dv.lexical.clone(),
                    value: dv.value.clone(),
                }
            } else {
                value_from_text(ex, m, ty, &dv.lexical, false)?
            };
            return Ok(Some(Item::Data(d)));
        }
    }
    if object_type(m, ty).is_some() && !super::is_any(ty) {
        return Err(property_type_error().into());
    }
    Ok(Some(Item::Data(value_to_data(ex, m, ty, v)?)))
}

fn find_prop(o: &DataObject, a: &Arg, ex: &mut Execution) -> CallResult<usize> {
    let v = arg_value(ex, Some(a))?;
    let name = match &v {
        Value::Object(p) => match super::types::property_name_of(p) {
            Some(n) => n,
            None => return Err(type_mismatch(1).into()),
        },
        _ => arg_str(ex, Some(a))?.to_string_lossy(),
    };
    o.eps
        .iter()
        .position(|p| p.prop.name == name)
        .ok_or_else(|| args_error(msg::XDTO_UNKNOWN_PROPERTY, vec![name]).into())
}

impl DataObject {
    fn get_prop(&self, ex: &mut Execution, i: usize) -> CallResult<Value> {
        let ep = &self.eps[i];
        if ep.is_list() {
            return Ok(match self.rc() {
                Some(owner) => Value::Object(Rc::new(DataList {
                    owner,
                    prop: ep.prop.name.clone(),
                })),
                None => Value::Undefined,
            });
        }
        let set = self
            .props
            .borrow()
            .iter()
            .find(|(n, _)| n == &ep.prop.name)
            .and_then(|(_, items)| items.first().map(Item::to_value));
        if let Some(v) = set {
            return Ok(v);
        }
        match ep.prop.default.as_deref() {
            Some(d) if !super::is_any(&ep.prop.ty) => {
                match value_from_text(ex, &self.factory.model, &ep.prop.ty, d, false)? {
                    XdtoData::Value { value, .. } => Ok(value),
                    _ => Ok(Value::Undefined),
                }
            }
            _ => Ok(Value::Undefined),
        }
    }

    fn set_prop(&self, ex: &mut Execution, i: usize, v: &Value) -> CallResult<()> {
        let ep = self.eps[i].clone();
        let item = to_item(ex, &self.factory, &ep, v)?;
        let mut props = self.props.borrow_mut();
        let pos = props.iter().position(|(n, _)| n == &ep.prop.name);
        match (item, pos) {
            (Some(it), Some(p)) => props[p].1 = vec![it],
            (Some(it), None) => props.push((ep.prop.name.clone(), vec![it])),
            (None, Some(p)) => {
                props.remove(p);
            }
            (None, None) => {}
        }
        Ok(())
    }

    fn validate(&self, ex: &mut Execution) -> CallResult<()> {
        let mut seen: HashSet<*const DataObject> = HashSet::new();
        let mut work: Vec<Rc<DataObject>> = self.rc().into_iter().collect();
        while let Some(o) = work.pop() {
            if !seen.insert(Rc::as_ptr(&o)) {
                continue;
            }
            ex.charge(1)?;
            let ordered = super::object_type(&o.factory.model, &o.ty).is_none_or(|d| d.ordered);
            check_structure(ex, &o.ty, &o.eps, ordered, &|n| o.count_of(n)).map_err(|b| b.1)?;
            for (_, items) in o.props.borrow().iter() {
                for it in items {
                    if let Item::Obj(c) = it {
                        work.push(c.clone());
                    }
                }
            }
        }
        Ok(())
    }
}

impl HostObject for DataObject {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(types().object)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<Str>> {
        Ok(Some(crate::collections::type_name(ex, types().object)))
    }

    fn find_member(&self, name: &str) -> Option<MemberId> {
        let at = self
            .eps
            .iter()
            .position(|p| p.prop.name == name)
            .or_else(|| {
                let f = fold_name(name);
                self.eps.iter().position(|p| fold_name(&p.prop.name) == f)
            })?;
        Some(MemberId(PROP_BASE + at as u32))
    }

    fn member_kind(&self, m: MemberId) -> Option<MemberKind> {
        let i = m.0.checked_sub(PROP_BASE)? as usize;
        (i < self.eps.len()).then_some(MemberKind::Property {
            readable: true,
            writable: true,
        })
    }

    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        match m.0.checked_sub(PROP_BASE) {
            Some(i) if (i as usize) < self.eps.len() => self.get_prop(ex, i as usize),
            _ => Err(not_implemented(types().object, m).into()),
        }
    }

    fn set(&self, ex: &mut Execution, m: MemberId, v: Value) -> CallResult<()> {
        match m.0.checked_sub(PROP_BASE) {
            Some(i) if (i as usize) < self.eps.len() => self.set_prop(ex, i as usize, &v),
            _ => Err(not_implemented(types().object, m).into()),
        }
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        let t = types().object;
        let args: &[Arg] = args;
        match method_name(t, m) {
            Some("Get") => {
                let i = find_prop(self, args.first().ok_or_else(|| type_mismatch(1))?, ex)?;
                self.get_prop(ex, i)
            }
            Some("Set") => {
                let i = find_prop(self, args.first().ok_or_else(|| type_mismatch(1))?, ex)?;
                let v = arg_value(ex, args.get(1))?;
                self.set_prop(ex, i, &v)?;
                Ok(Value::Undefined)
            }
            Some("IsSet") => {
                let i = find_prop(self, args.first().ok_or_else(|| type_mismatch(1))?, ex)?;
                Ok(Value::Bool(self.count_of(&self.eps[i].prop.name) > 0))
            }
            Some("Unset") => {
                let i = find_prop(self, args.first().ok_or_else(|| type_mismatch(1))?, ex)?;
                let name = self.eps[i].prop.name.clone();
                self.props.borrow_mut().retain(|(n, _)| n != &name);
                Ok(Value::Undefined)
            }
            Some("Add") => {
                let i = find_prop(self, args.first().ok_or_else(|| type_mismatch(1))?, ex)?;
                let v = arg_value(ex, args.get(1))?;
                let ep = self.eps[i].clone();
                let item = to_item(ex, &self.factory, &ep, &v)?;
                if let Some(it) = item {
                    let mut props = self.props.borrow_mut();
                    match props.iter_mut().find(|(n, _)| n == &ep.prop.name) {
                        Some((_, items)) => items.push(it),
                        None => props.push((ep.prop.name.clone(), vec![it])),
                    }
                }
                Ok(Value::Undefined)
            }
            Some("Validate") => {
                self.validate(ex)?;
                Ok(Value::Undefined)
            }
            Some("Type") => Ok(type_value(&self.factory, &self.ty)),
            Some("Factory") => Ok(factory_value(self.factory.clone())),
            _ => Err(not_implemented(t, m).into()),
        }
    }
}

impl HostObject for DataValue {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(types().value)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<Str>> {
        Ok(Some(crate::collections::type_name(ex, types().value)))
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let t = types().value;
        match property_name(t, m) {
            Some("Value") => Ok(self.value.clone()),
            Some("LexicalValue") => Ok(Value::from(self.lexical.as_str())),
            _ => Err(not_implemented(t, m).into()),
        }
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        let t = types().value;
        match crate::collections::property_ru(t, m) {
            "" => Err(not_implemented(t, m).into()),
            ru => Err(HostError::key(msg::PROP_NOT_WRITABLE)
                .with_arg(ru)
                .raw()
                .into()),
        }
    }

    fn call(&self, _ex: &mut Execution, m: MemberId, _args: &mut [Arg]) -> CallResult<Value> {
        let t = types().value;
        match method_name(t, m) {
            Some("Type") => Ok(type_value(&self.factory, &self.ty)),
            Some("Factory") => Ok(factory_value(self.factory.clone())),
            _ => Err(not_implemented(t, m).into()),
        }
    }
}

impl DataList {
    fn ep(&self) -> Option<EProp> {
        self.owner
            .eps
            .iter()
            .find(|p| p.prop.name == self.prop)
            .cloned()
    }

    fn len(&self) -> usize {
        self.owner.count_of(&self.prop)
    }

    fn index(&self, ex: &mut Execution, a: Option<&Arg>) -> CallResult<usize> {
        let n = crate::bind::arg_i64(ex, a, 1)?;
        usize::try_from(n)
            .ok()
            .filter(|&i| i < self.len())
            .ok_or_else(|| HostError::key(msg::RANGE).into())
    }

    fn insert(&self, ex: &mut Execution, at: Option<usize>, v: &Value) -> CallResult<()> {
        let ep = self.ep().ok_or_else(|| type_mismatch(1))?;
        let item = to_item(ex, &self.owner.factory, &ep, v)?;
        let Some(it) = item else {
            return Ok(());
        };
        let mut props = self.owner.props.borrow_mut();
        let pos = props.iter().position(|(n, _)| n == &self.prop);
        let items = match pos {
            Some(p) => &mut props[p].1,
            None => {
                props.push((self.prop.clone(), Vec::new()));
                let last = props.len() - 1;
                &mut props[last].1
            }
        };
        match at {
            Some(i) if i <= items.len() => items.insert(i, it),
            _ => items.push(it),
        }
        Ok(())
    }
}

impl HostObject for DataList {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(types().list)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<Str>> {
        Ok(Some(crate::collections::type_name(ex, types().list)))
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let t = types().list;
        match property_name(t, m) {
            Some("Owner") => Ok(Value::Object(self.owner.clone())),
            _ => Err(not_implemented(t, m).into()),
        }
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        Err(not_implemented(types().list, m).into())
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        let t = types().list;
        let args: &[Arg] = args;
        match method_name(t, m) {
            Some("Add") => {
                let v = arg_value(ex, args.first())?;
                self.insert(ex, None, &v)?;
                Ok(Value::Undefined)
            }
            Some("Insert") => {
                let i = crate::bind::arg_i64(ex, args.first(), 1)?;
                let v = arg_value(ex, args.get(1))?;
                let at = usize::try_from(i)
                    .ok()
                    .filter(|&i| i <= self.len())
                    .ok_or_else(|| HostError::key(msg::RANGE))?;
                self.insert(ex, Some(at), &v)?;
                Ok(Value::Undefined)
            }
            Some("Count") => Ok(Value::from(self.len() as i64)),
            Some("Get") => {
                let i = self.index(ex, args.first())?;
                Ok(self.item_value(i))
            }
            Some("Set") => {
                let i = self.index(ex, args.first())?;
                let v = arg_value(ex, args.get(1))?;
                let ep = self.ep().ok_or_else(|| type_mismatch(1))?;
                if let Some(it) = to_item(ex, &self.owner.factory, &ep, &v)? {
                    let mut props = self.owner.props.borrow_mut();
                    if let Some((_, items)) = props.iter_mut().find(|(n, _)| n == &self.prop)
                        && i < items.len()
                    {
                        items[i] = it;
                    }
                }
                Ok(Value::Undefined)
            }
            Some("Clear") => {
                self.owner
                    .props
                    .borrow_mut()
                    .retain(|(n, _)| n != &self.prop);
                Ok(Value::Undefined)
            }
            Some("Delete") => {
                let i = if args.is_empty() || !crate::bind::is_given(args.first()) {
                    self.len().checked_sub(1)
                } else {
                    Some(self.index(ex, args.first())?)
                };
                let mut props = self.owner.props.borrow_mut();
                let mut removed = Value::Undefined;
                if let Some(i) = i
                    && let Some((_, items)) = props.iter_mut().find(|(n, _)| n == &self.prop)
                    && i < items.len()
                {
                    removed = items.remove(i).to_value();
                }
                Ok(removed)
            }
            _ => Err(not_implemented(t, m).into()),
        }
    }

    fn iter_len(&self, _ex: &mut Execution) -> Option<usize> {
        Some(self.len())
    }

    fn iter_get(&self, _ex: &mut Execution, i: usize) -> CallResult<Value> {
        Ok(self.item_value(i))
    }

    fn index_get(&self, ex: &mut Execution, key: &Value) -> CallResult<Value> {
        let i = crate::bind::to_number(ex, key)?.trunc_to_i64_saturating();
        let i = usize::try_from(i)
            .ok()
            .filter(|&i| i < self.len())
            .ok_or_else(|| HostError::key(msg::INDEX_OFFBOUND).raw())?;
        Ok(self.item_value(i))
    }
}

impl DataList {
    fn item_value(&self, i: usize) -> Value {
        self.owner
            .props
            .borrow()
            .iter()
            .find(|(n, _)| n == &self.prop)
            .and_then(|(_, items)| items.get(i))
            .map_or(Value::Undefined, Item::to_value)
    }
}
