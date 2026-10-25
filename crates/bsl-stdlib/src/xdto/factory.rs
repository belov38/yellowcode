use super::data::{
    DataObject, DataValue, Factory, FactoryRef, Walk, XdtoData, XdtoWriteFlags, data_to_value,
    object_to_data,
};
use super::io_json::{read_json, write_json};
use super::io_xml::{read_xml, write_xml};
use super::types::{PackagesColl, type_name_of, type_value};
use super::{args_error, is_any, object_type, types, value_type};
use crate::bind::{arg_str, arg_value, is_given, msg, type_mismatch};
use crate::collections::{ctor_not_implemented, method_name, not_implemented, property_name};
use crate::enums::as_enum_value;
use crate::xml::{xml_reader, xml_writer};
use bsl_lib::xdto::{Model, QName, platform_model};
use bsl_prim::Str;
use bsl_vm::{
    Arg, CallResult, Execution, HostError, HostObject, MemberId, Registry, TypeId, TypeImpl, Value,
};
use std::any::Any;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

fn platform() -> Arc<Model> {
    static M: OnceLock<Arc<Model>> = OnceLock::new();
    M.get_or_init(|| Arc::new(platform_model().clone())).clone()
}

pub(crate) fn factory_value(f: FactoryRef) -> Value {
    Value::Object(Rc::new(FactoryObject { f }))
}

struct FactoryObject {
    f: FactoryRef,
}

pub(super) fn factory_ref(v: &Value) -> Option<FactoryRef> {
    v.as_object()?
        .as_any()
        .downcast_ref::<FactoryObject>()
        .map(|o| o.f.clone())
}

pub(super) fn platform_factory() -> (FactoryRef, Value) {
    let f = Rc::new(Factory::new(platform()));
    (f.clone(), factory_value(f))
}

pub(super) fn data_of(ex: &mut Execution, v: &Value, param: usize) -> CallResult<XdtoData> {
    let Some(o) = v.as_object() else {
        return Err(type_mismatch(param).into());
    };
    if let Some(obj) = o.as_any().downcast_ref::<DataObject>() {
        let rc = obj.rc().ok_or_else(|| type_mismatch(param))?;
        let mut walk = Walk::new();
        return Ok(object_to_data(ex, &rc, &mut walk)?);
    }
    if let Some(dv) = o.as_any().downcast_ref::<DataValue>() {
        return Ok(XdtoData::Value {
            projection: None,
            ty: dv.ty.clone(),
            lexical: dv.lexical.clone(),
            value: dv.value.clone(),
        });
    }
    Err(type_mismatch(param).into())
}

pub(super) fn explicit_arg(ex: &mut Execution, a: Option<&Arg>) -> CallResult<bool> {
    let v = arg_value(ex, a)?;
    Ok(as_enum_value(&v).is_some_and(|e| {
        e.type_idx() == crate::xml::types().assignment && e.name_en() == "Explicit"
    }))
}

impl FactoryObject {
    fn type_arg(&self, ex: &mut Execution, a: Option<&Arg>, param: usize) -> CallResult<QName> {
        let v = arg_value(ex, a)?;
        type_name_of(&v).ok_or_else(|| type_mismatch(param).into())
    }

    fn create(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let q = self.type_arg(ex, args.first(), 1)?;
        let m = &self.f.model;
        if object_type(m, &q).is_some() && !is_any(&q) {
            if m.type_def(&q)
                .is_some_and(|d| matches!(d, bsl_lib::xdto::TypeDef::Object(o) if o.abstract_))
            {
                return Err(args_error(msg::XDTO_ABSTRACT, vec![q.name.clone()]).into());
            }
            return Ok(Value::Object(DataObject::new(ex, self.f.clone(), q)?));
        }
        if value_type(m, &q).is_none() {
            return Err(type_mismatch(1).into());
        }
        let v = arg_value(ex, args.get(1))?;
        let d = match &v {
            Value::Object(o) if o.as_any().is::<DataValue>() => data_of(ex, &v, 2)?,
            _ => super::data::value_to_data(ex, m, &q, &v)?,
        };
        let XdtoData::Value { lexical, value, .. } = d else {
            return Err(type_mismatch(2).into());
        };
        Ok(Value::Object(Rc::new(DataValue {
            factory: self.f.clone(),
            ty: q,
            lexical,
            value,
        })))
    }

    fn read_xml(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let rv = arg_value(ex, args.first())?;
        let expect = match arg_value(ex, args.get(1))? {
            Value::Undefined => None,
            _ => Some(self.type_arg(ex, args.get(1), 2)?),
        };
        let mut st = xml_reader(ex, &rv)?;
        let d = read_xml(ex, &self.f.model, &mut st, expect.as_ref())?;
        Ok(data_to_value(ex, &self.f, d)?)
    }

    fn write_xml(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let wv = arg_value(ex, args.first())?;
        let item = arg_value(ex, args.get(1))?;
        let local = match arg_value(ex, args.get(2))? {
            Value::Undefined => None,
            _ => Some(arg_str(ex, args.get(2))?.to_string_lossy()),
        };
        let ns = match arg_value(ex, args.get(3))? {
            Value::Undefined => String::new(),
            _ => arg_str(ex, args.get(3))?.to_string_lossy(),
        };
        let explicit = explicit_arg(ex, args.get(5))?;
        let mut st = xml_writer(ex, &wv)?;
        let d = data_of(ex, &item, 2)?;
        let root = local.map(|l| QName::new(&ns, &l));
        write_xml(
            ex,
            &self.f.model,
            &mut st,
            root.as_ref(),
            &d,
            XdtoWriteFlags {
                explicit_type: explicit,
                untyped: false,
            },
        )?;
        Ok(Value::Undefined)
    }

    fn write_json(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let wv = arg_value(ex, args.first())?;
        let item = arg_value(ex, args.get(1))?;
        let explicit = explicit_arg(ex, args.get(2))?;
        drop(crate::json::json_writer(ex, &wv)?);
        let d = data_of(ex, &item, 2)?;
        write_json(
            ex,
            &self.f.model,
            &wv,
            &d,
            XdtoWriteFlags {
                explicit_type: explicit,
                untyped: false,
            },
        )?;
        Ok(Value::Undefined)
    }

    fn read_json(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let rv = arg_value(ex, args.first())?;
        let expect = match arg_value(ex, args.get(1))? {
            Value::Undefined => None,
            _ => Some(self.type_arg(ex, args.get(1), 2)?),
        };
        drop(crate::json::json_reader(ex, &rv)?);
        let d = read_json(ex, &self.f.model, &rv, expect.as_ref())?;
        Ok(data_to_value(ex, &self.f, d)?)
    }

    fn type_of(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        if args.len() == 1 {
            let v = arg_value(ex, args.first())?;
            let q = crate::values::xml_type::XmlDataType::of(&v).ok_or_else(|| type_mismatch(1))?;
            return Ok(type_value(&self.f, &QName::new(&q.uri, &q.name)));
        }
        let ns = arg_str(ex, args.first())?.to_string_lossy();
        let name = arg_str(ex, args.get(1))?.to_string_lossy();
        Ok(type_value(&self.f, &QName::new(&ns, &name)))
    }

    fn export_schema(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let ns = arg_str(ex, args.first())?.to_string_lossy();
        let Some(i) = self.f.model.packages.iter().position(|p| p.ns == ns) else {
            return Err(args_error(msg::XDTO_TYPE_NOT_FOUND, vec![ns]).into());
        };
        Ok(crate::xml::export_schema_set(
            self.f.model.packages[i].clone(),
        ))
    }
}

impl HostObject for FactoryObject {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(types().factory)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let t = types().factory;
        match property_name(t, m) {
            Some("Packages") => Ok(Value::Object(Rc::new(PackagesColl { f: self.f.clone() }))),
            _ => Err(not_implemented(t, m).into()),
        }
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        let t = types().factory;
        match crate::collections::property_ru(t, m) {
            "" => Err(not_implemented(t, m).into()),
            ru => Err(HostError::key(msg::PROP_NOT_WRITABLE)
                .with_arg(ru)
                .raw()
                .into()),
        }
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        let t = types().factory;
        let args: &[Arg] = args;
        match method_name(t, m) {
            Some("Create") => self.create(ex, args),
            Some("Type") => self.type_of(ex, args),
            Some("ReadXML") => self.read_xml(ex, args),
            Some("WriteXML") => self.write_xml(ex, args),
            Some("WriteJSON") => self.write_json(ex, args),
            Some("ReadJSON") => self.read_json(ex, args),
            Some("ExportXMLSchema") => self.export_schema(ex, args),
            _ => Err(not_implemented(t, m).into()),
        }
    }

    fn presentation(&self, _ex: &mut Execution) -> CallResult<Option<Str>> {
        Ok(None)
    }
}

fn ctor(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    let args: &[Arg] = args;
    let model = match args.first() {
        None => platform(),
        Some(_) => {
            let set = arg_value(ex, args.first())?;
            if !is_given(args.first()) || matches!(set, Value::Undefined) {
                platform()
            } else if args.len() > 1 {
                return Err(ctor_not_implemented(types().factory).into());
            } else {
                let packages = crate::xml::schema_set_packages(ex, &set)?;
                ex.reserve((packages.len() as u64).saturating_mul(1 << 12))?;
                let xs = platform()
                    .package(super::XS)
                    .cloned()
                    .ok_or_else(|| type_mismatch(1))?;
                Arc::new(Model::new(vec![xs]).with(packages))
            }
        }
    };
    Ok(factory_value(Rc::new(Factory::new(model))))
}

pub(super) fn register(reg: &mut Registry) {
    reg.type_impl(
        types().factory,
        TypeImpl {
            ctor: Some(Arc::new(ctor)),
        },
    );
    reg.global_prop(
        "ФабрикаXDTO",
        "XDTOFactory",
        Arc::new(|_| Ok(factory_value(Rc::new(Factory::new(platform()))))),
    );
}
