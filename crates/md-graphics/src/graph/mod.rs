pub(crate) mod chart;
mod chartfmt;
mod chartspec;
pub(crate) mod color;
pub(crate) mod font;
pub(crate) mod formsettings;
pub(crate) mod gschema;
pub(crate) mod line;
pub(crate) mod lstr;
pub(crate) mod picture;
pub(crate) mod read;
pub(crate) mod route;
pub(crate) mod scalar;
pub(crate) mod sheet;
pub(crate) mod tvalue;
pub(crate) mod values;

use crate::entries::EntryData;
use crate::load::LoadCtx;
use crate::objects::path::find_collection;
use crate::xml_out::{DumpSink, Part, PartStatus, paths};
use crate::{Configuration, MdKind, Result};
use brace::{Guid, Node};
use gschema::{Route, Task};
use mdschema::ChildSpec;
use std::borrow::Cow;
use values::GraphError;

pub use crate::templates::context::Lookup as FormSettingsLookup;
pub use md_model::graph::Flowchart;

const FLOWCHART_SUFFIX: &str = ".7";

pub(crate) fn entry_tree(data: &EntryData) -> std::result::Result<Cow<'_, Node>, GraphError> {
    match data {
        EntryData::Brace(doc) => Ok(Cow::Borrowed(&doc.root)),
        EntryData::Bytes(b) | EntryData::Json(b) => brace::parse_bytes(b)
            .map(Cow::Owned)
            .map_err(|e| GraphError(e.to_string())),
        EntryData::Nested(_) => Err(GraphError("nested container".into())),
    }
}

pub(crate) fn schema_template(cfg: &Configuration, data: &EntryData) -> Option<Vec<u8>> {
    let t = entry_tree(data).ok()?;
    gschema::to_xml(cfg, &t, None).ok()
}

fn child_order(cx: &LoadCtx, cfg: &Configuration, id: Guid) -> Vec<Guid> {
    fn walk(node: &Node, children: &[(String, ChildSpec)], out: &mut Vec<Guid>) {
        for (class, cs) in children {
            if cs.header.is_none() {
                continue;
            }
            for it in find_collection(node, class).unwrap_or_default() {
                if let Some(u) = cs
                    .uuid
                    .as_ref()
                    .and_then(|p| it.get(p))
                    .and_then(Node::as_guid)
                {
                    out.push(u);
                }
                walk(it, &cs.children, out);
            }
        }
    }
    let mut out = Vec::new();
    let Some((kind, Some(_))) = cfg.index.objs.get(&id) else {
        return out;
    };
    let Some(spec) = cx.schema.kind(kind.as_str()) else {
        return out;
    };
    let name = id.to_string();
    if let Ok(Some(data)) = cx.entries.doc(&name)
        && let EntryData::Brace(doc) = &*data
    {
        walk(&doc.root, &spec.children, &mut out);
    }
    if !cx.opts.keep_source {
        cx.entries.forget(&name);
    }
    out
}

fn task_of(
    cx: &LoadCtx,
    cfg: &Configuration,
    bp: Guid,
) -> std::result::Result<Option<Task>, GraphError> {
    let name = bp.to_string();
    let data = cx
        .entries
        .doc(&name)
        .map_err(|e| GraphError(e.to_string()))?
        .ok_or_else(|| GraphError(format!("business process {name}: no entry")))?;
    if !cx.opts.keep_source {
        cx.entries.forget(&name);
    }
    let EntryData::Brace(doc) = &*data else {
        return Err(GraphError(format!(
            "business process {name}: not a brace entry"
        )));
    };
    let Some(tu) = doc
        .root
        .get(&[1, 25])
        .and_then(Node::text)
        .and_then(Guid::parse)
    else {
        return Ok(None);
    };
    let Some(task) = cfg.index.names.get(&tu) else {
        return Ok(None);
    };
    let pref = format!("{task}.AddressingAttribute.");
    let mut rest: Vec<(&Guid, &String)> = cfg
        .index
        .names
        .iter()
        .filter(|(_, n)| n.starts_with(&pref))
        .collect();
    let mut attrs = Vec::new();
    if !rest.is_empty() {
        for u in child_order(cx, cfg, tu) {
            if let Some(i) = rest.iter().position(|(g, _)| **g == u) {
                let (g, n) = rest.swap_remove(i);
                attrs.push((g.to_string(), n.clone()));
            }
        }
        rest.sort_by(|a, b| a.1.cmp(b.1).then(a.0.cmp(b.0)));
        attrs.extend(rest.into_iter().map(|(g, n)| (g.to_string(), n.clone())));
    }
    Ok(Some(Task {
        name: task.clone(),
        attrs,
    }))
}

#[doc(hidden)]
pub fn load(cx: &LoadCtx, cfg: &mut Configuration) -> Result<()> {
    let mut found = Vec::new();
    let mut failed = 0u32;
    for (i, o) in cfg.objects.iter().enumerate() {
        if o.kind != MdKind::BusinessProcess {
            continue;
        }
        let name = format!("{}{FLOWCHART_SUFFIX}", o.id);
        let data = match cx.entries.get(&name) {
            Ok(Some(data)) => data,
            Ok(None) => continue,
            Err(_) => {
                failed += 1;
                continue;
            }
        };
        let route = Route {
            task: task_of(cx, cfg, o.id),
        };
        let xml = entry_tree(&data).and_then(|t| gschema::to_xml(cfg, &t, Some(&route)));
        if !cx.opts.keep_source {
            cx.entries.forget(&name);
        }
        match xml {
            Ok(xml) => found.push((i, xml)),
            Err(_) => failed += 1,
        }
    }
    if failed > 0 {
        let n = cfg
            .diagnostics
            .misses
            .entry("flowchart:failed".to_string())
            .or_insert(0);
        *n = n.saturating_add(failed);
    }
    for (i, xml) in found {
        if let Some(o) = cfg.objects.get_mut(i) {
            o.flowchart = Some(Flowchart { xml });
        }
    }
    Ok(())
}

#[doc(hidden)]
pub fn dump(cfg: &Configuration, sink: &mut DumpSink) -> Result<PartStatus> {
    for o in cfg.of_kind(MdKind::BusinessProcess) {
        if let Some(f) = &o.flowchart {
            let path = format!("{}/Ext/Flowchart.xml", paths::object_dir(cfg, o));
            sink.put(Part::Flowcharts, &path, &f.xml)?;
        }
    }
    Ok(PartStatus::Written)
}

#[cfg_attr(not(test), allow(dead_code))]
#[doc(hidden)]
pub fn form_settings_lines(
    cfg: &Configuration,
    value: &brace::Node,
    depth: usize,
) -> Option<Vec<String>> {
    form_settings_lines_resolved(cfg, value, depth)
}

pub fn form_settings_lines_resolved(
    cfg: &dyn FormSettingsLookup,
    value: &brace::Node,
    depth: usize,
) -> Option<Vec<String>> {
    settings_kind(value)?;
    Some(formsettings::settings_lines(cfg, value, depth).unwrap_or_default())
}

#[cfg_attr(not(test), allow(dead_code))]
#[doc(hidden)]
pub fn settings_kind(value: &brace::Node) -> Option<&'static str> {
    formsettings::kind(value)
}
