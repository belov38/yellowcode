use super::data::{XdtoData, XdtoWriteFlags, data_to_value};
use super::factory::{data_of, explicit_arg, factory_ref, platform_factory};
use super::io_json::{read_json_mode, write_json};
use super::io_xml::{ReadMode, read_xml_mode, write_xml};
use super::value_map::{XdtoCtx, type_qname};
use super::{is_any, types};
use crate::bind::{arg_value, is_given, msg, type_mismatch};
use crate::collections::{method_name, not_implemented, property_name};
use crate::enums::as_enum_value;
use crate::xml::{xml_reader, xml_writer};
use bsl_lib::xdto::QName;
use bsl_prim::Str;
use bsl_vm::{
    Arg, CallResult, Execution, HostError, HostObject, MemberId, Registry, TypeId, TypeImpl, Value,
};
use std::any::Any;
use std::rc::Rc;
use std::sync::Arc;

struct Serializer {
    factory: Value,
    f: super::data::FactoryRef,
}

fn serializer_value(factory: Value, f: super::data::FactoryRef) -> Value {
    Value::Object(Rc::new(Serializer { factory, f }))
}

fn ctor(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    let args: &[Arg] = args;
    if args.len() > 1 {
        return Err(crate::collections::ctor_not_implemented(types().serializer).into());
    }
    if !is_given(args.first()) {
        let (f, v) = platform_factory();
        return Ok(serializer_value(v, f));
    }
    let v = arg_value(ex, args.first())?;
    match factory_ref(&v) {
        Some(f) => Ok(serializer_value(v, f)),
        None => Err(type_mismatch(1).into()),
    }
}

struct WriteArgs {
    root: Option<QName>,
    explicit: bool,
}

impl Serializer {
    fn model(&self) -> &bsl_lib::xdto::Model {
        &self.f.model
    }

    fn write_args(&self, ex: &mut Execution, args: &[Arg], m: MemberId) -> CallResult<WriteArgs> {
        let mut names: Vec<String> = Vec::new();
        let mut explicit = false;
        for (i, a) in args.iter().enumerate().skip(2) {
            let x = arg_value(ex, Some(a))?;
            match (&x, as_enum_value(&x)) {
                (_, Some(e)) if e.type_idx() == crate::xml::types().assignment => {
                    explicit = e.name_en() == "Explicit";
                }
                (_, Some(e)) if e.type_idx() == types().form => {
                    if e.name_en() != "Element" {
                        return Err(not_implemented(types().serializer, m).into());
                    }
                }
                (Value::Undefined, _) => names.push(String::new()),
                (Value::String(s), _) => names.push(s.to_string_lossy()),
                _ => return Err(type_mismatch(i + 1).into()),
            }
        }
        let root = match names.as_slice() {
            [] => None,
            [n] if n.is_empty() => None,
            [n] => Some(QName::new("", n)),
            [n, u, ..] => Some(QName::new(u, n)),
        };
        Ok(WriteArgs { root, explicit })
    }

    fn data(&self, ex: &mut Execution, v: &Value, param: usize) -> CallResult<XdtoData> {
        let mut ctx = XdtoCtx::new(self.f.clone(), param);
        match ctx.to_data(ex, v) {
            Ok(d) => {
                if let XdtoData::Value { ty, .. } | XdtoData::Object { ty, .. } = &d
                    && ty.ns == super::value_map::NS_CORE
                    && self.model().type_def(ty).is_none()
                {
                    let e = ctx.mismatch(ex);
                    return Err(ctx.call_error(e));
                }
                Ok(d)
            }
            Err(e) => Err(ctx.call_error(e)),
        }
    }

    fn value(&self, ex: &mut Execution, d: &XdtoData, wire: bool) -> CallResult<Value> {
        let mut ctx = XdtoCtx::new(self.f.clone(), 1);
        if wire {
            ctx.wire_read();
        }
        match ctx.from_data(ex, d) {
            Ok(v) => Ok(v),
            Err(e) => Err(ctx.call_error(e)),
        }
    }

    fn write_xml(&self, ex: &mut Execution, args: &[Arg], m: MemberId) -> CallResult<Value> {
        let wv = arg_value(ex, args.first())?;
        let item = arg_value(ex, args.get(1))?;
        let a = self.write_args(ex, args, m)?;
        let mut st = xml_writer(ex, &wv)?;
        let d = self.data(ex, &item, 2)?;
        write_xml(
            ex,
            self.model(),
            &mut st,
            a.root.as_ref(),
            &d,
            XdtoWriteFlags {
                explicit_type: a.explicit,
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
        let d = self.data(ex, &item, 2)?;
        write_json(
            ex,
            self.model(),
            &wv,
            &d,
            XdtoWriteFlags {
                explicit_type: explicit,
                untyped: false,
            },
        )?;
        Ok(Value::Undefined)
    }

    fn expected(&self, ex: &mut Execution, a: Option<&Arg>) -> CallResult<Option<QName>> {
        match arg_value(ex, a)? {
            Value::Undefined => Ok(None),
            Value::Type(t) => Ok(Some(type_qname(ex, t))),
            _ => Err(type_mismatch(2).into()),
        }
    }

    fn read_xml(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let rv = arg_value(ex, args.first())?;
        let expect = self.expected(ex, args.get(1))?;
        let mut st = xml_reader(ex, &rv)?;
        let d = read_xml_mode(ex, self.model(), &mut st, expect.as_ref(), ReadMode::Values)?;
        self.value(ex, &d, true)
    }

    fn read_json(&self, ex: &mut Execution, args: &[Arg], m: MemberId) -> CallResult<Value> {
        if args.iter().skip(2).any(|a| is_given(Some(a))) {
            return Err(not_implemented(types().serializer, m).into());
        }
        let rv = arg_value(ex, args.first())?;
        let expect = self.expected(ex, args.get(1))?;
        drop(crate::json::json_reader(ex, &rv)?);
        let d = read_json_mode(ex, self.model(), &rv, expect.as_ref(), ReadMode::Values)?;
        self.value(ex, &d, true)
    }

    fn write_xdto(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let v = arg_value(ex, args.first())?;
        let d = match self.data(ex, &v, 1) {
            Err(bsl_vm::CallError::Host(e)) if matches!(&e.msg, bsl_vm::MsgRef::Key(k) if k.key == msg::TYPE_MISMATCH) =>
            {
                let name = type_qname(ex, v.type_id()).name;
                let detail = super::text_for(ex, msg::XDTO_MAPPING_MISSING, &[name]);
                return Err(super::headed(msg::XDTO_TYPE_MAPPING, &detail).into());
            }
            result => result?,
        };
        Ok(data_to_value(ex, &self.f, d)?)
    }

    fn read_xdto(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let v = arg_value(ex, args.first())?;
        let d = data_of(ex, &v, 1)?;
        if let XdtoData::Value { ty, .. } | XdtoData::Object { ty, .. } = &d
            && self.model().type_def(ty).is_none()
        {
            let detail = format!(
                "{}\n{}",
                super::text_for(ex, msg::XDTO_TYPE_NOT_FOUND, &[ty.clark()]),
                super::text_for(ex, msg::XDTO_UNKNOWN_PACKAGE, &[])
            );
            return Err(super::headed(msg::XDTO_TYPE_MISMATCH, &detail).into());
        }
        self.value(ex, &d, false)
    }

    fn can_read(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let rv = arg_value(ex, args.first())?;
        let st = xml_reader(ex, &rv)?;
        if !st.at_start() {
            return Ok(Value::Bool(false));
        }
        if st.is_nil() {
            return Ok(Value::Bool(true));
        }
        let m = self.model();
        let known = match st.xsi_type() {
            Some((ns, local)) => {
                let q = QName::new(&ns, &local);
                is_any(&q) || m.type_def(&q).is_some()
            }
            None => match st.node() {
                Some(bsl_lib::xml::XmlEvent::Start { ns, local, .. }) => {
                    m.type_def(&QName::new(ns, local)).is_some()
                        || m.package(ns)
                            .is_some_and(|p| p.globals.iter().any(|g| g.name == *local))
                }
                _ => false,
            },
        };
        Ok(Value::Bool(known))
    }

    fn get_type(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let rv = arg_value(ex, args.first())?;
        let st = xml_reader(ex, &rv)?;
        if let Some((ns, local)) = st.xsi_type() {
            return Ok(crate::values::new_xml_data_type(&ns, &local));
        }
        Ok(match st.node() {
            Some(bsl_lib::xml::XmlEvent::Start { ns, local, .. }) => {
                crate::values::new_xml_data_type(ns, local)
            }
            _ => Value::Undefined,
        })
    }
}

impl HostObject for Serializer {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(types().serializer)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let t = types().serializer;
        match property_name(t, m) {
            Some("Factory") => Ok(self.factory.clone()),
            _ => Err(not_implemented(t, m).into()),
        }
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        let t = types().serializer;
        match crate::collections::property_ru(t, m) {
            "" => Err(not_implemented(t, m).into()),
            ru => Err(HostError::key(crate::bind::msg::PROP_NOT_WRITABLE)
                .with_arg(ru)
                .raw()
                .into()),
        }
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        let t = types().serializer;
        match method_name(t, m) {
            Some("WriteXML") => self.write_xml(ex, args, m),
            Some("ReadXML") => self.read_xml(ex, args),
            Some("WriteJSON") => self.write_json(ex, args),
            Some("ReadJSON") => self.read_json(ex, args, m),
            Some("WriteXDTO") => self.write_xdto(ex, args),
            Some("ReadXDTO") => self.read_xdto(ex, args),
            Some("CanReadXML") => self.can_read(ex, args),
            Some("GetXMLType") => self.get_type(ex, args),
            Some("XMLString") => crate::convert::xml_string(ex, args),
            Some("XMLValue") => crate::convert::xml_value(ex, args),
            Some("XMLType") => crate::values::xml_type(ex, args),
            Some("XMLTypeOf") => crate::values::xml_type_of_value(ex, args),
            Some("FromXMLType") => crate::values::from_xml_type(ex, args),
            _ => Err(not_implemented(t, m).into()),
        }
    }

    fn presentation(&self, _ex: &mut Execution) -> CallResult<Option<Str>> {
        Ok(None)
    }
}

pub(super) fn register(reg: &mut Registry) {
    reg.type_impl(
        types().serializer,
        TypeImpl {
            ctor: Some(Arc::new(ctor)),
        },
    );
    reg.global_prop(
        "СериализаторXDTO",
        "XDTOSerializer",
        Arc::new(|_| {
            let (f, v) = platform_factory();
            Ok(serializer_value(v, f))
        }),
    );
}
