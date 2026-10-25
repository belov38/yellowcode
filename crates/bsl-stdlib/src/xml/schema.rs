use super::dom::dom_element;
use crate::bind::{arg_i64, arg_str, arg_value, msg, type_mismatch};
use crate::collections::{ctor_not_implemented, method_name, not_implemented, property_name};
use crate::enums::type_by_id;
use bsl_lib::xdto::{
    Fault, ModelFault, Package, XmlElement, check_package_facets, parse_xsd_with, platform_model,
};
use bsl_vm::{
    Arg, CallResult, Execution, HostError, HostObject, MemberId, Registry, TypeId, TypeIdx,
    TypeImpl, Value,
};
use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

struct Types {
    builder: TypeIdx,
    schema: TypeIdx,
    set: TypeIdx,
}

#[allow(
    clippy::panic,
    reason = "the embedded catalog has every type listed here"
)]
fn types() -> &'static Types {
    static T: OnceLock<Types> = OnceLock::new();
    T.get_or_init(|| {
        let t = |id: &str| type_by_id(id).unwrap_or_else(|| panic!("catalog type {id}"));
        Types {
            builder: t("XMLSchemaBuilder"),
            schema: t("XMLSchema"),
            set: t("XMLSchemaSet"),
        }
    })
}

enum Source {
    Element(Rc<XmlElement>),
    Package(Arc<Package>),
}

struct SchemaObject {
    ns: String,
    src: Source,
}

fn schema_of(v: &Value) -> Option<&SchemaObject> {
    v.as_object()?.as_any().downcast_ref::<SchemaObject>()
}

pub(crate) fn export_schema_set(p: Arc<Package>) -> Value {
    let s = Value::Object(Rc::new(SchemaObject {
        ns: p.ns.clone(),
        src: Source::Package(p),
    }));
    Value::Object(Rc::new(SetObject {
        schemas: RefCell::new(vec![s]),
    }))
}

pub(crate) fn schema_set_packages(ex: &mut Execution, set: &Value) -> CallResult<Vec<Package>> {
    let Some(s) = set
        .as_object()
        .and_then(|o| o.as_any().downcast_ref::<SetObject>())
    else {
        return Err(type_mismatch(1).into());
    };
    let schemas: Vec<Value> = s.schemas.borrow().clone();
    let mut done: Vec<Package> = Vec::new();
    let mut pending: Vec<Rc<XmlElement>> = Vec::new();
    for v in &schemas {
        match schema_of(v).map(|x| &x.src) {
            Some(Source::Package(p)) => done.push((**p).clone()),
            Some(Source::Element(e)) => pending.push(e.clone()),
            None => {}
        }
    }
    while !pending.is_empty() {
        ex.charge(pending.len() as u64)?;
        let mut left = Vec::new();
        let mut last_err = None;
        let before = pending.len();
        for e in pending {
            let refs: Vec<&Package> = done.iter().collect();
            match parse_xsd_with(&e, &refs) {
                Ok(p) => done.push(p),
                Err(err) => {
                    last_err = Some(err);
                    left.push(e);
                }
            }
        }
        if left.len() == before {
            let Some(err) = last_err else {
                return Err(type_mismatch(1).into());
            };
            return Err(parse_error(ex, err).into());
        }
        pending = left;
    }
    let model = platform_model().with(done.clone());
    for p in &done {
        if let Err(f) = check_package_facets(&model, &p.ns) {
            return Err(model_error(ex, &f, &p.ns).into());
        }
    }
    Ok(done)
}

fn parse_error(ex: &Execution, err: bsl_lib::xdto::XdtoParseError) -> HostError {
    use crate::xdto::text_for;
    match err.fault {
        Some(Fault::Pattern {
            ty,
            pattern,
            reason,
        }) => {
            let lines = format!(
                "{}\n{}",
                text_for(ex, msg::XDTO_VALUE_TYPE, &[pattern.clone(), ty.clark()]),
                text_for(
                    ex,
                    msg::XDTO_FACET_MISMATCH,
                    &["Pattern".to_string(), pattern]
                ),
            );
            let reason = match reason.as_str() {
                "unterminated class" => "xmlFAParseCharClass: ']' expected",
                other => other,
            };
            let text = format!("failed to compile: {reason}\n");
            crate::bind::with_other(crate::xdto::headed(msg::XDTO_VALIDATION, &lines), true)
                .caused_by(bsl_vm::Cause::Host(Box::new(HostError::text(&text, &text))))
        }
        None => {
            let m = err.message;
            crate::bind::with_other(HostError::text(&m, &m), false)
        }
    }
}

fn model_error(ex: &Execution, f: &ModelFault, ns: &str) -> HostError {
    use crate::xdto::text_for;
    let (key, nargs) = match f.code {
        "xdto-facet-7.1" => (msg::XDTO_FACET_7_1, 2),
        _ => (msg::XDTO_VALUETYPE_5_1_3, 2),
    };
    let detail = text_for(ex, key, &f.args[..nargs.min(f.args.len())]);
    let context = format!(
        "{}\t{}",
        text_for(ex, msg::XDTO_CONTEXT_TYPE, std::slice::from_ref(&f.ty.name)),
        text_for(ex, msg::XDTO_CONTEXT_PACKAGE, &[ns.to_string()]),
    );
    crate::bind::with_other(
        crate::bind::key_error(
            msg::XDTO_MODEL_ERROR,
            vec![f.code.to_string(), context, detail],
            String::new(),
        ),
        true,
    )
}

struct BuilderObject;

impl HostObject for BuilderObject {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(types().builder)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        Err(not_implemented(types().builder, m).into())
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        Err(not_implemented(types().builder, m).into())
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        let t = types().builder;
        match method_name(t, m) {
            Some("CreateXMLSchema") => {
                let v = arg_value(ex, args.first())?;
                let el = dom_element(&v).ok_or_else(|| type_mismatch(1))?;
                Ok(Value::Object(Rc::new(SchemaObject {
                    ns: el.attr("targetNamespace").unwrap_or("").to_string(),
                    src: Source::Element(el),
                })))
            }
            _ => Err(not_implemented(t, m).into()),
        }
    }
}

impl HostObject for SchemaObject {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(types().schema)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let t = types().schema;
        match property_name(t, m) {
            Some("TargetNamespace") => Ok(Value::from(self.ns.as_str())),
            _ => Err(not_implemented(t, m).into()),
        }
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        Err(not_implemented(types().schema, m).into())
    }

    fn call(&self, _ex: &mut Execution, m: MemberId, _args: &mut [Arg]) -> CallResult<Value> {
        Err(not_implemented(types().schema, m).into())
    }
}

struct SetObject {
    schemas: RefCell<Vec<Value>>,
}

impl HostObject for SetObject {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(types().set)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        Err(not_implemented(types().set, m).into())
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        Err(not_implemented(types().set, m).into())
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        let t = types().set;
        match method_name(t, m) {
            Some("Add") => {
                let v = arg_value(ex, args.first())?;
                if schema_of(&v).is_none() {
                    return Err(type_mismatch(1).into());
                }
                self.schemas.borrow_mut().push(v);
                Ok(Value::Undefined)
            }
            Some("Count") => Ok(Value::from(self.schemas.borrow().len() as i64)),
            Some("Get") => {
                let v = arg_value(ex, args.first())?;
                let schemas = self.schemas.borrow();
                if let Value::Number(_) = v {
                    let i = arg_i64(ex, args.first(), 1)?;
                    return usize::try_from(i)
                        .ok()
                        .and_then(|i| schemas.get(i).cloned())
                        .ok_or_else(|| HostError::key(msg::RANGE).into());
                }
                let ns = arg_str(ex, args.first())?.to_string_lossy();
                Ok(schemas
                    .iter()
                    .find(|s| schema_of(s).is_some_and(|x| x.ns == ns))
                    .cloned()
                    .unwrap_or(Value::Undefined))
            }
            _ => Err(not_implemented(t, m).into()),
        }
    }

    fn iter_len(&self, _ex: &mut Execution) -> Option<usize> {
        Some(self.schemas.borrow().len())
    }

    fn iter_get(&self, _ex: &mut Execution, i: usize) -> CallResult<Value> {
        self.schemas
            .borrow()
            .get(i)
            .cloned()
            .ok_or_else(|| HostError::key(msg::RANGE).into())
    }
}

fn builder_ctor(_ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    if !args.is_empty() {
        return Err(ctor_not_implemented(types().builder).into());
    }
    Ok(Value::Object(Rc::new(BuilderObject)))
}

fn set_ctor(_ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    if !args.is_empty() {
        return Err(ctor_not_implemented(types().set).into());
    }
    Ok(Value::Object(Rc::new(SetObject {
        schemas: RefCell::new(Vec::new()),
    })))
}

pub(super) fn register(reg: &mut Registry) {
    reg.type_impl(
        types().builder,
        TypeImpl {
            ctor: Some(Arc::new(builder_ctor)),
        },
    );
    reg.type_impl(
        types().set,
        TypeImpl {
            ctor: Some(Arc::new(set_ctor)),
        },
    );
}
