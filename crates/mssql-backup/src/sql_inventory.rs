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
    #[error("invalid SQL table inventory: {0}")]
    Mismatch(Mismatch),
    #[error("unsupported: unexpected 1C extension tables even with --ignore-extra-tables ({})", format_names(.0))]
    UnexpectedExtensionTwins(Vec<String>),
}

pub const SHOWN_NAMES: usize = 10;

fn format_names(names: &[String]) -> String {
    let mut out = format!("{} unexpected: ", sql_tables(names.len()));
    for (i, name) in names.iter().take(SHOWN_NAMES).enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(name);
    }
    if names.len() > SHOWN_NAMES {
        out.push_str(&format!(" (and {} more)", names.len() - SHOWN_NAMES));
    }
    out
}

pub fn is_extension_twin_name(name: &str) -> bool {
    name.starts_with('_') && bak_trace::extension_suffix(name).is_some()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mismatch {
    pub missing: Vec<Missing>,
    pub extra: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Missing {
    pub name: String,
    pub system: bool,
}

impl Mismatch {
    pub fn extension_twins<'a>(
        &'a self,
        extension_ids: &'a [u32],
    ) -> impl Iterator<Item = &'a str> + 'a {
        self.extra.iter().map(String::as_str).filter(move |name| {
            name.starts_with('_')
                && bak_trace::extension_suffix(name)
                    .is_some_and(|(_, id)| extension_ids.contains(&id))
        })
    }

    pub fn unaccounted_extension_twins(&self) -> impl Iterator<Item = &str> {
        self.extra
            .iter()
            .map(String::as_str)
            .filter(|name| is_extension_twin_name(name))
    }
}

pub fn sql_tables(count: usize) -> String {
    if count == 1 {
        "1 SQL table".to_owned()
    } else {
        format!("{count} SQL tables")
    }
}

fn list(out: &mut std::fmt::Formatter<'_>, label: &str, names: &[&str]) -> std::fmt::Result {
    write!(out, "; {label}: ")?;
    for (i, name) in names.iter().take(SHOWN_NAMES).enumerate() {
        if i > 0 {
            out.write_str(", ")?;
        }
        out.write_str(name)?;
    }
    if names.len() > SHOWN_NAMES {
        write!(out, " (and {} more)", names.len() - SHOWN_NAMES)?;
    }
    Ok(())
}

impl std::fmt::Display for Mismatch {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            out,
            "{} missing, {} unexpected",
            sql_tables(self.missing.len()),
            sql_tables(self.extra.len())
        )?;
        let missing: Vec<&str> = self.missing.iter().map(|m| m.name.as_str()).collect();
        if !missing.is_empty() {
            list(out, "missing", &missing)?;
        }
        let extra: Vec<&str> = self.extra.iter().map(String::as_str).collect();
        if !extra.is_empty() {
            list(out, "unexpected", &extra)?;
        }
        Ok(())
    }
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

pub const OPTIONAL_SYSTEM_TABLE: &str = "DepotFiles";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Inventory {
    pub depot_files: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedInventory {
    pub depot_files: bool,
    pub ignored_extra_tables: Vec<String>,
}

pub fn known_tables(schema: &LogicalSchema) -> Result<Vec<String>, Error> {
    known_tables_with_extensions(schema, None)
}

pub fn known_tables_with_extensions(
    schema: &LogicalSchema,
    extension: Option<(u32, &LogicalSchema)>,
) -> Result<Vec<String>, Error> {
    let mut names: Vec<String> = SYSTEM_TABLES.iter().map(|t| (*t).to_owned()).collect();
    for head in dtfile::TableHead::database(schema)? {
        names.push(head.phys_name);
    }
    if let Some((schema_id, ext_schema)) = extension {
        for head in dtfile::TableHead::extension(schema_id, ext_schema)? {
            names.push(head.phys_name);
        }
    }
    Ok(names)
}

pub fn validate_inventory(schema: &LogicalSchema, tables: &[String]) -> Result<Inventory, Error> {
    let validated = validate_inventory_with_extensions(schema, None, tables, false)?;
    Ok(Inventory {
        depot_files: validated.depot_files,
    })
}

pub fn validate_inventory_with_extensions(
    schema: &LogicalSchema,
    extension: Option<(u32, &LogicalSchema)>,
    tables: &[String],
    ignore_extra_tables: bool,
) -> Result<ValidatedInventory, Error> {
    let optional = name(OPTIONAL_SYSTEM_TABLE)?;
    let mut expected: HashMap<String, (String, bool)> = HashMap::new();
    for table in SYSTEM_TABLES {
        let key = name(table)?;
        if key == optional {
            continue;
        }
        if expected.insert(key, ((*table).to_owned(), true)).is_some() {
            return Err(Error::Invalid("duplicate system table"));
        }
    }
    for head in dtfile::TableHead::database(schema)? {
        if expected
            .insert(name(&head.phys_name)?, (head.phys_name, false))
            .is_some()
        {
            return Err(Error::Invalid("duplicate schema table"));
        }
    }
    if let Some((schema_id, ext_schema)) = extension {
        for head in dtfile::TableHead::extension(schema_id, ext_schema)? {
            if expected
                .insert(name(&head.phys_name)?, (head.phys_name, false))
                .is_some()
            {
                return Err(Error::Invalid("duplicate extension table"));
            }
        }
    }
    if expected.contains_key(&optional) {
        return Err(Error::Invalid("duplicate schema table"));
    }
    let mut actual = HashMap::new();
    for table in tables {
        if actual.insert(name(table)?, table).is_some() {
            return Err(Error::Invalid("ambiguous SQL table"));
        }
    }
    let depot_files = actual.remove(&optional).is_some();
    let mut missing: Vec<Missing> = expected
        .iter()
        .filter(|(key, _)| !actual.contains_key(*key))
        .map(|(_, (name, system))| Missing {
            name: name.clone(),
            system: *system,
        })
        .collect();
    let mut extra: Vec<String> = actual
        .iter()
        .filter(|(key, _)| !expected.contains_key(*key))
        .map(|(_, table)| (*table).clone())
        .collect();
    missing.sort_unstable_by_key(|m| (!m.system, m.name.to_ascii_lowercase()));
    extra.sort_unstable_by_key(|s| s.to_ascii_lowercase());
    if missing.is_empty() && extra.is_empty() {
        return Ok(ValidatedInventory {
            depot_files,
            ignored_extra_tables: Vec::new(),
        });
    }
    if missing.is_empty() && ignore_extra_tables {
        let twins: Vec<String> = extra
            .iter()
            .filter(|table| is_extension_twin_name(table))
            .cloned()
            .collect();
        if !twins.is_empty() {
            return Err(Error::UnexpectedExtensionTwins(twins));
        }
        return Ok(ValidatedInventory {
            depot_files,
            ignored_extra_tables: extra,
        });
    }
    Err(Error::Mismatch(Mismatch { missing, extra }))
}

const MAX_SCHEMA_ROWS: usize = 10_000;

pub fn read_schema_ids(input: impl Read) -> Result<Vec<u32>, Error> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Row {
        schema_id: i64,
    }
    let mut bytes = Vec::new();
    input.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        return Err(Error::Invalid("schema id output limit"));
    }
    bytes.retain(|&b| b != b'\r' && b != b'\n');
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(Vec::new());
    }
    let rows: Vec<Row> = serde_json::from_slice(&bytes)?;
    if rows.len() > MAX_SCHEMA_ROWS {
        return Err(Error::Invalid("schema id count"));
    }
    let mut ids = rows
        .into_iter()
        .map(|r| u32::try_from(r.schema_id).map_err(|_| Error::Invalid("schema id out of range")))
        .collect::<Result<Vec<u32>, Error>>()?;
    ids.sort_unstable();
    Ok(ids)
}
