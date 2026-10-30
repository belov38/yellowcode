use std::{
    collections::{HashMap, HashSet},
    io::Read,
};

use ibschema::logical::LogicalSchema;

const SYSTEM_TABLES: &[&str] = &[
    "_YearOffset",
    "BinaryData",
    "BinaryDataStorageContent",
    "BinaryDataStorageVersion",
    "Config",
    "ConfigCAS",
    "ConfigCASSave",
    "ConfigSave",
    "DBSchema",
    "DepotFiles",
    "ExternalBinDataStrgsBList",
    "ExternalBinDataStrgsList",
    "Files",
    "IBVersion",
    "Params",
    "SchemaStorage",
    "V8CMSDPWDS",
    "V8USERPWDPLCS",
    "v8users",
    "V8USERSMATKEYS",
];

#[derive(Clone, Copy)]
pub struct Limits {
    pub max_json_bytes: usize,
    pub max_tables: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_json_bytes: 4 * 1024 * 1024,
            max_tables: 100_000,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("SQL table inventory I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid SQL table inventory: {0}")]
    Invalid(&'static str),
    #[error("invalid SQL table inventory JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid DT table schema: {0}")]
    Dt(#[from] dtfile::Error),
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    schema: String,
    name: String,
}

pub fn read_inventory(input: impl Read, limits: Limits) -> Result<Vec<String>, Error> {
    let cap = u64::try_from(
        limits
            .max_json_bytes
            .checked_add(1)
            .ok_or(Error::Invalid("size overflow"))?,
    )
    .map_err(|_| Error::Invalid("size overflow"))?;
    let mut bytes = Vec::new();
    input.take(cap).read_to_end(&mut bytes)?;
    if bytes.len() > limits.max_json_bytes {
        return Err(Error::Invalid("size limit exceeded"));
    }
    bytes.retain(|&b| b != b'\r' && b != b'\n');
    let rows: Vec<Wire> = serde_json::from_slice(&bytes)?;
    if rows.is_empty() || rows.len() > limits.max_tables {
        return Err(Error::Invalid("table count"));
    }
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        if !row.schema.eq_ignore_ascii_case("dbo") {
            return Err(Error::Invalid("non-dbo table"));
        }
        out.push(row.name);
    }
    Ok(out)
}

fn name(name: &str) -> Result<String, Error> {
    if name.is_empty() || name.len() > 128 || !name.is_ascii() || name.chars().any(char::is_control)
    {
        return Err(Error::Invalid("unsupported table name"));
    }
    Ok(name.to_ascii_lowercase())
}

pub struct LegacyInventory {
    pub missing_system_tables: Vec<String>,
    pub extra_tables: Vec<String>,
}

pub fn read_extra_table_presence(input: impl Read) -> Result<bool, Error> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Presence {
        nonempty: u8,
    }
    let mut bytes = Vec::new();
    input.take(8193).read_to_end(&mut bytes)?;
    if bytes.len() > 8192 {
        return Err(Error::Invalid("presence output limit"));
    }
    bytes.retain(|&b| b != b'\r' && b != b'\n');
    match serde_json::from_slice::<Presence>(&bytes)?.nonempty {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(Error::Invalid("invalid presence flag")),
    }
}

pub fn inspect_legacy_inventory(
    schema: &LogicalSchema,
    tables: &[String],
) -> Result<LegacyInventory, Error> {
    let mut actual = HashMap::new();
    for table in tables {
        if actual.insert(name(table)?, table).is_some() {
            return Err(Error::Invalid("ambiguous SQL table"));
        }
    }
    let mut known = HashSet::new();
    let mut missing_system_tables = Vec::new();
    for table in SYSTEM_TABLES {
        let key = name(table)?;
        if !known.insert(key.clone()) {
            return Err(Error::Invalid("duplicate system table"));
        }
        if !actual.contains_key(&key) {
            missing_system_tables.push((*table).to_owned());
        }
    }
    for head in dtfile::TableHead::database(schema)? {
        let key = name(&head.phys_name)?;
        if !known.insert(key.clone()) {
            return Err(Error::Invalid("duplicate schema table"));
        }
        if !actual.contains_key(&key) {
            return Err(Error::Invalid("missing schema table"));
        }
    }
    let mut extra_tables: Vec<String> = actual
        .into_iter()
        .filter(|(key, _)| !known.contains(key))
        .map(|(_, table)| table.clone())
        .collect();
    missing_system_tables.sort_unstable_by_key(|s| s.to_ascii_lowercase());
    extra_tables.sort_unstable_by_key(|s| s.to_ascii_lowercase());
    Ok(LegacyInventory {
        missing_system_tables,
        extra_tables,
    })
}

pub fn validate_inventory(schema: &LogicalSchema, tables: &[String]) -> Result<(), Error> {
    let mut expected = HashSet::new();
    for table in SYSTEM_TABLES {
        if !expected.insert(name(table)?) {
            return Err(Error::Invalid("duplicate system table"));
        }
    }
    for head in dtfile::TableHead::database(schema)? {
        if !expected.insert(name(&head.phys_name)?) {
            return Err(Error::Invalid("duplicate schema table"));
        }
    }
    let mut actual = HashSet::new();
    for table in tables {
        if !actual.insert(name(table)?) {
            return Err(Error::Invalid("ambiguous SQL table"));
        }
    }
    if actual != expected {
        return Err(Error::Invalid("missing or unexpected SQL table"));
    }
    Ok(())
}
