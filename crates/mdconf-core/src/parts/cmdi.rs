use md_xmlout::codec::parts::cmdi::*;

use super::{CommandInterface, RawPart, fetch_brace, find_mut, miss};
use crate::load::LoadCtx;
use crate::{Configuration, MdKind, Result};
use brace::Guid;

pub(crate) fn load(cx: &LoadCtx, cfg: &mut Configuration) -> Result<()> {
    let ids: Vec<Guid> = cfg.of_kind(MdKind::Subsystem).map(|o| o.id).collect();
    for id in ids {
        let Some(node) = fetch_brace(cx, cfg, &format!("{id}.1"), &["cmdi", "ci"])? else {
            continue;
        };
        match convert_ci(cfg, &node) {
            Ok(_) => {
                if let Some(o) = find_mut(&mut cfg.objects, id) {
                    o.parts.command_interface = Some(CommandInterface { source: node });
                }
            }
            Err(why) => miss(cfg, &["cmdi", "ci", why]),
        }
    }
    let h = cfg.header_id;
    for (suffix, file) in [("a", "ci"), ("9", "main_ci")] {
        let Some(node) = fetch_brace(cx, cfg, &format!("{h}.{suffix}"), &["cmdi", file])? else {
            continue;
        };
        match convert_ci(cfg, &node) {
            Ok(_) => {
                let ci = Some(CommandInterface { source: node });
                if suffix == "a" {
                    cfg.parts.command_interface = ci;
                } else {
                    cfg.parts.main_section = ci;
                }
            }
            Err(why) => miss(cfg, &["cmdi", file, why]),
        }
    }
    if let Some(node) = fetch_brace(cx, cfg, &format!("{h}.8"), &["cmdi", "hp"])? {
        match convert_homepage(cfg, &node) {
            Ok(_) => cfg.parts.home_page = Some(RawPart { node }),
            Err(why) => miss(cfg, &["cmdi", "hp", why]),
        }
    }
    if let Some(node) = fetch_brace(cx, cfg, &format!("{h}.b"), &["cmdi", "cai"])? {
        match convert_cai(&node) {
            Ok(_) => cfg.parts.client_interface = Some(RawPart { node }),
            Err(why) => miss(cfg, &["cmdi", "cai", why]),
        }
    }
    Ok(())
}
