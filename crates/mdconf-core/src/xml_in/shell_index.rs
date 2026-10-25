use super::{
    shell_xml::*,
    walk::{err, nfc},
};
use crate::{
    Diagnostics, GeneratedType, Result,
    index::{Index, TypeOwner},
};
use brace::Guid;
use mdschema::{InternalInfoSpec, Schema};
use sha1::{Digest, Sha1};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

pub(super) struct Identities {
    metadata_ids: HashSet<Guid>,
    part_ids: HashSet<Guid>,
    type_ids: HashSet<Guid>,
    value_ids: HashSet<Guid>,
    total: usize,
    names: HashMap<String, Guid>,
    type_names: HashSet<String>,
    limit: usize,
    max_full_name_bytes: usize,
    max_total_name_bytes: usize,
    name_bytes: usize,
    pub count: usize,
}
enum Space {
    Metadata,
    Part,
    Type,
    Value,
}
impl Identities {
    pub fn new(options: &super::ShellOptions) -> Self {
        Self {
            metadata_ids: HashSet::new(),
            part_ids: HashSet::new(),
            type_ids: HashSet::new(),
            value_ids: HashSet::new(),
            total: 0,
            names: HashMap::new(),
            type_names: HashSet::new(),
            limit: options.max_identities,
            max_full_name_bytes: options.max_full_name_bytes,
            max_total_name_bytes: options.max_total_name_bytes,
            name_bytes: 0,
            count: 0,
        }
    }
    pub fn full_name(&mut self, parts: &[&str], path: &Path) -> Result<String> {
        let len = parts
            .iter()
            .try_fold(0usize, |n, part| n.checked_add(part.len()))
            .ok_or_else(|| err(path, "name byte limit"))?;
        if len > self.max_full_name_bytes {
            return Err(err(path, "name byte limit"));
        }
        if len > self.max_total_name_bytes.saturating_sub(self.name_bytes) {
            return Err(err(path, "total name byte limit"));
        }
        self.name_bytes += len;
        Ok(parts.concat())
    }
    fn reserve(&mut self, id: Guid, space: Space, path: &Path) -> Result<()> {
        if self.total >= self.limit {
            return Err(err(path, "identity count limit"));
        }
        let ids = match space {
            Space::Metadata => &mut self.metadata_ids,
            Space::Part => &mut self.part_ids,
            Space::Type => &mut self.type_ids,
            Space::Value => &mut self.value_ids,
        };
        if id.is_nil() || !ids.insert(id) {
            return Err(err(path, format!("nil or duplicate UUID {id}")));
        }
        self.total += 1;
        Ok(())
    }
    pub fn part(&mut self, id: Guid, path: &Path) -> Result<()> {
        self.reserve(id, Space::Part, path)
    }
    pub fn identity(&mut self, n: N<'_, '_>, full: &str, path: &Path) -> Result<Guid> {
        let id = attr(n, None, "uuid")
            .and_then(Guid::parse)
            .ok_or_else(|| err(path, "missing or invalid uuid"))?;
        self.reserve(id, Space::Metadata, path)?;
        let key = nfc(full).to_lowercase();
        if self.names.insert(key, id).is_some() {
            return Err(err(path, "duplicate metadata name"));
        }
        self.count += 1;
        Ok(id)
    }
    #[allow(
        clippy::too_many_arguments,
        reason = "Schema extraction carries the source, owner and destination diagnostics explicitly"
    )]
    pub fn generated(
        &mut self,
        n: N<'_, '_>,
        expected: &[InternalInfoSpec],
        tag: &str,
        name: &str,
        owner: TypeOwner,
        s: &Schema,
        ix: &mut Index,
        diag: &mut Diagnostics,
        path: &Path,
    ) -> Result<Vec<GeneratedType>> {
        let mut out = Vec::new();
        let mut cats = HashSet::new();
        if let Some(ii) = block(n, MD, "InternalInfo", path)? {
            for gt in ii.children() {
                if gt.ns() == Some(XR) && gt.local() == "ThisNode" {
                    if owner.child.is_some() || owner.recalc.is_some() {
                        return Err(err(path, "unsupported subordinate ThisNode"));
                    }
                    continue;
                }
                if gt.ns() != Some(XR) || gt.local() != "GeneratedType" {
                    return Err(err(path, "unknown InternalInfo element"));
                }
                attributes(gt, &["category", "name"], path)?;
                let category = attr(gt, None, "category")
                    .ok_or_else(|| err(path, "missing generated category"))?;
                if !cats.insert(category.to_string())
                    || !expected.iter().any(|e| e.category == category)
                {
                    return Err(err(path, "unknown or duplicate generated category"));
                }
                let prefix = s
                    .gt_name(category)
                    .map_or_else(|| format!("{tag}{category}"), String::from);
                let full = self.full_name(&[&prefix, ".", name], path)?;
                if attr(gt, None, "name").map(nfc).as_deref() != Some(full.as_str()) {
                    return Err(err(
                        path,
                        format!("wrong generated type owner: expected {full}"),
                    ));
                }
                let tid = guid_text(required(gt, XR, "TypeId", path)?, path)?;
                let vid = guid_text(required(gt, XR, "ValueId", path)?, path)?;
                if gt.children().count() != 2 {
                    return Err(err(path, "unknown generated type element"));
                }
                self.add_type(tid, vid, &full, category, &owner, ix, path)?;
                out.push(GeneratedType {
                    category: category.into(),
                    type_id: tid,
                    value_id: vid,
                });
            }
        }
        if tag == "Constant" && !cats.contains("ValueKey") {
            let tid = uuid5(owner.object, "typeId");
            let vid = uuid5(owner.object, "valueId");
            let full = self.full_name(&[tag, "ValueKey.", name], path)?;
            self.add_type(tid, vid, &full, "ValueKey", &owner, ix, path)?;
            out.push(GeneratedType {
                category: "ValueKey".into(),
                type_id: tid,
                value_id: vid,
            });
            cats.insert("ValueKey".into());
        }
        for e in expected {
            if !cats.contains(&e.category) {
                *diag
                    .misses
                    .entry(format!("missing-generated:{tag}.{}", e.category))
                    .or_default() += 1;
            }
        }
        Ok(out)
    }
    #[allow(
        clippy::too_many_arguments,
        reason = "Schema extraction carries the source, owner and destination diagnostics explicitly"
    )]
    fn add_type(
        &mut self,
        tid: Guid,
        vid: Guid,
        name: &str,
        category: &str,
        owner: &TypeOwner,
        ix: &mut Index,
        path: &Path,
    ) -> Result<()> {
        self.reserve(tid, Space::Type, path)?;
        self.reserve(vid, Space::Value, path)?;
        if !self.type_names.insert(nfc(name).to_lowercase()) {
            return Err(err(path, "duplicate generated type name"));
        }
        ix.types.insert(tid, name.into());
        ix.type_owners.insert(
            tid,
            TypeOwner {
                category: category.into(),
                ..owner.clone()
            },
        );
        Ok(())
    }
}
pub(super) fn guid_text(n: N<'_, '_>, path: &Path) -> Result<Guid> {
    let id = Guid::parse(leaf(n, path)?).ok_or_else(|| err(path, "invalid UUID text"))?;
    if id.is_nil() {
        return Err(err(path, "nil UUID"));
    }
    Ok(id)
}
pub(super) fn uuid5(namespace: Guid, name: &str) -> Guid {
    let digest = Sha1::new()
        .chain_update(namespace.as_bytes())
        .chain_update(name.as_bytes())
        .finalize();
    let mut bytes = [0u8; 16];
    for (to, from) in bytes.iter_mut().zip(digest.iter()) {
        *to = *from;
    }
    if let Some(b) = bytes.get_mut(6) {
        *b = (*b & 0x0f) | 0x50;
    }
    if let Some(b) = bytes.get_mut(8) {
        *b = (*b & 0x3f) | 0x80;
    }
    Guid::from_bytes(bytes)
}
