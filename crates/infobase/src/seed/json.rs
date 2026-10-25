use ibschema::logical::{Field, Index, KeyKind, LType, Table, TableKind, TypeCode};
use onecd::{FieldDescr, FieldType, IndexDescr, IndexField, TableDescr};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

#[derive(Serialize, Deserialize, PartialEq, Debug)]
pub struct JTable {
    pub name: String,
    pub recordlock: bool,
    pub fields: Vec<JField>,
    pub indexes: Vec<JIndex>,
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
pub struct JField {
    pub name: String,
    pub ty: String,
    pub length: u32,
    pub precision: u32,
    pub nullable: bool,
    pub cs: bool,
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
pub struct JIndex {
    pub name: String,
    pub unique: bool,
    pub fields: Vec<(String, u32)>,
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
pub struct JLTable {
    pub name: String,
    pub kind: String,
    pub number: u32,
    pub parent: String,
    pub fields: Vec<JLField>,
    pub nested: Vec<JLTable>,
    pub indexes: Vec<JLIndex>,
    pub one: i64,
    pub key: String,
    pub splitters: [Vec<Vec<String>>; 2],
    pub tail: (String, i64, i64),
}

pub type JLType = (String, u32, u32, String, u32, Option<i64>);

#[derive(Serialize, Deserialize, PartialEq, Debug)]
pub struct JLField {
    pub name: String,
    pub nullable: bool,
    pub types: Vec<JLType>,
    pub tail: (String, i64),
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
pub struct JLIndex {
    pub name: String,
    pub unique: bool,
    pub fields: Vec<String>,
    pub clustered: bool,
    pub flag2: bool,
    pub rest: (i64, Vec<i64>, i64, i64),
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
pub struct JParams {
    pub name: String,
    pub text: String,
    pub deflate: bool,
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
pub struct JRows {
    pub ibversion: (u32, u32),
    pub schemastorage: (u32, u32),
    pub newgen: String,
}

fn kind_letter(k: TableKind) -> &'static str {
    match k {
        TableKind::Main => "N",
        TableKind::Nested => "I",
    }
}

fn kind_of(s: &str) -> Option<TableKind> {
    match s {
        "N" => Some(TableKind::Main),
        "I" => Some(TableKind::Nested),
        _ => None,
    }
}

fn key_letter(k: KeyKind) -> &'static str {
    match k {
        KeyKind::Ref => "R",
        KeyKind::Plain => "S",
    }
}

fn key_of(s: &str) -> Option<KeyKind> {
    match s {
        "R" => Some(KeyKind::Ref),
        "S" => Some(KeyKind::Plain),
        _ => None,
    }
}

const TYPE_CODES: [(TypeCode, &str); 8] = [
    (TypeCode::B, "B"),
    (TypeCode::L, "L"),
    (TypeCode::N, "N"),
    (TypeCode::S, "S"),
    (TypeCode::T, "T"),
    (TypeCode::R, "R"),
    (TypeCode::E, "E"),
    (TypeCode::V, "V"),
];

fn type_letter(c: TypeCode) -> &'static str {
    TYPE_CODES
        .iter()
        .find_map(|&(t, l)| (t == c).then_some(l))
        .unwrap_or("?")
}

fn type_of(s: &str) -> Option<TypeCode> {
    TYPE_CODES.iter().find_map(|&(t, l)| (l == s).then_some(t))
}

fn bad(what: &str, name: &str, why: &str) -> Error {
    Error::Inconsistent(format!("seed {what} {name}: {why}"))
}

impl JTable {
    pub fn from_descr(d: &TableDescr) -> JTable {
        JTable {
            name: d.name.clone(),
            recordlock: d.recordlock,
            fields: d
                .fields
                .iter()
                .map(|f| JField {
                    name: f.name.clone(),
                    ty: f.ty.code().to_string(),
                    length: f.length,
                    precision: f.precision,
                    nullable: f.nullable,
                    cs: f.case_sensitive,
                })
                .collect(),
            indexes: d
                .indexes
                .iter()
                .map(|i| JIndex {
                    name: i.name.clone(),
                    unique: i.unique,
                    fields: i
                        .fields
                        .iter()
                        .map(|f| (f.name.clone(), f.prefix_len))
                        .collect(),
                })
                .collect(),
        }
    }

    pub fn to_descr(&self) -> Result<TableDescr> {
        let fields = self
            .fields
            .iter()
            .map(|f| {
                let ty = FieldType::from_code(&f.ty).ok_or_else(|| {
                    bad("field", &self.name, &format!("{}: type {:?}", f.name, f.ty))
                })?;
                Ok(FieldDescr {
                    name: f.name.clone(),
                    ty,
                    nullable: f.nullable,
                    length: f.length,
                    precision: f.precision,
                    case_sensitive: f.cs,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let indexes = self
            .indexes
            .iter()
            .map(|i| IndexDescr {
                name: i.name.clone(),
                unique: i.unique,
                fields: i
                    .fields
                    .iter()
                    .map(|(n, p)| IndexField {
                        name: n.clone(),
                        prefix_len: *p,
                    })
                    .collect(),
            })
            .collect();
        Ok(TableDescr {
            name: self.name.clone(),
            fields,
            indexes,
            recordlock: self.recordlock,
            files: [0; 3],
        })
    }
}

impl JLTable {
    pub fn from_table(t: &Table) -> JLTable {
        JLTable {
            name: t.name.clone(),
            kind: kind_letter(t.kind).to_string(),
            number: t.number,
            parent: t.parent.clone(),
            fields: t
                .fields
                .iter()
                .map(|f| JLField {
                    name: f.name.clone(),
                    nullable: f.nullable,
                    types: f
                        .types
                        .iter()
                        .map(|y| {
                            (
                                type_letter(y.code).to_string(),
                                y.length,
                                y.precision,
                                y.ref_table.clone(),
                                y.ref_kind,
                                y.extra,
                            )
                        })
                        .collect(),
                    tail: (f.tail_s.clone(), f.tail_n),
                })
                .collect(),
            nested: t.nested.iter().map(JLTable::from_table).collect(),
            indexes: t
                .indexes
                .iter()
                .map(|i| JLIndex {
                    name: i.name.clone(),
                    unique: i.unique,
                    fields: i.fields.clone(),
                    clustered: i.clustered,
                    flag2: i.flag2,
                    rest: (i.n3, i.list.clone(), i.n4, i.n5),
                })
                .collect(),
            one: t.one,
            key: key_letter(t.key).to_string(),
            splitters: t.splitters.clone(),
            tail: (t.tail_s.clone(), t.tail_n1, t.tail_n2),
        }
    }

    pub fn to_table(&self) -> Result<Table> {
        let kind = kind_of(&self.kind)
            .ok_or_else(|| bad("table", &self.name, &format!("kind {:?}", self.kind)))?;
        let key = key_of(&self.key)
            .ok_or_else(|| bad("table", &self.name, &format!("key {:?}", self.key)))?;
        let fields = self
            .fields
            .iter()
            .map(|f| {
                let types = f
                    .types
                    .iter()
                    .map(|(c, length, precision, ref_table, ref_kind, extra)| {
                        let code = type_of(c).ok_or_else(|| {
                            bad("field", &self.name, &format!("{}: type {c:?}", f.name))
                        })?;
                        Ok(LType {
                            code,
                            length: *length,
                            precision: *precision,
                            ref_table: ref_table.clone(),
                            ref_kind: *ref_kind,
                            extra: *extra,
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                Ok(Field {
                    name: f.name.clone(),
                    nullable: f.nullable,
                    types,
                    tail_s: f.tail.0.clone(),
                    tail_n: f.tail.1,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Table {
            name: self.name.clone(),
            kind,
            number: self.number,
            parent: self.parent.clone(),
            fields,
            nested: self
                .nested
                .iter()
                .map(JLTable::to_table)
                .collect::<Result<Vec<_>>>()?,
            indexes: self
                .indexes
                .iter()
                .map(|i| Index {
                    name: i.name.clone(),
                    unique: i.unique,
                    fields: i.fields.clone(),
                    clustered: i.clustered,
                    flag2: i.flag2,
                    n3: i.rest.0,
                    list: i.rest.1.clone(),
                    n4: i.rest.2,
                    n5: i.rest.3,
                })
                .collect(),
            one: self.one,
            key,
            splitters: self.splitters.clone(),
            tail_s: self.tail.0.clone(),
            tail_n1: self.tail.1,
            tail_n2: self.tail.2,
        })
    }
}
