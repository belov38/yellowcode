use super::coll::{CollTy, new_coll};
use super::{MdCtx, MdKey, MdPath, not_implemented};
use bsl_vm::{CallResult, HostError, Value};
use md_model::{Child, ChildTag, Codec, MdKind, MdObject, Prop, Value as Mv};
use std::sync::Arc;

fn service(cx: &MdCtx, index: usize) -> Option<&MdObject> {
    cx.model()
        .config
        .objects
        .get(index)
        .filter(|s| s.kind == MdKind::WebService && super::children::described(&s.name, &s.props))
}

fn operation(cx: &MdCtx, s: usize, o: usize) -> Option<&Child> {
    service(cx, s)?
        .children
        .get(o)
        .filter(|o| o.tag == ChildTag::Operation && super::children::described(&o.name, &o.props))
}

pub(super) fn child(cx: &MdCtx, path: MdPath) -> Option<&Child> {
    match path {
        MdPath::WebOperation(s, o) => operation(cx, s, o),
        MdPath::WebParameter(s, o, p) => operation(cx, s, o)?.children.get(p).filter(|p| {
            p.tag == ChildTag::Parameter && super::children::described(&p.name, &p.props)
        }),
        _ => None,
    }
}

pub(super) fn operations(cx: &Arc<MdCtx>, s: usize) -> CallResult<Value> {
    let owner = service(cx, s).ok_or_else(|| not_implemented("Операции"))?;
    let mut items = Vec::new();
    for (o, item) in owner.children.iter().enumerate() {
        if item.tag == ChildTag::Operation {
            let path = MdPath::WebOperation(s, o);
            if child(cx, path).is_none() {
                return Err(not_implemented("Операции").into());
            }
            items.push(MdKey::Path(path));
        }
    }
    Ok(new_coll(cx, CollTy::Objects, items))
}

fn parameters(cx: &Arc<MdCtx>, s: usize, o: usize) -> CallResult<Value> {
    let owner = operation(cx, s, o).ok_or_else(|| not_implemented("Параметры"))?;
    let mut items = Vec::new();
    for (p, item) in owner.children.iter().enumerate() {
        if item.tag == ChildTag::Parameter {
            let path = MdPath::WebParameter(s, o, p);
            if child(cx, path).is_none() {
                return Err(not_implemented("Параметры").into());
            }
            items.push(MdKey::Path(path));
        }
    }
    Ok(new_coll(cx, CollTy::Objects, items))
}

fn property<'a>(child: &'a Child, en: &str) -> Option<&'a Prop> {
    child.props.iter().find(|p| p.name == en)
}

pub(super) fn synonym(cx: &MdCtx, path: MdPath) -> Result<String, HostError> {
    let p = child(cx, path).and_then(|c| property(c, "Synonym"));
    match p.map(|p| (p.codec, &p.value)) {
        Some((Codec::LStr, Mv::LStr(s))) => {
            Ok(super::object::synonym_text(s, &cx.model().lang_code))
        }
        _ => Err(not_implemented("Синоним")),
    }
}

pub(super) fn get(cx: &Arc<MdCtx>, path: MdPath, ru: &str, en: &str) -> CallResult<Value> {
    let c = child(cx, path).ok_or_else(|| not_implemented(ru))?;
    if en == "Parameters"
        && let MdPath::WebOperation(s, o) = path
    {
        return parameters(cx, s, o);
    }
    if en == "Synonym" {
        return Ok(Value::from(synonym(cx, path)?.as_str()));
    }
    let p = property(c, en).ok_or_else(|| not_implemented(ru))?;
    let result = match (en, p.codec, &p.value) {
        ("Name" | "Comment" | "ProcedureName", Codec::Str, Mv::Text(s)) => {
            Some(Value::from(s.as_str()))
        }
        ("Nillable" | "Transactioned", Codec::Bool, Mv::Bool(b)) => Some(Value::Bool(*b)),
        (
            "TransferDirection" | "DataLockControlMode" | "ObjectBelonging",
            Codec::Enum,
            Mv::Text(s),
        ) => {
            let ty = if en == "DataLockControlMode" {
                "DefaultDataLockControlMode"
            } else {
                en
            };
            cx.catalog
                .find_type(ty)
                .and_then(|t| bsl_stdlib::enums::enum_value(t, s))
        }
        _ => None,
    };
    result.ok_or_else(|| not_implemented(ru).into())
}
