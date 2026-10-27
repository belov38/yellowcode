use crate::ManagedXmlWriter;
use brace::Guid;
use md_model::parts::ConfigDumpInfo;
use memory_budget::{BudgetError, BudgetedText, BudgetedVec, Category, Charged, MemoryBudget};
use std::fmt::Write;

pub trait ManagedDumpNames {
    fn configuration(&self) -> (Guid, Guid, &str);
    fn name(&self, id: Guid) -> Option<&str>;
    fn names(&self) -> impl Iterator<Item = (Guid, &str)>;
    fn owners(&self) -> &[Guid];
    fn recalculations(&self) -> impl Iterator<Item = Guid>;
    fn is_object(&self, id: Guid) -> bool;
    fn is_subobject(&self, id: Guid) -> bool;
}
enum Failure {
    Bad,
    Memory(BudgetError),
}
impl From<BudgetError> for Failure {
    fn from(error: BudgetError) -> Self {
        Self::Memory(error)
    }
}
type Result<T> = std::result::Result<T, Failure>;
fn text(budget: &MemoryBudget, parts: &[&str]) -> std::result::Result<BudgetedText, BudgetError> {
    let mut out = BudgetedText::new(budget, Category::Metadata, u64::MAX)?;
    for part in parts {
        out.try_push_str(part)?;
    }
    Ok(out)
}
fn canonical(raw: &str) -> Option<Guid> {
    if raw.len() != 36
        || !raw.bytes().enumerate().all(|(at, b)| {
            if [8, 13, 18, 23].contains(&at) {
                b == b'-'
            } else {
                b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
            }
        })
    {
        return None;
    }
    Guid::parse(raw)
}
fn head(name: &str, count: usize) -> Option<&str> {
    let (at, _) = name.match_indices('.').nth(count.checked_sub(1)?)?;
    name.get(..at)
}
fn table() -> Option<&'static mdschema::StaticValue> {
    mdschema::static_schema()
        .table("rights/configdump.json")?
        .get("subnames")
}
fn subname(path: &str, suffix: &str) -> Option<&'static str> {
    table()?
        .get(path)
        .and_then(|v| v.get(suffix))
        .and_then(|v| v.as_str())
        .or_else(|| {
            (path == "CalculationRegister.Recalculation" && suffix == "0")
                .then_some("RecordSetModule")
        })
}
fn generic(child: &str, suffix: &str) -> Option<&'static str> {
    let mut agreed = None;
    for (path, fields) in table()?.as_object()? {
        if path.split_once('.').map(|(_, tail)| tail) != Some(child) {
            continue;
        }
        if let Some(name) = fields.get(suffix).and_then(|v| v.as_str()) {
            match agreed {
                None => agreed = Some(name),
                Some(old) if old == name => {}
                Some(_) => return None,
            }
        }
    }
    agreed
}
fn element(
    budget: &MemoryBudget,
    names: &impl ManagedDumpNames,
    raw: &str,
) -> Result<BudgetedText> {
    let (id, suffix) = raw
        .split_once('.')
        .map_or((raw, None), |(id, suffix)| (id, Some(suffix)));
    let id = canonical(id).ok_or(Failure::Bad)?;
    let (configuration, header, name) = names.configuration();
    let root;
    let owner = if id == configuration || id == header {
        root = text(budget, &["Configuration.", name])?;
        root.as_str()
    } else {
        names.name(id).ok_or(Failure::Bad)?
    };
    let Some(suffix) = suffix else {
        return Ok(text(budget, &[owner])?);
    };
    let mut path = text(budget, &[])?;
    for (at, part) in owner.split('.').step_by(2).enumerate() {
        if at != 0 {
            path.try_push_str(".")?;
        }
        path.try_push_str(part)?;
    }
    let path = path.as_str();
    let sub = subname(path, suffix)
        .or_else(|| {
            path.starts_with("Subsystem")
                .then(|| subname("Subsystem", suffix))
                .flatten()
        })
        .or_else(|| {
            let (_, child) = path.split_once('.')?;
            generic(child, suffix)
        })
        .ok_or(Failure::Bad)?;
    Ok(text(budget, &[owner, ".", sub])?)
}
#[derive(Clone, Copy)]
struct Owner<'a> {
    name: &'a str,
    id: Guid,
    ordinal: usize,
}
fn last_owner(rows: &[Owner<'_>], name: &str) -> Option<Guid> {
    let end = rows.partition_point(|row| row.name <= name);
    rows.get(end.checked_sub(1)?)
        .filter(|row| row.name == name)
        .map(|row| row.id)
}
fn sort_owners(rows: &mut [Owner<'_>]) {
    rows.sort_unstable_by(|a, b| a.name.cmp(b.name).then(a.ordinal.cmp(&b.ordinal)));
}
#[derive(Clone, Copy)]
struct Child<'a> {
    owner: Guid,
    id: Guid,
    name: &'a str,
}
fn children<'a>(
    budget: &MemoryBudget,
    names: &'a impl ManagedDumpNames,
) -> std::result::Result<BudgetedVec<Child<'a>>, BudgetError> {
    let mut owners = BudgetedVec::new(budget, Category::Metadata)?;
    for (ordinal, id) in names.owners().iter().copied().enumerate() {
        if let Some(name) = names.name(id) {
            owners.try_push(Owner { name, id, ordinal })?;
        }
    }
    sort_owners(owners.as_mut_slice());
    let mut recalc_ids = BudgetedVec::new(budget, Category::Metadata)?;
    let mut recalcs = BudgetedVec::new(budget, Category::Metadata)?;
    for (ordinal, id) in names.recalculations().enumerate() {
        recalc_ids.try_push(id)?;
        if let Some(name) = names.name(id) {
            recalcs.try_push(Owner { name, id, ordinal })?;
        }
    }
    recalc_ids.sort_unstable();
    recalcs.sort_unstable_by(|a, b| a.name.cmp(b.name).then(a.id.cmp(&b.id)));
    let mut out = BudgetedVec::new(budget, Category::Metadata)?;
    for (id, name) in names.names() {
        if names.is_object(id) || names.is_subobject(id) || recalc_ids.binary_search(&id).is_ok() {
            continue;
        }
        let owner = head(name, 4)
            .and_then(|prefix| last_owner(&recalcs, prefix))
            .or_else(|| head(name, 2).and_then(|prefix| last_owner(&owners, prefix)));
        if let Some(owner) = owner {
            out.try_push(Child { owner, id, name })?;
        }
    }
    out.sort_unstable_by(|a, b| {
        a.owner
            .cmp(&b.owner)
            .then(a.id.cmp(&b.id))
            .then(a.name.cmp(b.name))
    });
    Ok(out)
}
fn version(id: Guid) -> std::result::Result<[u8; 40], BudgetError> {
    let mut out = [b'0'; 40];
    let digits = b"0123456789abcdef";
    for (at, byte) in id.to_bytes_le().iter().enumerate() {
        *out.get_mut(at * 2).ok_or(BudgetError::Overflow)? = *digits
            .get(usize::from(byte >> 4))
            .ok_or(BudgetError::Overflow)?;
        *out.get_mut(at * 2 + 1).ok_or(BudgetError::Overflow)? = *digits
            .get(usize::from(byte & 15))
            .ok_or(BudgetError::Overflow)?;
    }
    Ok(out)
}
fn guid_text(budget: &MemoryBudget, id: Guid) -> std::result::Result<BudgetedText, BudgetError> {
    let mut out = text(budget, &[])?;
    if write!(out, "{id}").is_err() {
        return Err(out.error().copied().unwrap_or(BudgetError::Overflow));
    }
    Ok(out)
}
fn write(
    budget: &MemoryBudget,
    names: &impl ManagedDumpNames,
    part: &ConfigDumpInfo,
    limit: u64,
) -> Result<Charged<String>> {
    let children = children(budget, names)?;
    let mut entries = BudgetedVec::new(budget, Category::Metadata)?;
    for (ordinal, (raw, version)) in part.versions.iter().enumerate() {
        if raw.is_empty() || matches!(raw.as_str(), "root" | "version" | "versions") {
            continue;
        }
        entries.try_push((
            element(budget, names, raw)?,
            raw.as_str(),
            *version,
            ordinal,
        ))?;
    }
    entries.sort_unstable_by(|a, b| a.0.as_str().cmp(b.0.as_str()).then(a.3.cmp(&b.3)));
    let mut out = ManagedXmlWriter::part_document(
        budget,
        limit,
        "ConfigDumpInfo",
        &[
            ("xmlns", "http://v8.1c.ru/8.3/xcf/dumpinfo"),
            ("xmlns:xen", "http://v8.1c.ru/8.3/xcf/enums"),
            ("xmlns:xs", "http://www.w3.org/2001/XMLSchema"),
            ("xmlns:xsi", "http://www.w3.org/2001/XMLSchema-instance"),
            ("format", "Hierarchical"),
            ("version", crate::DUMP_VERSION),
        ],
    )?;
    out.open("ConfigVersions", &[])?;
    if entries.is_empty() {
        out.empty_body_line()?;
    }
    for (name, raw, id, _) in entries.iter() {
        let version = version(*id)?;
        let version = std::str::from_utf8(&version).map_err(|_| Failure::Bad)?;
        let attrs = [
            ("name", name.as_str()),
            ("id", *raw),
            ("configVersion", version),
        ];
        let child_id = (!raw.contains('.')).then(|| canonical(raw)).flatten();
        let start = child_id
            .map(|id| children.partition_point(|row| row.owner < id))
            .unwrap_or(0);
        let selected = children
            .get(start..)
            .ok_or(BudgetError::Overflow)?
            .iter()
            .take_while(|row| Some(row.owner) == child_id);
        if selected.clone().next().is_none() {
            out.leaf("Metadata", None, &attrs)?;
            continue;
        }
        out.open("Metadata", &attrs)?;
        for row in selected {
            let id = guid_text(budget, row.id)?;
            out.leaf("Metadata", None, &[("name", row.name), ("id", id.as_str())])?;
        }
        out.close("Metadata")?;
    }
    out.close("ConfigVersions")?;
    Ok(out.finish()?)
}
pub fn managed_configdump_xml(
    budget: &MemoryBudget,
    names: &impl ManagedDumpNames,
    part: &ConfigDumpInfo,
    limit: u64,
) -> std::result::Result<Option<Charged<String>>, BudgetError> {
    match write(budget, names, part, limit) {
        Ok(body) => Ok(Some(body)),
        Err(Failure::Bad) => Ok(None),
        Err(Failure::Memory(error)) => Err(error),
    }
}
