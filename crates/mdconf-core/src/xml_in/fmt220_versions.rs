use super::{PartsOptions, parse_part, shell_xml, walk::err};
use crate::{Configuration, Result, parts::ConfigDumpInfo};
use brace::{Guid, Node};
use memory_budget::{Limits, MemoryBudget};
use std::{
    collections::{BTreeSet, HashMap},
    path::Path,
};
const DI: &str = "http://v8.1c.ru/8.3/xcf/dumpinfo";

fn child_owners(cfg: &Configuration) -> HashMap<Guid, Guid> {
    fn children(items: &[crate::Child], owner: Guid, owners: &mut HashMap<Guid, Guid>) {
        for child in items {
            owners.insert(child.id, owner);
            children(&child.children, owner, owners);
        }
    }
    fn objects(items: &[crate::MdObject], owners: &mut HashMap<Guid, Guid>) {
        for object in items {
            children(&object.children, object.id, owners);
            for recalc in &object.recalculations {
                for dimension in &recalc.dimensions {
                    owners.insert(dimension.id, recalc.id);
                }
            }
            objects(&object.subsystems, owners);
        }
    }
    let mut owners = HashMap::new();
    objects(&cfg.objects, &mut owners);
    owners
}

fn token(value: &str, path: &Path) -> Result<Guid> {
    if value.len() != 40 || !value.is_ascii() || !value.ends_with("00000000") {
        return Err(err(path, "invalid or unrepresentable configVersion token"));
    }
    let mut bytes = [0u8; 16];
    let hex = value
        .get(..32)
        .ok_or_else(|| err(path, "invalid configVersion width"))?;
    for (byte, pair) in bytes.iter_mut().zip(hex.as_bytes().as_chunks::<2>().0) {
        *byte = std::str::from_utf8(pair)
            .ok()
            .and_then(|s| u8::from_str_radix(s, 16).ok())
            .ok_or_else(|| err(path, "invalid configVersion hex"))?;
    }
    Ok(Guid::from_bytes_le(bytes))
}
fn identity<'a>(
    n: shell_xml::N<'a, '_>,
    names: &md_xmlout::codec::parts::configdump::Names<'_>,
    seen_ids: &mut BTreeSet<String>,
    seen_names: &mut BTreeSet<String>,
    path: &Path,
) -> Result<(&'a str, &'a str)> {
    if n.ns() != Some(DI)
        || n.local() != "Metadata"
        || n.text().is_some_and(|s| !s.trim().is_empty())
    {
        return Err(err(path, "invalid Metadata namespace or shape"));
    }
    let id = shell_xml::attr(n, None, "id").ok_or_else(|| err(path, "missing Metadata id"))?;
    let name =
        shell_xml::attr(n, None, "name").ok_or_else(|| err(path, "missing Metadata name"))?;
    if !seen_ids.insert(id.into()) || !seen_names.insert(name.into()) {
        return Err(err(path, "duplicate Metadata id or name"));
    }
    if names
        .element(id)
        .ok()
        .is_none_or(|expected| expected != name)
    {
        return Err(err(path, "unresolved or mismatched named version identity"));
    }
    Ok((id, name))
}
pub fn read_named_versions(
    cfg: &Configuration,
    bytes: &[u8],
    o: &PartsOptions,
) -> Result<ConfigDumpInfo> {
    let path = Path::new("ConfigDumpInfo.xml");
    if bytes.len() as u64 > o.max_retained_file_bytes {
        return Err(err(path, "ConfigDumpInfo byte limit"));
    }
    let budget = MemoryBudget::new(Limits::new(o.objects.shells.max_xml_memory));
    let arena = parse_part(bytes, path, &o.objects.shells, &budget, &mut 0)?;
    let root = arena
        .root()
        .ok_or_else(|| err(path, "missing ConfigDumpInfo"))?;
    if root.ns() != Some(DI) || root.local() != "ConfigDumpInfo" {
        return Err(err(path, "wrong ConfigDumpInfo namespace or root"));
    }
    shell_xml::attributes(root, &["format", "version"], path)?;
    if shell_xml::attr(root, None, "format") != Some("Hierarchical")
        || !matches!(
            shell_xml::attr(root, None, "version"),
            Some("2.20" | "2.21")
        )
    {
        return Err(err(path, "unsupported ConfigDumpInfo format/version"));
    }
    let entries = shell_xml::required(root, DI, "ConfigVersions", path)?;
    if root.children().count() != 1 || root.text().is_some_and(|s| !s.trim().is_empty()) {
        return Err(err(path, "unknown ConfigDumpInfo child/content"));
    }
    shell_xml::attributes(entries, &[], path)?;
    if entries.text().is_some_and(|s| !s.trim().is_empty()) {
        return Err(err(path, "invalid ConfigVersions text"));
    }
    let names = md_xmlout::codec::parts::configdump::Names::new(cfg)
        .map_err(|_| err(path, "version name index"))?;
    let owners = child_owners(cfg);
    let mut seen_ids = BTreeSet::new();
    let mut seen_names = BTreeSet::new();
    let mut versions = Vec::new();
    for entry in entries.children() {
        shell_xml::attributes(entry, &["name", "id", "configVersion"], path)?;
        let (id, _) = identity(entry, &names, &mut seen_ids, &mut seen_names, path)?;
        let value = shell_xml::attr(entry, None, "configVersion")
            .ok_or_else(|| err(path, "missing named configVersion"))?;
        versions.push((id.into(), token(value, path)?));
        for child in entry.children() {
            shell_xml::attributes(child, &["name", "id"], path)?;
            let (child_id, _) = identity(child, &names, &mut seen_ids, &mut seen_names, path)?;
            let owned = Guid::parse(id)
                .zip(Guid::parse(child_id))
                .is_some_and(|(owner, child)| owners.get(&child) == Some(&owner));
            if !owned || child.children().next().is_some() {
                return Err(err(path, "invalid Metadata child ownership"));
            }
        }
    }
    Ok(ConfigDumpInfo {
        versions,
        source: Node::atom(""),
    })
}
