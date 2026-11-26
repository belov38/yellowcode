use super::data::{Factory, FactoryRef, XdtoData, XdtoWriteFlags, data_to_value};
use super::factory::{data_of, explicit_arg, factory_ref, is_platform_model, platform_factory};
use super::io_json::{read_json_mode, read_serializer_json, write_serializer_json};
use super::io_xml::{ReadMode, read_serializer_xml, write_serializer_xml};
use super::value_map::{XdtoCtx, type_qname};
use super::{is_any, types};
use crate::bind::{arg_value, is_given, msg, type_mismatch};
use crate::collections::{method_name, not_implemented, property_name};
use crate::enums::as_enum_value;
use crate::xml::{xml_reader, xml_writer};
use bsl_lib::xdto::{
    Facets, Form, ObjectType, Package, Property, QName, TypeDef, ValueType, Variety,
};
use bsl_prim::Str;
use bsl_vm::host::AppXdtoTypeDef;
use bsl_vm::{
    Arg, CallResult, Execution, HostError, HostObject, MemberId, Registry, TypeId, TypeImpl, Value,
};
use platform_catalog::XdtoName;
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

    fn write_factory(
        &self,
        ex: &mut Execution,
        item: &Value,
        param: usize,
    ) -> CallResult<FactoryRef> {
        if !is_platform_model(&self.f.model) {
            return Ok(self.f.clone());
        }
        let Some(o) = item.as_object() else {
            return Ok(self.f.clone());
        };
        let Some(root) = ex
            .runtime()
            .types()
            .app_type(HostObject::type_id(&**o))
            .and_then(|a| a.xdto.clone())
        else {
            return Ok(self.f.clone());
        };
        if self
            .model()
            .type_def(&QName::new(&root.namespace, &root.name))
            .is_some()
        {
            return Ok(self.f.clone());
        }
        let app = ex.runtime().app().clone();
        let Some(serializer) = app.serializer() else {
            return Ok(self.f.clone());
        };
        let Some(defs) = serializer.xdto_types(ex, o, &root)? else {
            return Ok(self.f.clone());
        };
        let types = self
            .model()
            .packages
            .iter()
            .fold(0u64, |n, p| n.saturating_add(p.types.len() as u64));
        ex.charge(types)?;
        ex.reserve(
            package_bytes(&defs)
                .saturating_add(types.saturating_mul(std::mem::size_of::<usize>() as u64)),
        )?;
        let package = host_package(&root, defs).ok_or_else(|| type_mismatch(param))?;
        Ok(Rc::new(Factory::new(Arc::new(
            self.model().with(vec![package]),
        ))))
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "writer accounting is scoped to this invocation"
    )]
    fn data(
        &self,
        ex: &mut Execution,
        f: &FactoryRef,
        v: &Value,
        param: usize,
        xml_write: bool,
        json_write: bool,
        json_prefix: u64,
        xml_table_writer: Option<&Value>,
    ) -> CallResult<(XdtoData, u64)> {
        let mut ctx = XdtoCtx::new(f.clone(), param);
        ctx.xml_write(xml_write);
        ctx.json_write(json_write, json_prefix);
        if let Some(writer) = xml_table_writer {
            ctx.xml_prefix(json_prefix, writer);
        }
        match ctx.to_data(ex, v) {
            Ok(d) => {
                if let XdtoData::Value { ty, .. } | XdtoData::Object { ty, .. } = &d
                    && ty.ns == super::value_map::NS_CORE
                    && f.model.type_def(ty).is_none()
                {
                    let e = ctx.mismatch(ex);
                    return Err(ctx.call_error(e));
                }
                let live = if xml_table_writer.is_some() {
                    ctx.output_scratch()
                } else {
                    ctx.read_scratch(0).saturating_sub(json_prefix)
                };
                Ok((d, live))
            }
            Err(e) => Err(ctx.call_error(e)),
        }
    }

    fn value(
        &self,
        ex: &mut Execution,
        d: &XdtoData,
        wire: bool,
        xml: bool,
        json_input: Option<(Value, u64)>,
    ) -> CallResult<Value> {
        let mut ctx = XdtoCtx::new(self.f.clone(), 1);
        if wire {
            ctx.wire_read();
        }
        ctx.xml_read(xml && is_platform_model(&self.f.model));
        if let Some((reader, projection)) = json_input {
            ctx.json_read(reader, projection);
        }
        match ctx.from_data(ex, d) {
            Ok(v) => Ok(v),
            Err(e) => Err(ctx.call_error(e)),
        }
    }

    fn value_xml_table(
        &self,
        ex: &mut Execution,
        d: &XdtoData,
        reader: Value,
    ) -> CallResult<Value> {
        let projection = crate::serial::valuetable_xml_reader::projection_bytes(ex, d)?;
        let source =
            crate::xml::reader_live_bytes(ex, &reader)?.ok_or_else(|| ex.out_of_memory())?;
        ex.reserve(
            projection
                .checked_add(source)
                .ok_or_else(|| ex.out_of_memory())?,
        )?;
        let mut ctx = XdtoCtx::new(self.f.clone(), 1);
        ctx.wire_read();
        ctx.xml_read(true);
        ctx.xml_table_read(reader, projection);
        match ctx.read_xml_table(ex, d) {
            Ok(v) => Ok(v),
            Err(e) => Err(ctx.call_error(e)),
        }
    }

    fn write_xml(&self, ex: &mut Execution, args: &[Arg], m: MemberId) -> CallResult<Value> {
        let wv = arg_value(ex, args.first())?;
        let item = arg_value(ex, args.get(1))?;
        let a = self.write_args(ex, args, m)?;
        let table = item
            .as_object()
            .is_some_and(|o| o.as_any().is::<crate::collections::ValueTable>());
        let prefix = {
            let st = xml_writer(ex, &wv)?;
            if table {
                st.retained_prefix_bytes(ex)?
            } else {
                0
            }
        };
        let f = self.write_factory(ex, &item, 2)?;
        let (d, live) = self.data(ex, &f, &item, 2, true, false, prefix, table.then_some(&wv))?;
        let mut st = xml_writer(ex, &wv)?;
        if table {
            let current = st.retained_prefix_bytes(ex)?;
            ex.reserve(current.saturating_add(live))?;
        }
        write_serializer_xml(
            ex,
            &f.model,
            &mut st,
            a.root.as_ref(),
            &d,
            XdtoWriteFlags {
                explicit_type: a.explicit,
                untyped: false,
            },
            table,
        )?;
        Ok(Value::Undefined)
    }

    fn write_json(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let wv = arg_value(ex, args.first())?;
        let item = arg_value(ex, args.get(1))?;
        let explicit = explicit_arg(ex, args.get(2))?;
        drop(crate::json::json_writer(ex, &wv)?);
        let platform = is_platform_model(&self.f.model);
        let prefix = if platform {
            crate::json::json_prefix_bytes(&wv)?
        } else {
            0
        };
        let (d, live) = self.data(ex, &self.f, &item, 2, false, platform, prefix, None)?;
        let live = if platform {
            crate::serial::valuelist_json::data_envelope(ex, &item, &d, live, prefix)?
        } else {
            None
        };
        write_serializer_json(
            ex,
            self.model(),
            &wv,
            &d,
            live.is_some(),
            live.unwrap_or(0),
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
        let platform = is_platform_model(&self.f.model);
        let mut st = xml_reader(ex, &rv)?;
        let d = read_serializer_xml(ex, self.model(), &mut st, expect.as_ref(), platform)?;
        if platform
            && crate::serial::valuetable_xml_reader::is_root(&d)
            && !super::value_map::has_codec(&super::value_map::core("ValueTable"))
        {
            drop(st);
            return self.value_xml_table(ex, &d, rv);
        }
        self.value(ex, &d, true, true, None)
    }

    fn read_json(&self, ex: &mut Execution, args: &[Arg], m: MemberId) -> CallResult<Value> {
        if args.iter().skip(2).any(|a| is_given(Some(a))) {
            return Err(not_implemented(types().serializer, m).into());
        }
        let rv = arg_value(ex, args.first())?;
        let expect = self.expected(ex, args.get(1))?;
        drop(crate::json::json_reader(ex, &rv)?);
        if is_platform_model(&self.f.model) {
            let (d, projection) = read_serializer_json(ex, self.model(), &rv, expect.as_ref())?;
            self.value(ex, &d, true, false, Some((rv, projection)))
        } else {
            let d = read_json_mode(ex, self.model(), &rv, expect.as_ref(), ReadMode::Values)?;
            self.value(ex, &d, true, false, None)
        }
    }

    fn write_xdto(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let v = arg_value(ex, args.first())?;
        let f = self.write_factory(ex, &v, 1)?;
        let (d, _) = match self.data(ex, &f, &v, 1, false, false, 0, None) {
            Err(bsl_vm::CallError::Host(e)) if matches!(&e.msg, bsl_vm::MsgRef::Key(k) if k.key == msg::TYPE_MISMATCH) =>
            {
                let name = type_qname(ex, v.type_id()).name;
                let detail = super::text_for(ex, msg::XDTO_MAPPING_MISSING, &[name]);
                return Err(super::headed(msg::XDTO_TYPE_MAPPING, &detail).into());
            }
            result => result?,
        };
        Ok(data_to_value(ex, &f, d)?)
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
        self.value(ex, &d, false, false, None)
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

fn package_bytes(defs: &[AppXdtoTypeDef]) -> u64 {
    let name = |n: &XdtoName| (n.namespace.len() + n.name.len()) as u64;
    defs.iter().fold(0u64, |bytes, def| {
        bytes.saturating_add(match def {
            AppXdtoTypeDef::Object {
                name: n,
                properties,
            } => properties.iter().fold(
                name(n).saturating_add(std::mem::size_of::<TypeDef>() as u64),
                |b, p| {
                    b.saturating_add(p.name.len() as u64)
                        .saturating_add(name(&p.ty))
                        .saturating_add(std::mem::size_of::<Property>() as u64)
                },
            ),
            AppXdtoTypeDef::Value { name: n, base } => name(n)
                .saturating_add(name(base))
                .saturating_add(std::mem::size_of::<TypeDef>() as u64),
        })
    })
}

fn host_package(root: &XdtoName, defs: Vec<AppXdtoTypeDef>) -> Option<Package> {
    let qname = |n: &XdtoName| QName::new(&n.namespace, &n.name);
    match defs.first() {
        Some(AppXdtoTypeDef::Object { name, .. }) if name == root => {}
        _ => return None,
    }
    let mut types: Vec<TypeDef> = Vec::with_capacity(defs.len());
    for def in defs {
        let name = match &def {
            AppXdtoTypeDef::Object { name, .. } | AppXdtoTypeDef::Value { name, .. } => name,
        };
        if name.namespace != root.namespace || types.iter().any(|t| t.name().name == name.name) {
            return None;
        }
        types.push(match def {
            AppXdtoTypeDef::Object { name, properties } => TypeDef::Object(ObjectType {
                name: qname(&name),
                base: None,
                open: false,
                abstract_: false,
                mixed: false,
                sequenced: false,
                ordered: true,
                properties: properties
                    .into_iter()
                    .map(|p| Property {
                        name: p.name,
                        ty: qname(&p.ty),
                        lower: p.lower,
                        upper: p.upper,
                        nillable: p.nillable,
                        form: Form::Element,
                        qualified: true,
                        default: None,
                        local_name: None,
                        fixed: None,
                    })
                    .collect(),
            }),
            AppXdtoTypeDef::Value { name, base } => TypeDef::Value(ValueType {
                name: qname(&name),
                base: Some(qname(&base)),
                variety: Variety::Atomic,
                facets: Facets::default(),
            }),
        });
    }
    Some(Package {
        ns: root.namespace.clone(),
        element_qualified: true,
        attribute_qualified: false,
        types,
        anonymous_types: Vec::new(),
        globals: Vec::new(),
    })
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
