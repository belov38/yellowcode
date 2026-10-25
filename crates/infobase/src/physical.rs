use crate::error::{Error, Result};
use ibschema::columns::{ColType, Column, Origin, of_table, phys_table_name};
use ibschema::logical::{LogicalSchema, Table, TypeCode};
use onecd::{FieldDescr, FieldType, IndexDescr, IndexField, TableDescr};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameCase {
    Applied,
    Upper,
}

fn bad(at: String, why: String) -> Error {
    Error::Schema(ibschema::Error::Parse {
        what: "DBSCHEMA",
        at,
        why,
    })
}

pub fn field_descr(c: &Column) -> FieldDescr {
    let (ty, length, precision) = match c.ty {
        ColType::B(n) => (FieldType::B, n, 0),
        ColType::VB(n) => (FieldType::VB, n, 0),
        ColType::I => (FieldType::I, 0, 0),
        ColType::L => (FieldType::L, 0, 0),
        ColType::N(d, f) => (FieldType::N, d, f),
        ColType::NC(n) => (FieldType::NC, n, 0),
        ColType::NVC(n) => (FieldType::NVC, n, 0),
        ColType::NT => (FieldType::NT, 0, 0),
        ColType::DT => (FieldType::DT, 0, 0),
        ColType::RV => (FieldType::RV, 0, 0),
    };
    FieldDescr {
        name: c.name.to_ascii_uppercase(),
        ty,
        nullable: c.nullable,
        length,
        precision,
        case_sensitive: !matches!(c.ty, ColType::NC(_) | ColType::NVC(_) | ColType::NT),
    }
}

fn index_field(c: &Column) -> IndexField {
    let prefix_len = match c.ty {
        ColType::NC(n) | ColType::NVC(n) | ColType::B(n) | ColType::VB(n) => n,
        _ => 0,
    };
    IndexField {
        name: c.name.to_ascii_uppercase(),
        prefix_len,
    }
}

fn is_own_id(t: &Table) -> bool {
    t.fields.iter().any(|f| {
        f.name == "ID"
            && matches!(f.types.as_slice(), [ty] if ty.code == TypeCode::R && ty.ref_kind == 2)
    })
}

pub fn table_descr(t: &Table, owner: Option<&Table>, case: NameCase) -> Result<TableDescr> {
    let phys = phys_table_name(t, owner);
    let upper = phys.to_ascii_uppercase();
    let cols = of_table(t, owner)?;
    let [first, second] = &owner.unwrap_or(t).splitters;
    let list = if owner.is_some() { second } else { first };
    let splitters: Vec<IndexField> = {
        let names: Vec<&String> = list.iter().flatten().collect();
        let mut v = Vec::new();
        for n in names {
            let before = v.len();
            for c in &cols {
                let hit = match (&c.origin, owner) {
                    (Origin::OwnerSplitter(s), Some(_)) => s == n,
                    (Origin::Field(i), None) => t.fields.get(*i).is_some_and(|f| f.name == *n),
                    _ => false,
                };
                if hit {
                    v.push(index_field(c));
                }
            }
            if v.len() == before {
                return Err(bad(
                    format!(
                        "{}.splitters[{}]",
                        owner.unwrap_or(t).name,
                        usize::from(owner.is_some())
                    ),
                    format!("splitter field {n} has no column"),
                ));
            }
        }
        v
    };
    let suffix = "@".repeat(splitters.len());
    let owner_id = cols.iter().find(|c| c.origin == Origin::OwnerId);
    let id_col = || {
        cols.iter()
            .find(|c| matches!(c.origin, Origin::Field(i) if t.fields.get(i).is_some_and(|f| f.name == "ID")))
    };
    let mut implicit_first = None;
    let mut implicit_last: Vec<IndexDescr> = Vec::new();
    if owner.is_some() {
        let oid = owner_id.ok_or_else(|| bad(phys.clone(), "no owner reference column".into()))?;
        let key = cols
            .iter()
            .find(|c| c.origin == Origin::KeyField)
            .ok_or_else(|| bad(phys.clone(), "no key column".into()))?;
        let mut f = splitters.clone();
        f.push(index_field(oid));
        f.push(index_field(key));
        implicit_first = Some(IndexDescr {
            name: format!("{upper}_SK{suffix}"),
            unique: true,
            fields: f,
        });
    } else if is_own_id(t) {
        let id = id_col().ok_or_else(|| bad(phys.clone(), "no ID column".into()))?;
        if suffix.is_empty() {
            implicit_first = Some(IndexDescr {
                name: "_IDRREFIDX".into(),
                unique: true,
                fields: vec![index_field(id)],
            });
        } else {
            let mut f = splitters.clone();
            f.push(index_field(id));
            implicit_last.push(IndexDescr {
                name: format!("{upper}_S_HPK{suffix}"),
                unique: true,
                fields: f,
            });
            let [_, second] = &t.splitters;
            let l0: Vec<&String> = t.splitters[0].iter().flatten().collect();
            let l1: Vec<&String> = second.iter().flatten().collect();
            if l0 != l1 {
                let mut f = Vec::new();
                for n in l1 {
                    let c = cols
                        .iter()
                        .find(|c| matches!(c.origin, Origin::Field(i) if t.fields.get(i).is_some_and(|f| f.name == *n)))
                        .ok_or_else(|| {
                            bad(
                                format!("{}.splitters[1]", t.name),
                                format!("splitter field {n} has no column"),
                            )
                        })?;
                    f.push(index_field(c));
                }
                let at = "@".repeat(f.len());
                f.push(index_field(id));
                implicit_last.push(IndexDescr {
                    name: format!("{upper}_S_PK{at}"),
                    unique: true,
                    fields: f,
                });
            }
        }
    }
    let mut indexes: Vec<IndexDescr> = implicit_first.into_iter().collect();
    for (k, ix) in t.indexes.iter().enumerate() {
        let mut fields = splitters.clone();
        for name in &ix.fields {
            let before = fields.len();
            if owner.is_some() && name == "ID" {
                if let Some(c) = owner_id {
                    fields.push(index_field(c));
                }
            } else {
                for c in &cols {
                    if matches!(c.origin, Origin::Field(i) if t.fields.get(i).is_some_and(|f| f.name == *name))
                    {
                        fields.push(index_field(c));
                    }
                }
            }
            if fields.len() == before {
                return Err(bad(
                    format!("{}.{}", t.name, ix.name),
                    format!("index field {name} is not a column of the table"),
                ));
            }
        }
        indexes.push(IndexDescr {
            name: format!("{upper}_{}{suffix}", k + 1),
            unique: ix.unique,
            fields,
        });
    }
    indexes.extend(implicit_last);
    Ok(TableDescr {
        name: match case {
            NameCase::Applied => phys,
            NameCase::Upper => upper,
        },
        fields: cols.iter().map(field_descr).collect(),
        indexes,
        recordlock: false,
        files: [0; 3],
    })
}

pub fn descrs(schema: &LogicalSchema, case: NameCase) -> Result<Vec<TableDescr>> {
    schema
        .walk()
        .into_iter()
        .map(|(t, o)| table_descr(t, o, case))
        .collect()
}
