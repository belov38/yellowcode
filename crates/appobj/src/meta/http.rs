use super::coll::{CollTy, new_coll};
use super::{MdCtx, MdKey, MdPath, member_name, member_of, not_implemented};
use bsl_vm::{CallResult, MemberId, MemberKind, Value};
use md_model::{Child, ChildTag, MdKind, MdObject};
use platform_catalog::TypeIdx;
use std::sync::Arc;

const URL_TYPE: &str = "MetadataObject: HTTPServiceURLTemplate";
pub(super) const COMMON_BASE: u32 = 1 << 24;
const COMMON_MEMBERS: [&str; 6] = [
    "Name",
    "Synonym",
    "Comment",
    "FullName",
    "Parent",
    "Presentation",
];

fn service(cx: &MdCtx, index: usize) -> Option<&MdObject> {
    cx.model()
        .config
        .objects
        .get(index)
        .filter(|s| s.kind == MdKind::HTTPService && super::children::described(&s.name, &s.props))
}

fn url(cx: &MdCtx, s: usize, u: usize) -> Option<&Child> {
    service(cx, s)?
        .children
        .get(u)
        .filter(|u| u.tag == ChildTag::URLTemplate && super::children::described(&u.name, &u.props))
}

pub(super) fn child(cx: &MdCtx, path: MdPath) -> Option<&Child> {
    match path {
        MdPath::HttpUrlTemplate(s, u) => url(cx, s, u),
        MdPath::HttpMethod(s, u, m) => url(cx, s, u)?
            .children
            .get(m)
            .filter(|m| m.tag == ChildTag::Method && super::children::described(&m.name, &m.props)),
        _ => None,
    }
}

pub(super) fn urls(cx: &Arc<MdCtx>, s: usize) -> CallResult<Value> {
    let owner = service(cx, s).ok_or_else(|| not_implemented("ШаблоныURL"))?;
    let mut items = Vec::new();
    for (u, item) in owner.children.iter().enumerate() {
        if item.tag == ChildTag::URLTemplate {
            let path = MdPath::HttpUrlTemplate(s, u);
            if child(cx, path).is_none() {
                return Err(not_implemented("ШаблоныURL").into());
            }
            items.push(MdKey::Path(path));
        }
    }
    Ok(new_coll(cx, CollTy::Objects, items))
}

pub(super) fn methods(cx: &Arc<MdCtx>, s: usize, u: usize) -> CallResult<Value> {
    let owner = url(cx, s, u).ok_or_else(|| not_implemented("Методы"))?;
    let mut items = Vec::new();
    for (m, item) in owner.children.iter().enumerate() {
        if item.tag == ChildTag::Method {
            let path = MdPath::HttpMethod(s, u, m);
            if child(cx, path).is_none() {
                return Err(not_implemented("Методы").into());
            }
            items.push(MdKey::Path(path));
        }
    }
    Ok(new_coll(cx, CollTy::Objects, items))
}

pub(super) fn common_member(cx: &MdCtx, name: &str) -> Option<(MemberId, MemberKind)> {
    let t = cx.catalog.find_type(URL_TYPE)?;
    let (id, kind) = member_of(cx.catalog, t, name)?;
    let (_, _, en) = member_name(cx.catalog, t, id)?;
    COMMON_MEMBERS
        .contains(&en)
        .then_some((MemberId(id.0.checked_add(COMMON_BASE)?), kind))
}

pub(super) fn common_source(cx: &MdCtx, id: MemberId) -> Option<(TypeIdx, MemberId)> {
    let t = cx.catalog.find_type(URL_TYPE)?;
    let source = MemberId(id.0.checked_sub(COMMON_BASE)?);
    let (_, _, en) = member_name(cx.catalog, t, source)?;
    COMMON_MEMBERS.contains(&en).then_some((t, source))
}
