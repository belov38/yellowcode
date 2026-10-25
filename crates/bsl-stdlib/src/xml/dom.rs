use super::xml_reader;
use crate::bind::{arg_value, type_mismatch};
use crate::collections::{ctor_not_implemented, method_name, not_implemented, property_name};
use crate::enums::type_by_id;
use bsl_lib::xdto::XmlElement;
use bsl_vm::{
    Arg, CallResult, Execution, HostObject, MemberId, Registry, TypeId, TypeIdx, TypeImpl, Value,
};
use std::any::Any;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

struct Types {
    builder: TypeIdx,
    document: TypeIdx,
    element: TypeIdx,
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
            builder: t("DOMBuilder"),
            document: t("DOMDocument"),
            element: t("DOMElement"),
        }
    })
}

pub(crate) fn dom_element(v: &Value) -> Option<Rc<XmlElement>> {
    v.as_object()?
        .as_any()
        .downcast_ref::<ElementObject>()
        .map(|e| e.el.clone())
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
            Some("Read") => {
                let rv = arg_value(ex, args.first())?;
                let mut st = xml_reader(ex, &rv).map_err(|_| type_mismatch(1))?;
                let root = st.read_tree(ex)?;
                Ok(Value::Object(Rc::new(DocumentObject {
                    root: Rc::new(root),
                })))
            }
            _ => Err(not_implemented(t, m).into()),
        }
    }
}

struct DocumentObject {
    root: Rc<XmlElement>,
}

impl HostObject for DocumentObject {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(types().document)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let t = types().document;
        match property_name(t, m) {
            Some("DocumentElement") => Ok(Value::Object(Rc::new(ElementObject {
                el: self.root.clone(),
            }))),
            _ => Err(not_implemented(t, m).into()),
        }
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        Err(not_implemented(types().document, m).into())
    }

    fn call(&self, _ex: &mut Execution, m: MemberId, _args: &mut [Arg]) -> CallResult<Value> {
        Err(not_implemented(types().document, m).into())
    }
}

struct ElementObject {
    el: Rc<XmlElement>,
}

impl HostObject for ElementObject {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(types().element)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let t = types().element;
        match property_name(t, m) {
            Some("LocalName") => Ok(Value::from(self.el.local.as_str())),
            Some("NamespaceURI") => Ok(Value::from(self.el.ns.as_str())),
            _ => Err(not_implemented(t, m).into()),
        }
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        Err(not_implemented(types().element, m).into())
    }

    fn call(&self, _ex: &mut Execution, m: MemberId, _args: &mut [Arg]) -> CallResult<Value> {
        Err(not_implemented(types().element, m).into())
    }
}

fn builder_ctor(_ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    if !args.is_empty() {
        return Err(ctor_not_implemented(types().builder).into());
    }
    Ok(Value::Object(Rc::new(BuilderObject)))
}

pub(super) fn register(reg: &mut Registry) {
    reg.type_impl(
        types().builder,
        TypeImpl {
            ctor: Some(Arc::new(builder_ctor)),
        },
    );
}
