use crate::entries::EntryData;
use crate::load::LoadCtx;
use crate::model::{Child, Diagnostics, MdObject, Module, ModuleKind};
use crate::{Configuration, Result};
use brace::{Guid, Node};
use std::sync::Arc;

const BOM: &[u8] = b"\xef\xbb\xbf";

use md_xmlout::codec::modules::CONFIG_MODULES;

pub(crate) fn kind_of_file(file: &str) -> Option<ModuleKind> {
    Some(match file {
        "Module.bsl" => ModuleKind::Module,
        "ObjectModule.bsl" => ModuleKind::ObjectModule,
        "ManagerModule.bsl" => ModuleKind::ManagerModule,
        "RecordSetModule.bsl" => ModuleKind::RecordSetModule,
        "ValueManagerModule.bsl" => ModuleKind::ValueManagerModule,
        "CommandModule.bsl" => ModuleKind::CommandModule,
        _ => return None,
    })
}

fn entry(cx: &LoadCtx, name: &str) -> Result<Option<Arc<EntryData>>> {
    let d = cx.entries.get(name)?;
    if !cx.opts.keep_source {
        cx.entries.forget(name);
    }
    Ok(d)
}

fn container_text(cx: &LoadCtx, name: &str) -> Result<Option<Vec<u8>>> {
    Ok(match entry(cx, name)?.as_deref() {
        Some(EntryData::Nested(items)) => items
            .iter()
            .find(|(n, _)| n == "text")
            .map(|(_, b)| b.clone()),
        _ => None,
    })
}

fn form_module(cx: &LoadCtx, id: Guid, diag: &mut Diagnostics) -> Result<Option<Module>> {
    let Some(d) = entry(cx, &format!("{id}.0"))? else {
        return Ok(None);
    };
    let doc = match &*d {
        EntryData::Brace(doc) => doc,
        EntryData::Bytes(raw) => {
            let m = lenient_form_module(raw);
            if m.is_some() {
                *diag
                    .misses
                    .entry(format!("lenient:form-module {id}"))
                    .or_default() += 1;
            }
            return Ok(m);
        }
        _ => return Ok(None),
    };
    if !doc.bom || !doc.lead.is_empty() {
        return Ok(None);
    }
    Ok(match doc.root.at(2) {
        Some(Node::Str(s)) if !s.is_empty() => {
            let mut source = BOM.to_vec();
            source.extend_from_slice(s.as_bytes());
            Some(Module {
                kind: ModuleKind::FormModule,
                source,
            })
        }
        _ => None,
    })
}

pub(crate) fn lenient_form_module(raw: &[u8]) -> Option<Module> {
    let body = raw.strip_prefix(BOM)?;
    if body.first() != Some(&b'{') {
        return None;
    }
    let (mut depth, mut in_str, mut commas) = (0usize, false, 0);
    let mut i = 0;
    while let Some(&b) = body.get(i) {
        match (in_str, b) {
            (true, b'"') => in_str = false,
            (true, _) => {}
            (false, b'"') => in_str = true,
            (false, b'{') => depth += 1,
            (false, b'}') => depth = depth.checked_sub(1)?,
            (false, b',') if depth == 1 => {
                commas += 1;
                if commas == 2 {
                    break;
                }
            }
            _ => {}
        }
        i += 1;
    }
    if commas != 2 {
        return None;
    }
    let mut rest = body.get(i + 1..)?.iter().copied().peekable();
    while rest.next_if(|c| c.is_ascii_whitespace()).is_some() {}
    rest.next_if_eq(&b'"')?;
    let mut text = Vec::new();
    loop {
        match rest.next()? {
            b'"' if rest.next_if_eq(&b'"').is_some() => text.push(b'"'),
            b'"' => break,
            c => text.push(c),
        }
    }
    if text.is_empty() || std::str::from_utf8(&text).is_err() {
        return None;
    }
    let mut source = BOM.to_vec();
    source.extend_from_slice(&text);
    Some(Module {
        kind: ModuleKind::FormModule,
        source,
    })
}

fn load_child(cx: &LoadCtx, c: &mut Child) -> Result<()> {
    if c.tag == crate::model::ChildTag::Command {
        c.module = container_text(cx, &format!("{}.2", c.id))?.map(|source| Module {
            kind: ModuleKind::CommandModule,
            source,
        });
    }
    Ok(())
}

fn load_object(cx: &LoadCtx, o: &mut MdObject, diag: &mut Diagnostics) -> Result<()> {
    for (suf, file) in cx.schema.modules(o.kind.as_str()) {
        let Some(kind) = kind_of_file(file) else {
            continue;
        };
        if let Some(source) = container_text(cx, &format!("{}.{suf}", o.id))? {
            o.modules.push(Module { kind, source });
        }
    }
    for c in &mut o.children {
        load_child(cx, c)?;
    }
    for r in &mut o.recalculations {
        r.module = container_text(cx, &format!("{}.0", r.id))?.map(|source| Module {
            kind: ModuleKind::RecordSetModule,
            source,
        });
    }
    if o.kind == crate::MdKind::CommonForm {
        o.modules.extend(form_module(cx, o.id, diag)?);
    }
    for f in &mut o.forms {
        f.module = form_module(cx, f.id, diag)?;
    }
    for s in &mut o.subsystems {
        load_object(cx, s, diag)?;
    }
    Ok(())
}

pub(crate) fn slot_kinds(schema: &'static mdschema::Schema, kind: &str) -> Vec<ModuleKind> {
    schema
        .modules(kind)
        .iter()
        .filter_map(|(_, file)| kind_of_file(file))
        .collect()
}

pub(crate) fn entry_slots<'c>(
    schema: &'static mdschema::Schema,
    cfg: &'c Configuration,
) -> Vec<(String, Option<&'c Module>)> {
    fn object<'c>(
        schema: &'static mdschema::Schema,
        o: &'c MdObject,
        out: &mut Vec<(String, Option<&'c Module>)>,
    ) {
        for (suf, file) in schema.modules(o.kind.as_str()) {
            let Some(kind) = kind_of_file(file) else {
                continue;
            };
            let module = o.modules.iter().find(|m| m.kind == kind);
            out.push((format!("{}.{suf}", o.id), module));
        }
        for c in &o.children {
            if c.tag == crate::model::ChildTag::Command {
                out.push((format!("{}.2", c.id), c.module.as_ref()));
            }
        }
        for r in &o.recalculations {
            out.push((format!("{}.0", r.id), r.module.as_ref()));
        }
        for s in &o.subsystems {
            object(schema, s, out);
        }
    }
    let mut out = Vec::new();
    for o in &cfg.objects {
        object(schema, o, &mut out);
    }
    for (suf, kind, _) in CONFIG_MODULES {
        let module = cfg.modules.iter().find(|m| m.kind == kind);
        out.push((format!("{}.{suf}", cfg.header_id), module));
    }
    out
}

pub fn load(cx: &LoadCtx, cfg: &mut Configuration) -> Result<()> {
    let mut objects = std::mem::take(&mut cfg.objects);
    let diag = &mut cfg.diagnostics;
    let r = objects
        .iter_mut()
        .try_for_each(|o| load_object(cx, o, diag));
    cfg.objects = objects;
    r?;
    let mut mods = Vec::new();
    for (suf, kind, _) in CONFIG_MODULES {
        if let Some(source) = container_text(cx, &format!("{}.{suf}", cfg.header_id))? {
            mods.push(Module { kind, source });
        }
    }
    cfg.modules = mods;
    Ok(())
}
