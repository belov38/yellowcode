use serde_json::{Map, Value, json};

use super::{FieldInfo, IndexInfo, StorageMap, TableInfo};
use crate::error::{Error, Result};
use crate::names::{StdField, role};
use crate::values::Uuid16;
use md_model::ScriptVariant;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageInfoField {
    pub storage_field_name: String,
    pub field_name: String,
    pub metadata: String,
    pub field_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageInfoIndex {
    pub storage_index_name: String,
    pub fields: Vec<StorageInfoField>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageInfoTable {
    pub storage_table_name: String,
    pub table_name: String,
    pub metadata: String,
    pub purpose: String,
    pub fields: Vec<StorageInfoField>,
    pub indexes: Vec<StorageInfoIndex>,
}

impl StorageInfoField {
    fn to_json(&self) -> Value {
        json!({
            "FieldType": self.field_type,
            "Metadata": self.metadata,
            "FieldName": self.field_name,
            "StorageFieldName": self.storage_field_name,
        })
    }
}

impl StorageInfoTable {
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("IndexStorageTablespace".into(), json!(""));
        m.insert("DataStorageTablespace".into(), json!(""));
        m.insert(
            "Indexes".into(),
            Value::Array(
                self.indexes
                    .iter()
                    .map(|ix| {
                        json!({
                            "Fields": ix.fields.iter().map(StorageInfoField::to_json).collect::<Vec<_>>(),
                            "StorageIndexName": ix.storage_index_name,
                        })
                    })
                    .collect(),
            ),
        );
        m.insert(
            "Fields".into(),
            Value::Array(self.fields.iter().map(StorageInfoField::to_json).collect()),
        );
        m.insert("Purpose".into(), json!(self.purpose));
        m.insert("TableName".into(), json!(self.table_name));
        m.insert("Metadata".into(), json!(self.metadata));
        m.insert("StorageTableName".into(), json!(self.storage_table_name));
        Value::Object(m)
    }
}

const UNNAMED: &[&str] = &[
    role::INFORMATION_REGISTER_TOTALS_OPTIONS,
    role::TOTALS_SLICE_LAST,
    role::TOTALS_SLICE_FIRST,
    role::TOTALS,
    role::TURNOVERS,
    role::ACCUMULATION_REGISTER_TOTALS_OPTIONS,
    role::ACCUMULATION_REGISTERS_AGGREGATE_LIST,
    role::ACCUMULATION_REGISTERS_AGGREGATES_OPTIONS,
    role::ACCUMULATION_REGISTER_DIMENSION_DICTIONARY,
    role::NEW_TURNOVERS,
    role::TURNOVERS_BUFFER,
    role::QUERY_STATISTICS,
    role::TOTALS_BY_ACCOUNTS,
    role::TOTALS_BETWEEN_ACCOUNTS,
    role::ACCOUNT_REGISTER_TOTALS_OPTIONS,
    role::DISPLACEMENT_ORDER,
    role::CONSTANT_CHANGE_RECORDS,
    role::INITIALIZED_PREDEFINED_DATA_IN_CATALOG,
    role::INITIALIZED_PREDEFINED_DATA_IN_CHART_OF_CHARACTERISTIC_TYPES,
    role::INITIALIZED_PREDEFINED_DATA_IN_CHART_OF_ACCOUNTS,
    role::INITIALIZED_PREDEFINED_DATA_IN_CHART_OF_CALCULATION_TYPES,
];

fn table_prefix(t: &TableInfo) -> &str {
    t.logical
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(|c: char| c.is_ascii_digit())
}

fn std_name(t: &TableInfo, s: StdField) -> &'static str {
    match (s, table_prefix(t), t.purpose.as_str()) {
        (StdField::Period, "CRg", _) => "RegistrationPeriod",
        (StdField::Recorder, _, role::RECALCULATION) => "RecalculationObject",
        (StdField::PredefinedId, "Node", _) => "",
        (StdField::Value | StdField::Kind, "AccRg", role::MAIN) => "",
        _ => s.stor_name(),
    }
}

fn unnamed(t: &TableInfo) -> bool {
    t.md.is_none()
        || UNNAMED.contains(&t.purpose.as_str())
        || t.purpose.starts_with(role::TOTALS_BY_ACCOUNTS)
}

fn field_name(t: &TableInfo, f: &FieldInfo) -> String {
    if unnamed(t) {
        return String::new();
    }
    if let Some(s) = f.std {
        return std_name(t, s).to_string();
    }
    let common = f.md_name.starts_with("CommonAttribute.");
    if t.purpose == role::CONSTANT && !common {
        return "Value".into();
    }
    let name = f.md_name.rsplit('.').next().unwrap_or_default();
    match f.side {
        Some(side) => format!("{name}{}", side.query_suffix()),
        None => name.to_string(),
    }
}

fn metadata(t: &TableInfo, f: &FieldInfo) -> String {
    if f.side.is_some() && t.purpose == role::TOTALS_BETWEEN_ACCOUNTS {
        String::new()
    } else {
        f.md_name.clone()
    }
}

fn info_field(t: &TableInfo, f: &FieldInfo, name: &str, field_type: &str) -> StorageInfoField {
    StorageInfoField {
        storage_field_name: name.to_string(),
        field_name: field_name(t, f),
        metadata: metadata(t, f),
        field_type: field_type.to_string(),
    }
}

fn blank_in_index(f: &FieldInfo) -> bool {
    f.implicit && matches!(f.std, Some(StdField::Id | StdField::KeyField))
}

fn index_columns(t: &TableInfo, ix: &IndexInfo, name: &str) -> Result<StorageInfoIndex> {
    let mut fields = Vec::new();
    for &id in &ix.fields {
        let f = t.field(id).ok_or_else(|| {
            super::bad(
                &t.logical,
                format!("index {} has no field {}", ix.name, id.0),
            )
        })?;
        let blank = blank_in_index(f);
        for c in f.columns.clone() {
            let col = t.columns.get(c).ok_or_else(|| {
                super::bad(&t.logical, format!("field {} has no column {c}", f.name))
            })?;
            let mut x = info_field(t, f, &col.name, if blank { "" } else { "Indexed" });
            if blank {
                x.field_name.clear();
                x.metadata.clear();
            }
            fields.push(x);
        }
    }
    Ok(StorageInfoIndex {
        storage_index_name: name.to_string(),
        fields,
    })
}

fn logical_row(t: &TableInfo) -> Result<StorageInfoTable> {
    let fields = t
        .fields
        .iter()
        .filter(|f| !f.implicit)
        .map(|f| info_field(t, f, &f.name, ""))
        .collect();
    let mut indexes = Vec::with_capacity(t.indexes.len());
    for ix in &t.indexes {
        let mut fs = Vec::new();
        for &id in ix.fields.iter().skip(ix.prefix) {
            let f = t.field(id).ok_or_else(|| {
                super::bad(
                    &t.logical,
                    format!("index {} has no field {}", ix.name, id.0),
                )
            })?;
            fs.push(info_field(t, f, &f.name, "Indexed"));
        }
        indexes.push(StorageInfoIndex {
            storage_index_name: ix.name.clone(),
            fields: fs,
        });
    }
    Ok(row(t, t.logical.clone(), fields, indexes))
}

fn row(
    t: &TableInfo,
    name: String,
    fields: Vec<StorageInfoField>,
    indexes: Vec<StorageInfoIndex>,
) -> StorageInfoTable {
    StorageInfoTable {
        storage_table_name: name,
        table_name: if unnamed(t) {
            String::new()
        } else {
            t.query_name.clone()
        },
        metadata: t.md_name.clone(),
        purpose: t.purpose.clone(),
        fields,
        indexes,
    }
}

fn physical_row(t: &TableInfo) -> Result<StorageInfoTable> {
    let rank = |f: &FieldInfo| match (f.implicit, f.std) {
        (false, _) => 0,
        (true, Some(StdField::Id)) => 2,
        (true, Some(StdField::KeyField)) => 3,
        (true, _) => 1,
    };
    let mut order: Vec<&FieldInfo> = t.fields.iter().collect();
    order.sort_by_key(|f| rank(f));
    let mut fields = Vec::with_capacity(t.columns.len());
    for f in order {
        for c in f.columns.clone() {
            let col = t.columns.get(c).ok_or_else(|| {
                super::bad(&t.logical, format!("field {} has no column {c}", f.name))
            })?;
            fields.push(info_field(t, f, &col.name, ""));
        }
    }
    let mut indexes = Vec::with_capacity(t.indexes.len() + 1);
    for (k, ix) in t.indexes.iter().enumerate() {
        indexes.push(index_columns(t, ix, &format!("{}_{}", t.phys, k + 1))?);
    }
    if let Some(key) = &t.key_index {
        let name = if t.owner.is_some() {
            format!("{}_SK", t.phys)
        } else if key.prefix == 0 {
            "_IDRRefIDX".to_string()
        } else {
            format!("{}_S_HPK", t.phys)
        };
        indexes.push(index_columns(t, key, &name)?);
    }
    Ok(row(t, t.phys.clone(), fields, indexes))
}

pub(super) fn storage_info(
    m: &StorageMap,
    objects: Option<&[Uuid16]>,
    db_names: bool,
) -> Result<Vec<StorageInfoTable>> {
    if m.script_variant() == ScriptVariant::Russian {
        return Err(Error::Unsupported("script variant Russian".into()));
    }
    let mut out = Vec::new();
    for (i, t) in m.tables().iter().enumerate() {
        if let Some(list) = objects
            && !m.table_object(i).is_some_and(|o| list.contains(&o))
        {
            continue;
        }
        out.push(if db_names {
            physical_row(t)?
        } else {
            logical_row(t)?
        });
    }
    Ok(out)
}
