use crate::config::{TableDiff, allocated_lines, object_table_diffs};
use crate::error::{Error, Result};
use ibschema::logical::LogicalSchema;
use ibschema::rules::Built;
use ibschema::storage::{StorageMap, TableId};
use ibschema::values::Uuid16;
use onecd::Database;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedTable {
    pub object: Uuid16,
    pub rows: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SchemaPin {
    pub tables: Vec<PinnedTable>,
    pub lines: Vec<String>,
}

impl SchemaPin {
    pub fn parse(
        tables: &[(&str, u64)],
        lines: &[String],
    ) -> std::result::Result<SchemaPin, String> {
        let tables = tables
            .iter()
            .map(|(text, rows)| {
                Uuid16::parse(text)
                    .filter(|object| object.to_string() == *text)
                    .map(|object| PinnedTable {
                        object,
                        rows: *rows,
                    })
                    .ok_or_else(|| {
                        format!(
                            "{text:?} is not a uuid in the canonical form (36 characters, lower-case hex digits and four hyphens)"
                        )
                    })
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(SchemaPin {
            tables,
            lines: lines.to_vec(),
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct WarnWrite {
    pub pin: SchemaPin,
    pub sealed_sha256: Vec<String>,
}

pub(crate) fn check_file(file: &Path, sealed: &[String]) -> Result<()> {
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if std::fs::metadata(file).is_ok_and(|m| m.permissions().readonly()) {
        return Err(Error::Sealed(format!(
            "{name} is read only: a sealed snapshot is never opened for writing, open a copy"
        )));
    }
    if sealed.is_empty() {
        return Ok(());
    }
    let hash = sha256_of(file).map_err(|e| {
        Error::Sealed(format!(
            "{name} cannot be hashed to check it against the sealed snapshots: {e}"
        ))
    })?;
    if sealed.iter().any(|s| s.eq_ignore_ascii_case(&hash)) {
        return Err(Error::Sealed(format!(
            "{name} has the hash of a sealed snapshot ({hash}): it is never opened for writing, not even as a copy"
        )));
    }
    Ok(())
}

fn sha256_of(file: &Path) -> std::io::Result<String> {
    let mut input = std::fs::File::open(file)?;
    let mut hash = Sha256::new();
    let mut chunk = vec![0u8; 1 << 20];
    loop {
        let n = input.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        hash.update(chunk.get(..n).unwrap_or_default());
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn rows_up_to(db: &Database, map: &StorageMap, logical: &str, limit: u64) -> Result<u64> {
    let Some(id) = map.table_by_name(logical) else {
        return Ok(0);
    };
    let txn = db.read();
    let table = txn.table(&map.table(id).phys)?;
    let mut rows = 0u64;
    for row in table.scan() {
        row?;
        rows += 1;
        if rows > limit {
            break;
        }
    }
    Ok(rows)
}

type Found = BTreeMap<Uuid16, Vec<(String, String)>>;

fn compare(
    pin: &SchemaPin,
    found: &Found,
    lines: &[String],
    rows: &mut dyn FnMut(&str, u64) -> Result<u64>,
) -> Result<Vec<String>> {
    let mut problems = Vec::new();
    let mut pinned: BTreeMap<Uuid16, Vec<u64>> = BTreeMap::new();
    for t in &pin.tables {
        pinned.entry(t.object).or_default().push(t.rows);
    }
    for (object, tables) in found {
        let Some(want) = pinned.get(object) else {
            for (table, name) in tables {
                problems.push(format!(
                    "table {table} of object {object} ({name}) differs from the rules and is not in the pin"
                ));
            }
            continue;
        };
        let limit = want.iter().copied().max().unwrap_or(0);
        let mut have = Vec::with_capacity(tables.len());
        for (table, _) in tables {
            have.push(rows(table, limit)?);
        }
        let (mut a, mut b) = (have.clone(), want.clone());
        a.sort_unstable();
        b.sort_unstable();
        if a != b {
            let shown = |n: u64| {
                if n > limit {
                    format!("more than {limit}")
                } else {
                    n.to_string()
                }
            };
            for ((table, name), n) in tables.iter().zip(&have) {
                problems.push(format!(
                    "table {table} of object {object} ({name}) has {} rows, the pin says {want:?}",
                    shown(*n)
                ));
            }
        }
    }
    for object in pinned.keys().filter(|o| !found.contains_key(o)) {
        problems.push(format!(
            "the pin has object {object} but no table of it differs from the rules"
        ));
    }
    let mut unmatched = pin.lines.clone();
    for line in lines {
        match unmatched.iter().position(|l| l == line) {
            Some(at) => {
                unmatched.remove(at);
            }
            None => problems.push(format!("line not in the pin: {line}")),
        }
    }
    for line in unmatched {
        problems.push(format!("pinned line not found: {line}"));
    }
    Ok(problems)
}

fn unpinnable(skipped: &[String], left_out: &[String]) -> Vec<String> {
    let mut problems = Vec::new();
    if !skipped.is_empty() {
        problems.push(format!(
            "objects of kinds without rules cannot be pinned: {}",
            skipped.join(", ")
        ));
    }
    if !left_out.is_empty() {
        problems.push(format!(
            "tables or fields left out of the storage map cannot be pinned: {}",
            left_out.join("; ")
        ));
    }
    problems
}

pub(crate) fn check_pin(
    pin: &SchemaPin,
    built: &Built,
    actual: &LogicalSchema,
    left_out: &[String],
    map: &StorageMap,
    db: &Database,
) -> Result<(Vec<TableId>, Vec<TableId>)> {
    check_pin_with(pin, built, actual, left_out, map, &mut |table, limit| {
        rows_up_to(db, map, table, limit)
    })
}

fn check_pin_with(
    pin: &SchemaPin,
    built: &Built,
    actual: &LogicalSchema,
    left_out: &[String],
    map: &StorageMap,
    rows: &mut dyn FnMut(&str, u64) -> Result<u64>,
) -> Result<(Vec<TableId>, Vec<TableId>)> {
    let mut problems = unpinnable(&built.skipped, left_out);
    let mut found = Found::new();
    for diff in object_table_diffs(built, actual) {
        let table = diff.table().to_string();
        let owner = built
            .objects
            .iter()
            .find(|o| o.tables.iter().any(|t| t.logical == table));
        match (&diff, owner) {
            (TableDiff::Differs(..), Some(o)) => found
                .entry(o.md)
                .or_default()
                .push((table, o.full_name.clone())),
            (TableDiff::Differs(..), None) => {
                problems.push(format!("table {table} differs and belongs to no object"));
            }
            (TableDiff::Extra(_), _) => problems.push(format!(
                "table {table} is not in DBSCHEMA: only a table that differs can be pinned"
            )),
            (TableDiff::Missing(_), _) => problems.push(format!(
                "table {table} is not in the built schema: only a table that differs can be pinned"
            )),
        }
    }
    problems.extend(compare(pin, &found, &allocated_lines(built), rows)?);
    if !problems.is_empty() {
        return Err(Error::SchemaPin(problems));
    }
    let differing: Vec<TableId> = found
        .values()
        .flatten()
        .filter_map(|(table, _)| map.table_by_name(table))
        .collect();
    let mut refused = differing.clone();
    for md in found.keys() {
        if let Some(object) = map.object(*md) {
            refused.push(object.main);
            refused.extend(object.tabular.iter().map(|(_, t)| *t));
            refused.extend(object.lists.iter().map(|(_, t)| *t));
            refused.extend(object.service.iter().map(|(_, t)| *t));
        }
    }
    refused.sort_unstable();
    refused.dedup();
    Ok((differing, refused))
}
