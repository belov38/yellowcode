use crate::bind::{arg_value, global, msg, type_mismatch};
use crate::enums::as_enum_value;
use crate::values::{new_xml_data_type, type_from_xml, xml_type_of};
use crate::xml::{
    NS_DATA, NS_XS, NS_XSI, XmlReaderState, XmlWriterState, split_qname, xml_error, xml_reader,
    xml_writer,
};
use bsl_lib::xml::XmlEvent;
use bsl_prim::Str;
use bsl_vm::{Arg, CallResult, Execution, TypeId, Value};

pub(crate) fn register(reg: &mut bsl_vm::Registry) {
    global(reg, "ЗаписатьXML", "WriteXML", write_xml);
    global(reg, "ПрочитатьXML", "ReadXML", read_xml);
    global(reg, "ВозможностьЧтенияXML", "CanReadXML", can_read);
    global(reg, "ПолучитьXMLТип", "GetXMLType", get_type);
}

fn not_serializable() -> bsl_vm::CallError {
    xml_error(msg::XML_NOT_SERIALIZABLE).into()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
    Schema,
    Data,
    Applied,
}

struct Plan {
    family: Family,
    name: String,
    text: String,
    nil: bool,
    type_value: Option<(String, String)>,
}

fn plan(ex: &mut Execution, v: &Value) -> CallResult<Plan> {
    if matches!(v, Value::Undefined) {
        return Ok(Plan {
            family: Family::Data,
            name: "Undefined".into(),
            text: String::new(),
            nil: true,
            type_value: None,
        });
    }
    if let Value::Type(t) = v {
        let named = xml_type_of(ex, *t).ok_or_else(not_serializable)?;
        return Ok(Plan {
            family: Family::Data,
            name: "Type".into(),
            text: String::new(),
            nil: false,
            type_value: Some(named),
        });
    }
    let (ns, name) = xml_type_of(ex, v.type_id()).ok_or_else(not_serializable)?;
    if let Value::String(s) = v {
        ex.reserve((s.len() as u64).saturating_mul(3))?;
    }
    let family = match ns.as_str() {
        NS_XS => Family::Schema,
        NS_DATA => Family::Data,
        _ => Family::Applied,
    };
    let text = if matches!(v, Value::Object(_)) {
        crate::serial::xmlvalue::xml_string(ex, v)?.ok_or_else(not_serializable)?
    } else {
        crate::serial::xmlvalue::to_xml_string(ex, v)?
    };
    Ok(Plan {
        family,
        name,
        text,
        nil: false,
        type_value: None,
    })
}

fn ensure_prefix(
    st: &mut XmlWriterState,
    ex: &mut Execution,
    uri: &str,
    preferred: &str,
) -> CallResult<String> {
    if let Some(p) = st.prefix_of(uri) {
        return Ok(p.to_string());
    }
    st.namespace(ex, preferred, uri)?;
    Ok(preferred.to_string())
}

fn qualified(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}:{name}")
    }
}

fn emit(
    st: &mut XmlWriterState,
    ex: &mut Execution,
    p: &Plan,
    custom: Option<(&str, &str)>,
    explicit: bool,
) -> CallResult<()> {
    let d = st.depth() + 1;
    let gen_prefix = format!("d{d}p1");
    let in_data = custom.is_none() && p.family == Family::Data;
    match custom {
        Some((name, "")) if name.contains(':') => st.start_element_full(ex, name)?,
        Some((name, uri)) => st.start_element(ex, name, (!uri.is_empty()).then_some(uri))?,
        None if in_data && explicit => st.start_element(ex, &p.name, Some(NS_DATA))?,
        None if in_data => st.start_element_prefixed(ex, &p.name, NS_DATA, &gen_prefix)?,
        None => st.start_element(ex, &p.name, None)?,
    }
    let text = match &p.type_value {
        Some((ns, name)) => match ns.as_str() {
            "" => name.clone(),
            NS_XS => qualified(&ensure_prefix(st, ex, NS_XS, "xs")?, name),
            uri => qualified(&ensure_prefix(st, ex, uri, &gen_prefix)?, name),
        },
        None => p.text.clone(),
    };
    if explicit && !p.nil {
        let type_ref = match p.family {
            Family::Schema => qualified(&ensure_prefix(st, ex, NS_XS, "xs")?, &p.name),
            Family::Data => qualified(&ensure_prefix(st, ex, NS_DATA, &gen_prefix)?, &p.name),
            Family::Applied => p.name.clone(),
        };
        ensure_prefix(st, ex, NS_XSI, "xsi")?;
        st.attribute(ex, "type", Some(NS_XSI), &type_ref)?;
    }
    if p.nil {
        ensure_prefix(st, ex, NS_XSI, "xsi")?;
        st.attribute(ex, "nil", Some(NS_XSI), "true")?;
    }
    if !text.is_empty() {
        st.text(ex, &text)?;
    }
    st.end_element(ex)
}

fn write_xml(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    let w = arg_value(ex, args.first())?;
    let v = arg_value(ex, args.get(1))?;
    let mut names: Vec<String> = Vec::new();
    let mut explicit = false;
    for (i, a) in args.iter().enumerate().skip(2) {
        let x = arg_value(ex, Some(a))?;
        match (&x, as_enum_value(&x)) {
            (_, Some(e)) if e.type_idx() == crate::xml::types().assignment => {
                explicit = e.name_en() == "Explicit";
            }
            (Value::Undefined, _) => names.push(String::new()),
            (Value::String(s), _) => names.push(s.to_string_lossy()),
            _ => return Err(type_mismatch(i + 1).into()),
        }
    }
    let mut st = xml_writer(ex, &w)?;
    let p = plan(ex, &v)?;
    crate::xml::check_chars(&p.text)?;
    let custom = match names.as_slice() {
        [] => None,
        [n] if n.is_empty() => None,
        [n] => Some((n.as_str(), "")),
        [n, u, ..] => Some((n.as_str(), u.as_str())),
    };
    emit(&mut st, ex, &p, custom, explicit)?;
    Ok(Value::Undefined)
}

fn element_type(ex: &Execution, st: &XmlReaderState) -> Option<TypeId> {
    if let Some((ns, local)) = st.xsi_type() {
        return by_name(ex, &ns, &local, true);
    }
    match st.node()? {
        XmlEvent::Start { ns, local, .. } => by_name(ex, ns, local, false),
        _ => None,
    }
}

fn by_name(ex: &Execution, ns: &str, local: &str, via_xsi: bool) -> Option<TypeId> {
    match (ns, local) {
        (NS_DATA, "Undefined") => Some(TypeId::UNDEFINED),
        (NS_XS, _) if !via_xsi => None,
        ("", "decimal" | "string" | "dateTime" | "boolean" | "base64Binary") => {
            type_from_xml(ex, NS_XS, local)
        }
        _ => type_from_xml(ex, ns, local),
    }
}

fn readable_type(ex: &Execution, t: TypeId) -> bool {
    t == TypeId::UNDEFINED || xml_type_of(ex, t).is_some()
}

fn read_value(ex: &mut Execution, st: &mut XmlReaderState, t: TypeId) -> CallResult<Value> {
    if st.is_nil() {
        st.read_simple(ex, false)?;
        return Ok(Value::Undefined);
    }
    if t == TypeId::TYPE {
        let s = st.read_simple(ex, true)?;
        let text = s.text.trim();
        let (prefix, local) = split_qname(text).ok_or_else(|| xml_error(msg::XML_CONVERSION))?;
        let ns = match (prefix, s.prefix_ns) {
            ("", ns) => ns.unwrap_or_default(),
            (_, Some(ns)) => ns,
            (_, None) => return Err(xml_error(msg::XML_CONVERSION).into()),
        };
        return match type_from_xml(ex, &ns, local) {
            Some(t) => Ok(Value::Type(t)),
            None => Err(xml_error(msg::XML_CONVERSION).into()),
        };
    }
    let s = st.read_simple(ex, false)?;
    if t == TypeId::UNDEFINED {
        return Ok(Value::Undefined);
    }
    ex.reserve((s.text.len() as u64).saturating_mul(2))?;
    crate::convert::xml_from_text(ex, t, &Str::from(s.text.as_str()))
}

fn read_xml(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    let r = arg_value(ex, args.first())?;
    let want = match arg_value(ex, args.get(1))? {
        Value::Undefined => None,
        Value::Type(t) => Some(t),
        _ => return Err(type_mismatch(2).into()),
    };
    let mut st = xml_reader(ex, &r)?;
    if st.node().is_none() {
        st.move_to_content(ex, false)?;
    }
    if !st.at_start() {
        return Err(xml_error(msg::XML_INVALID_DATA).into());
    }
    if want.is_none() && st.is_nil() {
        st.read_simple(ex, false)?;
        return Ok(Value::Undefined);
    }
    let t = match want {
        Some(t) if readable_type(ex, t) => t,
        Some(_) => return Err(not_serializable()),
        None => element_type(ex, &st).ok_or_else(not_serializable)?,
    };
    read_value(ex, &mut st, t)
}

fn can_read(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    let r = arg_value(ex, args.first())?;
    let st = xml_reader(ex, &r)?;
    Ok(Value::Bool(
        st.at_start() && (st.is_nil() || element_type(ex, &st).is_some()),
    ))
}

fn get_type(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    let r = arg_value(ex, args.first())?;
    let st = xml_reader(ex, &r)?;
    if let Some((ns, local)) = st.xsi_type() {
        return Ok(new_xml_data_type(&ns, &local));
    }
    Ok(match st.node() {
        Some(XmlEvent::Start { ns, local, .. }) => new_xml_data_type(ns, local),
        _ => Value::Undefined,
    })
}
