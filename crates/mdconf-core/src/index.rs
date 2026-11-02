use crate::entries::{Entries, EntryData, root_uuid};
use crate::error::{Error, Result};
use crate::model::{ExternalKind, SubKind};
use crate::objects::overlay;
use crate::objects::path::{find_collection, find_header};
use crate::objects::recalc::{
    DIM_HEADER, DIM_UUID, GENERATED, RECALC_CLASS, RECALC_DIM_CLASS, RECALC_HEADER, recalc_header,
};
use brace::{Guid, Node};
use mdschema::{ChildSpec, KindSpec, MdKind, Schema};
use std::collections::HashSet;
use std::sync::Arc;

pub const MAX_NESTING: usize = 64;

pub const MAX_EXTERNAL_DEPTH: usize = 2;

pub(crate) use md_model::index::{Index, TypeOwner};

pub fn build(e: &Entries, s: &'static Schema) -> Result<Index> {
    let mut ix = Index {
        cfg_id: root_uuid(e)?,
        ..Default::default()
    };
    let cfg_name = ix.cfg_id.to_string();
    let cfg = brace_entry(e, &cfg_name)?;
    let cfg_tree = tree(&cfg, &cfg_name)?;
    ix.header_id = cfg_tree
        .get(&[3, 1, 1, 1, 1, 1, 2])
        .and_then(Node::as_guid)
        .ok_or_else(|| Error::Format {
            entry: cfg_name.clone(),
            what: "no configuration header uuid".into(),
        })?;
    for (kind, uuids) in class_lists(cfg_tree, s) {
        index_kind(&mut ix, e, s, kind, &uuids);
    }
    Ok(ix)
}

fn index_kind(ix: &mut Index, e: &Entries, s: &'static Schema, kind: MdKind, uuids: &[Guid]) {
    let spec = overlay::kind(s, kind.as_str());
    for &u in uuids {
        let entry = u.to_string();
        if !e.contains(&entry) {
            ix.missing.push(u);
            continue;
        }
        let Some(spec) = spec else {
            ix.objs.insert(u, (kind, None));
            continue;
        };
        let Some(header) = &spec.header else {
            ix.objs.insert(u, (kind, None));
            continue;
        };
        let Ok(Some(data)) = e.doc(&entry) else {
            ix.missing.push(u);
            continue;
        };
        let Some(raw) = doc_tree(&data) else {
            ix.missing.push(u);
            continue;
        };
        let t = &*overlay::normalize(kind.as_str(), raw);
        let name = name_at(t, header);
        ix.objs.insert(u, (kind, Some(name.clone())));
        let full = format!("{}.{name}", kind.as_str());
        ix.names.insert(u, full.clone());
        ix.order.push(u);
        for ii in &spec.internal_info {
            if let Some(tid) = t.get(&ii.type_id).and_then(Node::as_guid) {
                let pref = s
                    .gt_name(&ii.category)
                    .map_or_else(|| format!("{}{}", kind.as_str(), ii.category), String::from);
                ix.types.insert(tid, format!("{pref}.{name}"));
                ix.type_owners.insert(
                    tid,
                    TypeOwner {
                        object: u,
                        child: None,
                        category: ii.category.clone(),
                        recalc: None,
                        external: None,
                    },
                );
            }
        }
        index_children(ix, t, &spec.children, &full, (kind, &name, u));
        index_subobjects(ix, e, spec, kind, u, &data, &name);
        if kind == MdKind::ExternalDataSource {
            index_external(ix, e, s, t, spec, u, u, &full, &name);
        }
        if kind == MdKind::CalculationRegister {
            index_recalcs(ix, e, t, u, &full, &name);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn index_external(
    ix: &mut Index,
    e: &Entries,
    s: &'static Schema,
    tree: &Node,
    spec: &KindSpec,
    owner: Guid,
    source: Guid,
    full: &str,
    qualified: &str,
) {
    for prop in s.xdto_props(&format!("{}ChildObjects", spec.kind)) {
        let tag = prop.name.as_str();
        let Some(kind) = ExternalKind::from_name(tag) else {
            continue;
        };
        let Some((_, class)) = spec.subobjects.iter().find(|(t, _)| t == tag) else {
            continue;
        };
        let Some(nested_spec) = s.kind(tag) else {
            continue;
        };
        for item in find_collection(tree, class).unwrap_or_default() {
            let Some(id) = item.as_guid() else { continue };
            if ix.names.contains_key(&id) {
                ix.skipped.push(format!(
                    "ExternalDataSource {source}: {tag} {id} invalid nested ownership"
                ));
                continue;
            }
            let entry = id.to_string();
            let data = e.doc(&entry).ok().flatten();
            let raw = data.as_deref().and_then(doc_tree);
            let valid = raw.zip(nested_spec.header.as_deref()).filter(|(t, h)| {
                t.get(h).is_some_and(|n| n.as_list().is_some())
                    && nested_spec
                        .uuid
                        .as_deref()
                        .and_then(|p| t.get(p))
                        .and_then(Node::as_guid)
                        == Some(id)
            });
            let Some((raw, hdr)) = valid else {
                ix.skipped.push(format!(
                    "ExternalDataSource {source}: {tag} {id} missing or malformed entry"
                ));
                continue;
            };
            let name = name_at(raw, hdr);
            let fullname = format!("{full}.{tag}.{name}");
            let typename = format!("{qualified}.{name}");
            ix.names.insert(id, fullname.clone());
            ix.external.entry(owner).or_default().push((kind, id));
            for ii in &nested_spec.internal_info {
                if let Some(tid) = raw.get(&ii.type_id).and_then(Node::as_guid) {
                    ix.types.insert(
                        tid,
                        format!(
                            "{}.{typename}",
                            s.nested_gt_prefix(tag, &ii.category).unwrap_or_default()
                        ),
                    );
                    ix.type_owners.insert(
                        tid,
                        TypeOwner {
                            object: source,
                            child: None,
                            category: ii.category.clone(),
                            recalc: None,
                            external: Some(id),
                        },
                    );
                }
            }
            index_children(
                ix,
                raw,
                &nested_spec.children,
                &fullname,
                (MdKind::ExternalDataSource, &typename, source),
            );
            for (subtag, subclass) in &nested_spec.subobjects {
                let sk = match subtag.as_str() {
                    "Form" => SubKind::Form,
                    "Template" => SubKind::Template,
                    _ => continue,
                };
                for su in find_collection(raw, subclass)
                    .unwrap_or_default()
                    .iter()
                    .filter_map(Node::as_guid)
                {
                    let sd = e.doc(&su.to_string()).ok().flatten();
                    let found = sd
                        .as_deref()
                        .and_then(doc_tree)
                        .and_then(|st| Some((st, find_header(st, &su.to_string())?)));
                    let Some((st, h)) = found else {
                        ix.skipped.push(format!(
                            "ExternalDataSource {source}: {subtag} {su} missing or malformed entry"
                        ));
                        continue;
                    };
                    ix.names
                        .insert(su, format!("{fullname}.{subtag}.{}", name_at(st, &h)));
                    ix.sub_hdr.insert(su, h);
                    ix.subs.entry(id).or_default().push((sk, su));
                }
            }
            if kind == ExternalKind::Cube {
                index_external(ix, e, s, raw, nested_spec, id, source, &fullname, &typename);
            }
        }
    }
}

fn index_recalcs(ix: &mut Index, e: &Entries, reg: &Node, owner: Guid, full: &str, name: &str) {
    for n in find_collection(reg, RECALC_CLASS).unwrap_or_default() {
        let Some(r) = n.as_guid() else { continue };
        let Ok(Some(data)) = e.doc(&r.to_string()) else {
            continue;
        };
        let Some(t) = doc_tree(&data) else { continue };
        if recalc_header(t).is_none() {
            continue;
        }
        let rn = name_at(t, &RECALC_HEADER);
        let rfull = format!("{full}.Recalculation.{rn}");
        for it in find_collection(t, RECALC_DIM_CLASS).unwrap_or_default() {
            if let Some(d) = it.get(&DIM_UUID).and_then(Node::as_guid) {
                let dn = name_at(it, &DIM_HEADER);
                ix.names.insert(d, format!("{rfull}.Dimension.{dn}"));
            }
        }
        for (cat, tid, _) in &GENERATED {
            if let Some(tid) = t.get(tid).and_then(Node::as_guid) {
                ix.types
                    .insert(tid, format!("Recalculation{cat}.{name}.{rn}"));
                ix.type_owners.insert(
                    tid,
                    TypeOwner {
                        object: owner,
                        child: None,
                        category: (*cat).into(),
                        recalc: Some(r),
                        external: None,
                    },
                );
            }
        }
        ix.names.insert(r, rfull);
        ix.recalcs.entry(owner).or_default().push(r);
    }
}

fn index_children(
    ix: &mut Index,
    node: &Node,
    children: &[(String, ChildSpec)],
    prefix: &str,
    (kind, objname, owner): (MdKind, &str, Guid),
) {
    for (class, cs) in children {
        let Some(header) = &cs.header else { continue };
        for it in find_collection(node, class).unwrap_or_default() {
            let nm = name_at(it, header);
            let full = format!("{prefix}.{}.{nm}", cs.tag);
            let child = cs
                .uuid
                .as_ref()
                .and_then(|p| it.get(p))
                .and_then(Node::as_guid);
            if let Some(u) = child {
                ix.names.insert(u, full.clone());
            }
            for ii in &cs.internal_info {
                if let Some(tid) = it.get(&ii.type_id).and_then(Node::as_guid) {
                    ix.types.insert(
                        tid,
                        format!(
                            "{}{}.{objname}.{nm}",
                            md_xmlout::codec::objects::child_generated_kind(kind.as_str(), &cs.tag),
                            ii.category
                        ),
                    );
                    ix.type_owners.insert(
                        tid,
                        TypeOwner {
                            object: owner,
                            child,
                            category: ii.category.clone(),
                            recalc: None,
                            external: None,
                        },
                    );
                }
            }
            index_children(ix, it, &cs.children, &full, (kind, objname, owner));
        }
    }
}

fn index_subobjects(
    ix: &mut Index,
    e: &Entries,
    spec: &KindSpec,
    kind: MdKind,
    root_owner: Guid,
    root_data: &EntryData,
    root_name: &str,
) {
    struct Frame {
        owner: Guid,
        depth: usize,
        todo: Vec<(&'static str, Guid)>,
        full: String,
        name: String,
    }
    let pending = |data: &EntryData| -> Vec<(&'static str, Guid)> {
        let Some(t) = doc_tree(data) else {
            return Vec::new();
        };
        let mut v = Vec::new();
        for (tag, class) in &spec.subobjects {
            let tag: &'static str = match tag.as_str() {
                "Form" => "Form",
                "Template" => "Template",
                x if x == kind.as_str() => "Same",
                _ => continue,
            };
            for n in find_collection(t, class).unwrap_or_default() {
                if let Some(g) = n.as_guid() {
                    v.push((tag, g));
                }
            }
        }
        v.reverse();
        v
    };
    if let Some(t) = doc_tree(root_data) {
        for (tag, class) in &spec.subobjects {
            if matches!(tag.as_str(), "Form" | "Template")
                || tag == kind.as_str()
                || (kind == MdKind::ExternalDataSource && ExternalKind::from_name(tag).is_some())
            {
                continue;
            }
            if find_collection(t, class).is_some_and(|c| !c.is_empty()) {
                ix.skipped.push(format!(
                    "{} {root_owner}: {tag} sub-objects are not modelled (C3f)",
                    kind.as_str()
                ));
            }
        }
    }
    let mut seen: HashSet<Guid> = HashSet::from([root_owner]);
    let mut stack = vec![Frame {
        owner: root_owner,
        depth: 0,
        todo: pending(root_data),
        full: ix
            .names
            .get(&root_owner)
            .cloned()
            .unwrap_or_else(|| format!("{}.{root_name}", kind.as_str())),
        name: root_name.to_string(),
    }];
    while let Some(top) = stack.last_mut() {
        let Some((tag, su)) = top.todo.pop() else {
            stack.pop();
            continue;
        };
        let entry = su.to_string();
        let Ok(Some(sdata)) = e.doc(&entry) else {
            continue;
        };
        let Some(st) = doc_tree(&sdata) else { continue };
        if tag == "Same" {
            if !seen.insert(su) {
                continue;
            }
            let depth = top.depth + 1;
            if depth > MAX_NESTING {
                ix.too_deep.push(su);
                continue;
            }
            let sn = spec
                .header
                .as_deref()
                .map(|h| name_at(st, h))
                .unwrap_or_default();
            let full = format!("{}.{}.{sn}", top.full, kind.as_str());
            ix.objs.insert(su, (kind, Some(sn.clone())));
            ix.names.insert(su, full.clone());
            ix.nested.push((su, top.owner));
            let todo = pending(&sdata);
            stack.push(Frame {
                owner: su,
                depth,
                todo,
                full,
                name: sn,
            });
            continue;
        }
        let Some(h) = find_header(st, &entry) else {
            continue;
        };
        let sn = name_at(st, &h);
        let sk = if tag == "Form" {
            SubKind::Form
        } else {
            SubKind::Template
        };
        ix.names
            .insert(su, format!("{}.{}.{tag}.{sn}", kind.as_str(), top.name));
        ix.sub_hdr.insert(su, h);
        ix.subs.entry(top.owner).or_default().push((sk, su));
    }
}

fn brace_entry(e: &Entries, name: &str) -> Result<Arc<EntryData>> {
    e.doc(name)?.ok_or_else(|| {
        if e.contains(name) {
            Error::Format {
                entry: name.into(),
                what: "not a brace document".into(),
            }
        } else {
            Error::MissingEntry(name.into())
        }
    })
}

fn doc_tree(d: &EntryData) -> Option<&Node> {
    match d {
        EntryData::Brace(doc) => Some(&doc.root),
        _ => None,
    }
}

fn tree<'a>(d: &'a EntryData, entry: &str) -> Result<&'a Node> {
    doc_tree(d).ok_or_else(|| Error::Format {
        entry: entry.into(),
        what: "not a brace document".into(),
    })
}

fn name_at(t: &Node, header: &[usize]) -> String {
    let mut p = header.to_vec();
    p.push(2);
    t.get(&p)
        .and_then(Node::text)
        .unwrap_or_default()
        .to_string()
}

fn class_lists(cfg: &Node, s: &Schema) -> Vec<(MdKind, Vec<Guid>)> {
    let mut found = Vec::new();
    let mut stack = vec![cfg];
    while let Some(n) = stack.pop() {
        let Some(l) = n.as_list() else { continue };
        if let (Some(head), Some(cnt)) = (l.first(), l.get(1)) {
            let kind = head
                .text()
                .and_then(|c| s.class_kind(c))
                .and_then(MdKind::from_name);
            if let (Some(kind), false, Some(c)) = (
                kind,
                cnt.is_list(),
                cnt.text().and_then(|t| t.trim().parse::<usize>().ok()),
            ) {
                let items = l.get(2..).unwrap_or_default();
                let items = items.get(..c.min(items.len())).unwrap_or(items);
                found.push((kind, items.iter().filter_map(Node::as_guid).collect()));
                continue;
            }
        }
        stack.extend(l.iter().rev());
    }
    found
}
