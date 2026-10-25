pub(crate) use md_xmlout::codec::parts::configdump::Names;
use md_xmlout::codec::parts::configdump::*;

use super::{ConfigDumpInfo, fetch, miss};
use crate::entries::EntryData;
use crate::load::LoadCtx;
use crate::{Configuration, Result};
use brace::{Guid, Node};

const VERSIONS: &str = "versions";

fn parse_versions(t: &Node, misses: &mut Vec<String>) -> R<Vec<(String, Guid)>> {
    let bad = || "versions".to_string();
    let l = t.as_list().ok_or_else(bad)?;
    let n: i64 = l
        .get(1)
        .and_then(Node::text)
        .and_then(|s| s.trim().parse().ok())
        .ok_or_else(bad)?;
    let n = usize::try_from(n).unwrap_or(0);
    let mut out = Vec::new();
    for i in 0..n {
        let at = |k: usize| {
            i.checked_mul(2)
                .and_then(|j| j.checked_add(k))
                .and_then(|j| l.get(j))
                .and_then(Node::text)
                .ok_or_else(bad)
        };
        let (name, ver) = (at(2)?, at(3)?);
        match Guid::parse(ver) {
            Some(g) => out.push((name.to_string(), g)),
            None if name.is_empty() => misses.push("version".into()),
            None => return Err(format!("version:{name}")),
        }
    }
    Ok(out)
}

pub(crate) fn load(cx: &LoadCtx, cfg: &mut Configuration) -> Result<()> {
    let data = fetch(cx, VERSIONS)?;
    let t = match data.as_deref() {
        Some(EntryData::Brace(doc)) => &doc.root,
        _ if !cx.entries.contains(VERSIONS) => {
            miss(cfg, &["configdump", "no_entry"]);
            return Ok(());
        }
        _ => {
            miss(cfg, &["configdump", "not_brace"]);
            return Ok(());
        }
    };
    let mut misses = Vec::new();
    match parse_versions(t, &mut misses) {
        Ok(versions) => {
            let d = ConfigDumpInfo {
                versions,
                source: if cx.opts.keep_source {
                    t.clone()
                } else {
                    Node::empty()
                },
            };
            if let Err(why) = convert(cfg, &d) {
                misses.push(why);
            }
            cfg.parts.dump_info = Some(d);
        }
        Err(why) => misses.push(why),
    }
    for m in misses {
        miss(cfg, &["configdump", &m]);
    }
    Ok(())
}
