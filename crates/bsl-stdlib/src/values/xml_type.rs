use crate::bind::{arg_str, arg_value, type_mismatch};
use crate::collections::{
    ctor_not_implemented, not_implemented, property_name, property_ru, types,
};
use crate::enums::type_by_id;
use crate::serial::storage::is_storage_type;
use crate::xml::{NS_DATA, NS_XS};
use bsl_prim::Str;
use bsl_vm::{
    Arg, CallResult, Execution, HostError, HostObject, MemberId, Registry, TypeId, TypeImpl, Value,
};
use platform_catalog::TypeIdx;
use std::any::Any;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

#[allow(
    clippy::panic,
    reason = "the embedded catalog has every type listed here"
)]
fn data_type() -> TypeIdx {
    static T: OnceLock<TypeIdx> = OnceLock::new();
    *T.get_or_init(|| {
        type_by_id("XMLDataType").unwrap_or_else(|| panic!("catalog type XMLDataType"))
    })
}

pub(crate) struct XmlDataType {
    pub(crate) name: String,
    pub(crate) uri: String,
}

impl XmlDataType {
    pub(crate) fn of(v: &Value) -> Option<&XmlDataType> {
        v.as_object()?.as_any().downcast_ref::<XmlDataType>()
    }
}

pub(crate) fn new_xml_data_type(uri: &str, name: &str) -> Value {
    Value::Object(Rc::new(XmlDataType {
        name: name.to_string(),
        uri: uri.to_string(),
    }))
}

pub(crate) fn comparison_input_len(value: &Value) -> Option<u64> {
    let ty = value.as_object()?.as_any().downcast_ref::<XmlDataType>()?;
    Some((ty.name.len() as u64).saturating_add(ty.uri.len() as u64))
}

impl HostObject for XmlDataType {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(data_type())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<Str>> {
        Ok(Some(crate::collections::type_name(ex, data_type())))
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let t = data_type();
        match property_name(t, m) {
            Some("TypeName") => Ok(Value::from(self.name.as_str())),
            Some("NamespaceURI") => Ok(Value::from(self.uri.as_str())),
            _ => Err(not_implemented(t, m).into()),
        }
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        let t = data_type();
        match property_name(t, m) {
            Some(_) => Err(HostError::key(crate::bind::msg::PROP_NOT_WRITABLE)
                .with_arg(property_ru(t, m))
                .raw()
                .into()),
            None => Err(not_implemented(t, m).into()),
        }
    }

    fn call(&self, _ex: &mut Execution, m: MemberId, _args: &mut [Arg]) -> CallResult<Value> {
        Err(not_implemented(data_type(), m).into())
    }

    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        let o = other.as_any().downcast_ref::<XmlDataType>()?;
        Some(o.name == self.name && o.uri == self.uri)
    }
}

fn ctor(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    if args.len() != 2 {
        return Err(ctor_not_implemented(data_type()).into());
    }
    let name = arg_str(ex, args.first())?.to_string_lossy();
    let uri = arg_str(ex, args.get(1))?.to_string_lossy();
    Ok(new_xml_data_type(&uri, &name))
}

fn applied_name(ex: &Execution, t: TypeId) -> Option<String> {
    ex.runtime().types().app_type(t)?;
    let app = ex.runtime().app();
    app.serializer()?.xdto_type(t).map(|x| x.name)
}

pub(crate) fn xml_type_of(ex: &Execution, t: TypeId) -> Option<(String, String)> {
    let xs = |n: &str| Some((NS_XS.to_string(), n.to_string()));
    let data = |n: &str| Some((NS_DATA.to_string(), n.to_string()));
    match t {
        TypeId::NUMBER => xs("decimal"),
        TypeId::STRING => xs("string"),
        TypeId::DATE => xs("dateTime"),
        TypeId::BOOL => xs("boolean"),
        TypeId::NULL => data("Null"),
        TypeId::TYPE => data("Type"),
        _ if t == TypeId::of_catalog(types().binary) => xs("base64Binary"),
        _ if t == TypeId::of_catalog(types().uuid) => data("UUID"),
        _ if t == TypeId::of_catalog(crate::typedesc::idx("TypeDescription")) => {
            data("TypeDescription")
        }
        _ if is_storage_type(t) => data("ValueStorage"),
        _ => applied_name(ex, t).map(|n| (String::new(), n)),
    }
}

pub(crate) fn type_from_xml(ex: &Execution, uri: &str, name: &str) -> Option<TypeId> {
    match (uri, name) {
        (NS_XS, "decimal") => Some(TypeId::NUMBER),
        (NS_XS, "string") => Some(TypeId::STRING),
        (NS_XS, "dateTime") => Some(TypeId::DATE),
        (NS_XS, "boolean") => Some(TypeId::BOOL),
        (NS_XS, "base64Binary") => Some(TypeId::of_catalog(types().binary)),
        (NS_DATA, "Null") => Some(TypeId::NULL),
        (NS_DATA, "Type") => Some(TypeId::TYPE),
        (NS_DATA, "UUID") => Some(TypeId::of_catalog(types().uuid)),
        (NS_DATA, "TypeDescription") => {
            Some(TypeId::of_catalog(crate::typedesc::idx("TypeDescription")))
        }
        (NS_DATA, "ValueStorage") => type_by_id("ValueStorage").map(TypeId::of_catalog),
        ("", _) => ex
            .runtime()
            .types()
            .by_name(name)
            .filter(|t| applied_name(ex, *t).is_some_and(|n| n == name)),
        _ => None,
    }
}

fn xml_type_value(ex: &Execution, t: TypeId) -> Value {
    match xml_type_of(ex, t) {
        Some((uri, name)) => new_xml_data_type(&uri, &name),
        None => Value::Undefined,
    }
}

pub(crate) fn xml_type(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    match arg_value(ex, args.first())? {
        Value::Type(t) => Ok(xml_type_value(ex, t)),
        _ => Err(type_mismatch(1).into()),
    }
}

pub(crate) fn xml_type_of_value(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    let v = arg_value(ex, args.first())?;
    Ok(xml_type_value(ex, v.type_id()))
}

pub(crate) fn from_xml_type(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    let first = arg_value(ex, args.first())?;
    let (name, uri) = match XmlDataType::of(&first) {
        Some(d) => (d.name.clone(), d.uri.clone()),
        None if args.len() >= 2 && matches!(first, Value::String(_)) => (
            arg_str(ex, args.first())?.to_string_lossy(),
            arg_str(ex, args.get(1))?.to_string_lossy(),
        ),
        None => return Err(type_mismatch(1).into()),
    };
    Ok(match type_from_xml(ex, &uri, &name) {
        Some(t) => Value::Type(t),
        None => Value::Undefined,
    })
}

pub(crate) fn register(reg: &mut Registry) {
    reg.type_impl(
        data_type(),
        TypeImpl {
            ctor: Some(Arc::new(ctor)),
        },
    );
    crate::bind::global(reg, "XMLТип", "XMLType", xml_type);
    crate::bind::global(reg, "XMLТипЗнч", "XMLTypeOf", xml_type_of_value);
    crate::bind::global(reg, "ИзXMLТипа", "FromXMLType", from_xml_type);
}
