use super::coll::{CollTy, new_coll};
use super::{MdCtx, MdKey, MdPath, not_implemented};
use crate::model::Uuid16;
use bsl_vm::{CallResult, Value};
use md_model::{MdObject, Value as Mv};
use std::sync::Arc;

pub(super) fn object(cx: &MdCtx, k: usize) -> Option<&MdObject> {
    cx.model()
        .subsystem(k)
        .filter(|o| super::children::described(&o.name, &o.props))
}

pub(super) fn top_level(cx: &MdCtx) -> Vec<MdKey> {
    cx.model()
        .subsystems
        .iter()
        .enumerate()
        .filter(|(_, d)| d.parent.is_none())
        .map(|(k, _)| MdKey::Path(MdPath::Subsystem(k)))
        .collect()
}

pub(super) fn nested(cx: &Arc<MdCtx>, k: usize) -> CallResult<Value> {
    let refuse = || not_implemented("Подсистемы");
    object(cx, k).ok_or_else(refuse)?;
    let def = cx.model().subsystems.get(k).ok_or_else(refuse)?;
    let mut items = Vec::with_capacity(def.children.len());
    for &child in &def.children {
        if object(cx, child).is_none() {
            return Err(refuse().into());
        }
        items.push(MdKey::Path(MdPath::Subsystem(child)));
    }
    Ok(new_coll(cx, CollTy::Objects, items))
}

fn id_of(cx: &MdCtx, path: MdPath) -> Option<Uuid16> {
    let model = cx.model();
    match path {
        MdPath::Object(o) => Some(model.obj(o).md),
        MdPath::CommonModule(i) => model.common_modules.get(i).map(|m| m.md),
        MdPath::SessionParam(i) => model.session_params.get(i).map(|p| p.md),
        MdPath::Subscription(i) => model.subscriptions.get(i).map(|s| s.md),
        MdPath::Subsystem(k) => model.subsystem(k).map(|o| o.id),
        MdPath::Other(i) => model.config.objects.get(i).map(|o| o.id),
        _ => None,
    }
}

pub(super) fn content(cx: &Arc<MdCtx>, k: usize) -> CallResult<Value> {
    let refuse = || not_implemented("Состав");
    let object = object(cx, k).ok_or_else(refuse)?;
    let Some(Mv::RefList(refs)) = object.prop("Content") else {
        return Err(refuse().into());
    };
    let mut items = Vec::with_capacity(refs.len());
    for r in refs {
        let path = super::props::resolve_ref(cx, r).ok_or_else(refuse)?;
        items.push((id_of(cx, path).ok_or_else(refuse)?, path));
    }
    items.sort_by_key(|(id, _)| *id);
    let listed = items.len();
    items.dedup_by_key(|(id, _)| *id);
    if items.len() != listed {
        return Err(refuse().into());
    }
    let items = items.into_iter().map(|(_, p)| MdKey::Path(p)).collect();
    Ok(new_coll(cx, CollTy::Values, items))
}

pub(super) fn is_refused(folded: &str) -> bool {
    matches!(
        folded,
        "картинка" | "командныйинтерфейс" | "объектрасширяемойконфигурации"
    )
}
