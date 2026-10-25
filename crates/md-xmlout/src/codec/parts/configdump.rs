use super::XML_HEAD;
use crate::{DUMP_VERSION, DumpSink, Part, PartStatus, Result, esc_attr};
use brace::Guid;
use md_model::parts::*;
use md_model::*;
use std::collections::{BTreeSet, HashMap, HashSet};

pub type R<T> = std::result::Result<T, String>;

pub fn cfg_version(g: Guid) -> String {
    let mut s: String = g.to_bytes_le().iter().map(|b| format!("{b:02x}")).collect();
    s.push_str("00000000");
    s
}

pub fn guid_key(s: &str) -> Option<Guid> {
    Guid::parse(s).filter(|g| g.to_string() == s)
}

fn head(full: &str, segs: usize) -> Option<&str> {
    let (i, _) = full.match_indices('.').nth(segs.checked_sub(1)?)?;
    full.get(..i)
}

fn child_index(cfg: &Configuration) -> HashMap<Guid, Vec<(Guid, &str)>> {
    let ix = &cfg.index;
    let named = |u: &Guid| Some((ix.names.get(u)?.as_str(), *u));
    let owners: HashMap<&str, Guid> = ix.order.iter().filter_map(named).collect();
    let recalc_ids: HashSet<Guid> = ix.recalcs.values().flatten().copied().collect();
    let recalcs: HashMap<&str, Guid> = recalc_ids.iter().filter_map(named).collect();
    let mut kids: HashMap<Guid, Vec<(Guid, &str)>> = HashMap::new();
    for (u, full) in &ix.names {
        if ix.objs.contains_key(u) || ix.sub_hdr.contains_key(u) || recalc_ids.contains(u) {
            continue;
        }
        let owner = head(full, 4)
            .and_then(|r| recalcs.get(r))
            .or_else(|| head(full, 2).and_then(|o| owners.get(o)));
        if let Some(o) = owner {
            kids.entry(*o).or_default().push((*u, full.as_str()));
        }
    }
    kids
}

fn owner_kind_path(full: &str) -> String {
    full.split('.').step_by(2).collect::<Vec<_>>().join(".")
}

fn generic_subnames(
    tables: &mdschema::rights::RightsTables,
) -> HashMap<String, HashMap<String, String>> {
    let mut seen: HashMap<&str, HashMap<&str, BTreeSet<&str>>> = HashMap::new();
    let table = tables
        .configdump
        .get("subnames")
        .and_then(|s| s.as_object());
    for (kp, d) in table.into_iter().flatten() {
        let Some((_, child)) = kp.split_once('.') else {
            continue;
        };
        for (suf, nm) in d.as_object().into_iter().flatten() {
            if let Some(nm) = nm.as_str() {
                seen.entry(child)
                    .or_default()
                    .entry(suf.as_str())
                    .or_default()
                    .insert(nm);
            }
        }
    }
    seen.into_iter()
        .map(|(c, d)| {
            let one = d
                .into_iter()
                .filter_map(|(suf, v)| match v.len() {
                    1 => v.first().map(|nm| (suf.to_string(), nm.to_string())),
                    _ => None,
                })
                .collect();
            (c.to_string(), one)
        })
        .collect()
}

const EXTRA_SUBNAMES: [(&str, &str, &str); 1] =
    [("CalculationRegister.Recalculation", "0", "RecordSetModule")];

pub struct Names<'a> {
    cfg: &'a Configuration,
    cfg_full: String,
    tables: &'a mdschema::rights::RightsTables,
    generic: HashMap<String, HashMap<String, String>>,
}

impl<'a> Names<'a> {
    pub fn new(cfg: &'a Configuration) -> R<Self> {
        let tables = mdschema::rights::rights_tables().map_err(|_| "tables".to_string())?;
        Ok(Names {
            cfg,
            cfg_full: format!("Configuration.{}", cfg.settings.name),
            tables,
            generic: generic_subnames(tables),
        })
    }

    fn sub(&self, kp: &str, suf: &str) -> Option<&str> {
        let table = || {
            self.tables
                .configdump
                .get("subnames")?
                .get(kp)?
                .get(suf)?
                .as_str()
        };
        table().or_else(|| {
            EXTRA_SUBNAMES
                .iter()
                .find(|(k, s, _)| *k == kp && *s == suf)
                .map(|(_, _, n)| *n)
        })
    }

    pub fn element(&self, name: &str) -> R<String> {
        let (u, suf) = match name.split_once('.') {
            Some((u, s)) => (u, Some(s)),
            None => (name, None),
        };
        let owner = if u == self.cfg.id.to_string() || u == self.cfg.header_id.to_string() {
            self.cfg_full.as_str()
        } else {
            guid_key(u)
                .and_then(|g| self.cfg.full_name(g))
                .ok_or_else(|| format!("element:{name}"))?
        };
        let Some(suf) = suf else {
            return Ok(owner.to_string());
        };
        let kp = owner_kind_path(owner);
        let mut sub = self.sub(&kp, suf);
        if sub.is_none() && kp.starts_with("Subsystem") {
            sub = self.sub("Subsystem", suf);
        }
        if sub.is_none()
            && let Some((_, child)) = kp.split_once('.')
        {
            sub = self
                .generic
                .get(child)
                .and_then(|d| d.get(suf))
                .map(String::as_str);
        }
        let sub = sub.ok_or_else(|| format!("suffix:{kp}:{suf}"))?;
        Ok(format!("{owner}.{sub}"))
    }
}

pub fn convert(cfg: &Configuration, d: &ConfigDumpInfo) -> R<String> {
    let names = Names::new(cfg)?;
    let kids = child_index(cfg);
    let mut entries = Vec::new();
    for (name, ver) in &d.versions {
        if name.is_empty() || matches!(name.as_str(), "root" | "version" | "versions") {
            continue;
        }
        entries.push((names.element(name)?, name.as_str(), *ver));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let mut lines = Vec::with_capacity(entries.len());
    for (full, name, ver) in &entries {
        let attrs = format!(
            "name=\"{}\" id=\"{name}\" configVersion=\"{}\"",
            esc_attr(full),
            cfg_version(*ver)
        );
        let ch = if name.contains('.') {
            None
        } else {
            guid_key(name).and_then(|g| kids.get(&g))
        };
        match ch.filter(|c| !c.is_empty()) {
            Some(ch) => {
                let mut ch = ch.clone();
                ch.sort();
                lines.push(format!("\t\t<Metadata {attrs}>"));
                for (cu, cn) in ch {
                    lines.push(format!(
                        "\t\t\t<Metadata name=\"{}\" id=\"{cu}\"/>",
                        esc_attr(cn)
                    ));
                }
                lines.push("\t\t</Metadata>".into());
            }
            None => lines.push(format!("\t\t<Metadata {attrs}/>")),
        }
    }
    Ok(format!(
        "{XML_HEAD}\r\n<ConfigDumpInfo xmlns=\"http://v8.1c.ru/8.3/xcf/dumpinfo\" xmlns:xen=\"http://v8.1c.ru/8.3/xcf/enums\" \
         xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" \
         format=\"Hierarchical\" version=\"{DUMP_VERSION}\">\r\n\t<ConfigVersions>\r\n{}\r\n\t</ConfigVersions>\r\n</ConfigDumpInfo>",
        lines.join("\r\n")
    ))
}

pub fn configdump_xml(cfg: &Configuration, d: &ConfigDumpInfo) -> Option<String> {
    convert(cfg, d).ok()
}

pub fn dump(cfg: &Configuration, sink: &mut DumpSink) -> Result<PartStatus> {
    if let Some(xml) = cfg
        .parts
        .dump_info
        .as_ref()
        .and_then(|d| configdump_xml(cfg, d))
    {
        sink.put(Part::Configuration, "ConfigDumpInfo.xml", xml.as_bytes())?;
    }
    Ok(PartStatus::Written)
}
