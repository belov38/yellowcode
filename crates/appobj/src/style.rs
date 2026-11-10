use crate::Application;
use crate::meta::{not_implemented, synonym_text};
use bsl_prim::Number;
use bsl_stdlib::{StyleItem, StyleItemKind, StyleItems};
use bsl_vm::{Arg, CallError, CallResult, Execution, Registry, Value};
use md_model::{MdKind, XmlElem};
use std::sync::Arc;

mod texts;

pub(crate) fn register(app: &Arc<Application>, reg: &mut Registry) {
    let model = app.model();
    let items = model
        .config
        .objects
        .iter()
        .filter(|o| o.kind == MdKind::StyleItem)
        .filter_map(|o| {
            let kind = match o.props.iter().find(|p| p.name == "Type").map(|p| &p.value) {
                Some(md_model::Value::Text(t)) if t == "Color" => StyleItemKind::Color,
                Some(md_model::Value::Text(t)) if t == "Font" => StyleItemKind::Font,
                _ => return None,
            };
            Some(StyleItem {
                name: o.name.clone(),
                synonym: synonym_text(&o.synonym, &model.lang_code),
                uuid: o.id.to_string(),
                kind,
            })
        })
        .collect();
    let owned = |table: &[(&str, &str)]| {
        table
            .iter()
            .map(|(name, text)| (name.to_string(), text.to_string()))
            .collect::<Vec<_>>()
    };
    reg.set_data(StyleItems::new(
        items,
        owned(texts::BUILTIN),
        owned(texts::WEB),
    ));
}

fn refuse() -> CallError {
    not_implemented("Значение").into()
}

pub(crate) fn value(ex: &mut Execution, e: &XmlElem) -> CallResult<Value> {
    let ty = e
        .attrs
        .iter()
        .find(|(k, _)| k == "xsi:type")
        .map(|(_, v)| v.as_str());
    match ty {
        Some("v8ui:Color") => color(ex, e.text.as_deref().unwrap_or_default()),
        Some("v8ui:Font") => font(ex, e),
        _ => Err(refuse()),
    }
}

fn member(ex: &mut Execution, enumeration: &str, name: &str) -> CallResult<Value> {
    let t = platform_catalog::catalog()
        .find_type(enumeration)
        .ok_or_else(refuse)?;
    let manager = ex.system_enum(t)?;
    Ok(ex.get_member(&manager, name)?)
}

fn color(ex: &mut Execution, text: &str) -> CallResult<Value> {
    if text == "auto" {
        return Ok(ex.construct_by_name("Цвет", &mut [])?);
    }
    if let Some(hex) = text.strip_prefix('#') {
        let n = u32::from_str_radix(hex, 16)
            .ok()
            .filter(|_| hex.len() == 6)
            .ok_or_else(refuse)?;
        let mut rgb =
            [n >> 16, (n >> 8) & 255, n & 255].map(|c| Arg::Val(Value::from(i64::from(c))));
        return Ok(ex.construct_by_name("Цвет", &mut rgb)?);
    }
    let (prefix, name) = text.split_once(':').ok_or_else(refuse)?;
    let enumeration = match prefix {
        "web" => "WebЦвета",
        "pal" => "ЦветаПалитры",
        "style" => "ЦветаСтиля",
        _ => return Err(refuse()),
    };
    member(ex, enumeration, name)
}

fn font(ex: &mut Execution, e: &XmlElem) -> CallResult<Value> {
    let attr = |name: &str| {
        e.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    };
    let flag = |name: &str| match attr(name) {
        Some("true") => Value::Bool(true),
        Some("false") => Value::Bool(false),
        _ => Value::Undefined,
    };
    let number = |name: &str| {
        attr(name)
            .and_then(Number::parse)
            .map_or(Value::Undefined, Value::Number)
    };
    let mut args = Vec::new();
    match (attr("kind"), attr("ref"), attr("faceName")) {
        (Some("StyleItem"), Some(base), None) => {
            let name = base.strip_prefix("style:").ok_or_else(refuse)?;
            args.push(Arg::Val(member(ex, "ШрифтыСтиля", name)?));
            args.push(Arg::Val(Value::Undefined));
        }
        (Some("Absolute"), None, Some(face)) => args.push(Arg::Val(Value::from(face))),
        _ => return Err(refuse()),
    }
    for field in [
        number("height"),
        flag("bold"),
        flag("italic"),
        flag("underline"),
        flag("strikeout"),
        number("scale"),
    ] {
        args.push(Arg::Val(field));
    }
    Ok(ex.construct_by_name("Шрифт", &mut args)?)
}
