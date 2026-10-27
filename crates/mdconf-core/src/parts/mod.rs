use crate::entries::EntryData;
use crate::load::LoadCtx;
use crate::{Configuration, MdObject, Result};
use brace::{Guid, Node};
use memory_budget::profile::{Phase, phase};
use std::sync::Arc;

mod cmdi;
pub(crate) mod configdump;
mod configuration;
mod help;
mod misc;
mod pictures;
mod predefined;
mod rights;

pub(crate) use md_model::parts::MiscSource;
pub use md_model::parts::{
    AccountData, AccountType, CalcTypeData, CommandInterface, ConfigDumpInfo, ConfigParts,
    ContentItem, ExtDimensionType, Help, ObjectParts, ObjectRights, Picture, Predefined,
    PredefinedCode, PredefinedItem, RawPart, Restriction, Right, RightTarget, RightValue, Rights,
    Schedule, Style,
};
use md_xmlout::codec::parts::{R, count, int, uint};
pub(crate) use pictures::ext_of as picture_extension;

pub(crate) fn load(cx: &LoadCtx, cfg: &mut Configuration, stages: crate::Stages) -> Result<()> {
    use crate::Stages;
    if stages.contains(Stages::RIGHTS) {
        let _phase = phase(Phase::Rights);
        rights::load(cx, cfg)?;
    }
    {
        let _phase = phase(Phase::PredefinedData);
        predefined::load(cx, cfg)?;
    }
    if stages.contains(Stages::COMMAND_INTERFACE) {
        cmdi::load(cx, cfg)?;
    }
    {
        let _phase = phase(Phase::ConfigurationParts);
        configuration::decode_configuration(cx, cfg)?;
    }
    if stages.contains(Stages::CONFIG_DUMP) {
        configdump::load(cx, cfg)?;
    }
    {
        let _phase = phase(Phase::MiscParts);
        misc::load(cx, cfg)?;
    }
    if stages.contains(Stages::PICTURES) {
        pictures::load(cx, cfg)?;
    }
    if stages.contains(Stages::HELP) {
        help::load(cx, cfg)?;
    }
    Ok(())
}

fn miss(cfg: &mut Configuration, key: &[&str]) {
    *cfg.diagnostics.misses.entry(key.join(":")).or_insert(0) += 1;
}

fn fetch(cx: &LoadCtx, name: &str) -> Result<Option<Arc<EntryData>>> {
    let data = cx.entries.get(name)?;
    if !cx.opts.keep_source {
        cx.entries.forget(name);
    }
    Ok(data)
}

fn fetch_brace(
    cx: &LoadCtx,
    cfg: &mut Configuration,
    name: &str,
    what: &[&str],
) -> Result<Option<Node>> {
    match fetch(cx, name)?.as_deref() {
        Some(EntryData::Brace(doc)) => Ok(Some(doc.root.clone())),
        Some(_) => {
            miss(cfg, &[what, &["not_brace"]].concat());
            Ok(None)
        }
        None => Ok(None),
    }
}

fn find_mut(objs: &mut [MdObject], id: Guid) -> Option<&mut MdObject> {
    for o in objs {
        if o.id == id {
            return Some(o);
        }
        if let Some(f) = find_mut(&mut o.subsystems, id) {
            return Some(f);
        }
    }
    None
}

fn slice(l: &[Node], from: usize, n: usize) -> &[Node] {
    let tail = l.get(from..).unwrap_or_default();
    tail.get(..n.min(tail.len())).unwrap_or(tail)
}

fn plain_path(s: &str) -> bool {
    s.split('/')
        .all(|c| !c.is_empty() && c != "." && c != ".." && !c.contains(['\\', ':', '\0']))
}
