pub(crate) use crate::{allocation, codec_base64};
pub(crate) mod context;
mod dcs;
mod files;
mod kind;
mod links;
pub(crate) mod moxel;
pub(crate) mod moxel_view;
pub(crate) mod mxltables;
pub(crate) mod mxlxml;
mod simple;
pub(crate) mod tree;
mod type_view;
mod xdto;

use crate::load::LoadCtx;
use crate::xml_out::{DumpSink, Part, PartStatus, paths};
use crate::{Configuration, MdKind, MdObject, Result};

pub use md_model::templates::{Template, TemplateType};

type Fail = &'static str;

fn convert(
    cfg: &Configuration,
    data: &crate::entries::EntryData,
    raw: &[u8],
    kind: TemplateType,
) -> std::result::Result<Vec<(String, Vec<u8>)>, Fail> {
    const FAILED: Fail = "failed";
    match kind {
        TemplateType::ActiveDocument | TemplateType::GeographicalSchema => Ok(Vec::new()),
        TemplateType::SpreadsheetDocument => {
            let bytes = entry_bytes(data, raw).ok_or(FAILED)?;
            let converted = mxlxml::entry_xml(cfg, bytes);
            let xml = match converted {
                Ok(xml) => xml,
                Err(_) => crate::graph::sheet::sheet_xml(cfg, bytes)
                    .ok()
                    .ok_or(FAILED)?,
            };
            Ok(vec![("Template.xml".to_string(), xml)])
        }
        TemplateType::GraphicalSchema => {
            let xml = crate::graph::schema_template(cfg, data).ok_or(FAILED)?;
            Ok(vec![("Template.xml".to_string(), xml)])
        }
        TemplateType::DataCompositionSchema => {
            let bytes = entry_bytes(data, raw).ok_or(FAILED)?;
            let xml = dcs::convert(cfg, bytes).ok_or(FAILED)?;
            Ok(vec![("Template.xml".to_string(), xml)])
        }
        _ => simple::convert_simple(cfg, data, raw, kind),
    }
}

pub(crate) fn convert_with(
    lookup: &dyn context::Lookup,
    data: &crate::EntryData,
    raw: &[u8],
    kind: TemplateType,
    allocation: allocation::Allocation<'_>,
) -> crate::graph::read::R<allocation::Owned<Vec<files::File>>> {
    use crate::graph::read::{Read, ReadError};
    let read = Read::new(allocation);
    let xml = match kind {
        TemplateType::ActiveDocument | TemplateType::GeographicalSchema => {
            return Ok(allocation.vector(0)?.finish());
        }
        TemplateType::SpreadsheetDocument => crate::graph::sheet::template_xml_with(
            lookup,
            entry_bytes(data, raw).ok_or_else(|| read.invalid("failed"))?,
            allocation,
        )?,
        TemplateType::GraphicalSchema => {
            let tree =
                tree::entry(data, allocation).map_err(|error| mxlxml::source_error(error, read))?;
            crate::graph::gschema::to_xml_with(lookup, tree.root(), None, allocation)?
        }
        TemplateType::DataCompositionSchema => dcs::convert_with(
            entry_bytes(data, raw).ok_or_else(|| read.invalid("failed"))?,
            lookup,
            allocation,
        )?,
        _ => {
            return simple::convert_with(allocation, lookup, data, raw, kind).map_err(|error| {
                match error {
                    simple::SimpleError::Memory(error) => ReadError::Memory(error),
                    simple::SimpleError::Path => read.invalid("path"),
                    simple::SimpleError::Failed => read.invalid("failed"),
                }
            });
        }
    };
    let mut files = files::FileSet::new(allocation)?;
    files.put(&["Template.xml"], xml)?;
    Ok(files.finish()?)
}

type Slot = (Vec<usize>, Option<usize>);

fn object_at<'a>(cfg: &'a mut Configuration, path: &[usize]) -> Option<&'a mut MdObject> {
    let (first, rest) = path.split_first()?;
    let mut o = cfg.objects.get_mut(*first)?;
    for &i in rest {
        o = o.subsystems.get_mut(i)?;
    }
    Some(o)
}

fn collect(
    cfg: &Configuration,
    cx: &LoadCtx,
    o: &MdObject,
    path: &mut Vec<usize>,
    out: &mut Vec<(Slot, Template)>,
    misses: &mut Vec<String>,
) -> Result<()> {
    let mut one = |id: brace::Guid, common: bool| -> Result<Option<Template>> {
        let Some(kind) = kind::template_type(cfg, cx.entries, id, common) else {
            return Ok(None);
        };
        let name = format!("{id}.0");
        let files = match (cx.entries.get(&name)?, cx.entries.raw(&name)?) {
            (Some(data), Some(raw)) => match convert(cfg, &data, &raw, kind) {
                Ok(files) => files,
                Err(why) => {
                    misses.push(format!("templates:{}:{why}", kind.name()));
                    Vec::new()
                }
            },
            _ => Vec::new(),
        };
        if !cx.opts.keep_source {
            cx.entries.forget(&name);
        }
        Ok(Some(Template {
            kind: Some(kind),
            files,
        }))
    };
    if o.kind == MdKind::CommonTemplate
        && let Some(t) = one(o.id, true)?
    {
        out.push(((path.clone(), None), t));
    }
    for (i, s) in o.templates.iter().enumerate() {
        if let Some(t) = one(s.id, false)? {
            out.push(((path.clone(), Some(i)), t));
        }
    }
    for (i, s) in o.subsystems.iter().enumerate() {
        path.push(i);
        collect(cfg, cx, s, path, out, misses)?;
        path.pop();
    }
    Ok(())
}

#[doc(hidden)]
pub fn load(cx: &LoadCtx, cfg: &mut Configuration) -> Result<()> {
    let mut found = Vec::new();
    let mut misses = Vec::new();
    for (i, o) in cfg.objects.iter().enumerate() {
        collect(cfg, cx, o, &mut vec![i], &mut found, &mut misses)?;
    }
    for key in misses {
        *cfg.diagnostics.misses.entry(key).or_insert(0) += 1;
    }
    for ((path, sub), t) in found {
        let Some(o) = object_at(cfg, &path) else {
            continue;
        };
        match sub {
            None => o.template = Some(t),
            Some(i) => {
                if let Some(s) = o.templates.get_mut(i) {
                    s.template = Some(t);
                }
            }
        }
    }
    Ok(())
}

fn dump_object(cfg: &Configuration, sink: &mut DumpSink, o: &MdObject) -> Result<()> {
    let mut write = |dir: &str, t: &Template| -> Result<()> {
        for (path, data) in &t.files {
            sink.put(Part::Templates, &format!("{dir}/Ext/{path}"), data)?;
        }
        Ok(())
    };
    if let Some(t) = &o.template {
        write(&paths::object_dir(cfg, o), t)?;
    }
    for s in &o.templates {
        if let Some(t) = &s.template {
            write(&paths::sub_dir(cfg, o, s), t)?;
        }
    }
    for s in &o.subsystems {
        dump_object(cfg, sink, s)?;
    }
    Ok(())
}

#[doc(hidden)]
pub fn dump(cfg: &Configuration, sink: &mut DumpSink) -> Result<PartStatus> {
    for o in &cfg.objects {
        dump_object(cfg, sink, o)?;
    }
    Ok(PartStatus::Written)
}

pub(crate) fn entry_bytes<'a>(data: &crate::entries::EntryData, raw: &'a [u8]) -> Option<&'a [u8]> {
    match data {
        crate::entries::EntryData::Nested(_) => None,
        _ => Some(raw),
    }
}
