use super::{MdCtx, MdPath, not_implemented};
use crate::model::ObjId;
use bsl_vm::HostError;
use md_model::{Child, ChildTag, MdObject, Prop, SubKind, SubObject, Value};

pub(super) fn described(name: &str, props: &[Prop]) -> bool {
    props
        .iter()
        .any(|p| p.name == "Name" && matches!(&p.value, Value::Text(n) if n == name))
}

pub(super) fn owner(cx: &MdCtx, obj: ObjId) -> Option<&MdObject> {
    let def = cx.model().obj(obj);
    let source = cx.model().config.objects.get(*cx.cfg_index.get(&def.md)?)?;
    (source.id == def.md && source.name == def.name && described(&source.name, &source.props))
        .then_some(source)
}

pub(super) fn command(cx: &MdCtx, obj: ObjId, index: usize) -> Option<&Child> {
    owner(cx, obj)?
        .children
        .get(index)
        .filter(|c| c.tag == ChildTag::Command && described(&c.name, &c.props))
}

pub(super) fn template(cx: &MdCtx, obj: ObjId, index: usize) -> Option<&SubObject> {
    owner(cx, obj)?
        .templates
        .get(index)
        .filter(|t| t.kind == SubKind::Template && described(&t.name, &t.props))
}

pub(super) fn admit(cx: &MdCtx, path: MdPath) -> Result<(), HostError> {
    match path {
        MdPath::Command(o, i) if command(cx, o, i).is_none() => Err(not_implemented("Команда")),
        MdPath::Template(o, i) if template(cx, o, i).is_none() => Err(not_implemented("Макет")),
        MdPath::HttpUrlTemplate(..) | MdPath::HttpMethod(..)
            if super::http::child(cx, path).is_none() =>
        {
            Err(not_implemented("HTTPСервис"))
        }
        MdPath::WebOperation(..) | MdPath::WebParameter(..)
            if super::web::child(cx, path).is_none() =>
        {
            Err(not_implemented("WebСервис"))
        }
        _ => Ok(()),
    }
}
