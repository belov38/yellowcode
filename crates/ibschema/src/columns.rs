use crate::error::{Error, Result};
use crate::logical::{Field, LType, Table, TypeCode, VAR};
use crate::values::{ColumnValue, numeric_fits, utf16_len};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColType {
    B(u32),
    VB(u32),
    I,
    L,
    N(u32, u32),
    NC(u32),
    NVC(u32),
    NT,
    DT,
    RV,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColRole {
    Ref,
    TableNo,
    TypeCode,
    KeyField,
    Binary,
    Bool,
    Number,
    Str,
    DateTime,
    RowVersion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Single,
    Type,
    L,
    N,
    T,
    S,
    B,
    RTRef,
    RRRef,
    TRef,
    RRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    OwnerId,
    OwnerSplitter(String),
    KeyField,
    Field(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    pub name: String,
    pub ty: ColType,
    pub role: ColRole,
    pub nullable: bool,
    pub origin: Origin,
    pub part: Part,
}

impl Column {
    pub fn in_dt(&self) -> bool {
        self.ty != ColType::RV
    }

    pub fn check(&self, v: &ColumnValue) -> std::result::Result<(), String> {
        if self.role == ColRole::RowVersion {
            return Err("read-only column".into());
        }
        if matches!(v, ColumnValue::Null) {
            return if self.nullable {
                Ok(())
            } else {
                Err("NULL in a NOT NULL column".into())
            };
        }
        let mismatch = || Err(format!("value kind does not match {:?}", self.ty));
        match (self.role, v) {
            (ColRole::Ref, ColumnValue::Bytes(b)) => exact_len(b.len(), 16),
            (ColRole::TableNo | ColRole::KeyField, ColumnValue::Bytes(b)) => exact_len(b.len(), 4),
            (ColRole::TypeCode, ColumnValue::Bytes(b)) => exact_len(b.len(), 1),
            (ColRole::Binary, ColumnValue::Bytes(b)) => match self.ty {
                ColType::B(n) => exact_len(b.len(), n as usize),
                ColType::VB(n) if b.len() <= n as usize => Ok(()),
                ColType::VB(n) => Err(format!("{} bytes exceed VB({n})", b.len())),
                _ => Ok(()),
            },
            (ColRole::Bool, ColumnValue::Bool(_)) => Ok(()),
            (ColRole::Number, ColumnValue::Number(x)) => match self.ty {
                ColType::N(d, f) if !numeric_fits(x, d, f) => {
                    Err(format!("number does not fit N({d},{f})"))
                }
                _ => Ok(()),
            },
            (ColRole::Str, ColumnValue::Str(s)) => match self.ty {
                ColType::NC(n) | ColType::NVC(n) if utf16_len(s) > n as usize => {
                    Err(format!("string of {} units exceeds {n}", utf16_len(s)))
                }
                _ => Ok(()),
            },
            (ColRole::DateTime, ColumnValue::DateTime(d)) if !d.is_valid() => {
                Err("invalid date and time".to_string())
            }
            (ColRole::DateTime, ColumnValue::DateTime(_)) => Ok(()),
            _ => mismatch(),
        }
    }
}

fn exact_len(got: usize, want: usize) -> std::result::Result<(), String> {
    if got == want {
        Ok(())
    } else {
        Err(format!("{got} bytes, expected {want}"))
    }
}

pub fn phys_table_name(t: &Table, owner: Option<&Table>) -> String {
    match owner {
        Some(o) => format!("_{}_{}", o.name, t.name),
        None => format!("_{}", t.name),
    }
}

fn bad(table: &str, field: &str, why: impl Into<String>) -> Error {
    Error::Parse {
        what: "DBSCHEMA",
        at: if table.is_empty() {
            field.to_string()
        } else {
            format!("{table}.{field}")
        },
        why: why.into(),
    }
}

fn bin_type(length: u32) -> ColType {
    if length == VAR {
        ColType::I
    } else if length & VAR != 0 {
        ColType::VB(length & !VAR)
    } else {
        ColType::B(length)
    }
}

fn str_type(length: u32) -> ColType {
    if length == VAR {
        ColType::NT
    } else if length & VAR != 0 {
        ColType::NVC(length & !VAR)
    } else {
        ColType::NC(length)
    }
}

pub fn of_field(f: &Field, index: usize) -> Result<Vec<Column>> {
    field_columns(f, index, "")
}

fn field_columns(f: &Field, index: usize, table: &str) -> Result<Vec<Column>> {
    let err = |why: &str| bad(table, &f.name, why);
    let mk = |suffix: &str, ty, role, part| Column {
        name: format!("_{}{}", f.name, suffix),
        ty,
        role,
        nullable: f.nullable,
        origin: Origin::Field(index),
        part,
    };
    let mut out = Vec::new();
    match f.types.as_slice() {
        [] => return Err(err("field without types")),
        [t] => match t.code {
            TypeCode::E => return Err(err("E type alone")),
            TypeCode::V => out.push(mk("", ColType::RV, ColRole::RowVersion, Part::Single)),
            TypeCode::L => out.push(mk("", ColType::L, ColRole::Bool, Part::Single)),
            TypeCode::N => out.push(mk(
                "",
                ColType::N(t.length, t.precision),
                ColRole::Number,
                Part::Single,
            )),
            TypeCode::T => out.push(mk("", ColType::DT, ColRole::DateTime, Part::Single)),
            TypeCode::S => out.push(mk("", str_type(t.length), ColRole::Str, Part::Single)),
            TypeCode::B => out.push(mk("", bin_type(t.length), ColRole::Binary, Part::Single)),
            TypeCode::R => match t.ref_kind {
                2 | 3 => out.push(mk("RRef", ColType::B(16), ColRole::Ref, Part::Single)),
                4 => {
                    out.push(mk("TRef", ColType::B(4), ColRole::TableNo, Part::TRef));
                    out.push(mk("RRef", ColType::B(16), ColRole::Ref, Part::RRef));
                }
                k => return Err(err(&format!("reference kind {k}"))),
            },
        },
        types => {
            if types.first().map(|t| t.code) != Some(TypeCode::E) {
                return Err(err("several types without a leading E"));
            }
            for (i, t) in types.iter().enumerate() {
                if types.iter().take(i).any(|p| p.code == t.code) {
                    return Err(err(&format!("repeated type {:?} in a composite", t.code)));
                }
                composite_part(t, i == 0, &mk, &mut out).map_err(|why| match why {
                    PartError::Bad(w) => err(&w),
                    PartError::Unsupported(w) => {
                        if table.is_empty() {
                            Error::Unsupported(format!("{}: {w}", f.name))
                        } else {
                            Error::Unsupported(format!("{table}.{}: {w}", f.name))
                        }
                    }
                })?;
            }
        }
    }
    Ok(out)
}

enum PartError {
    Bad(String),
    Unsupported(String),
}

fn composite_part(
    t: &LType,
    first: bool,
    mk: &dyn Fn(&str, ColType, ColRole, Part) -> Column,
    out: &mut Vec<Column>,
) -> std::result::Result<(), PartError> {
    match t.code {
        TypeCode::E if first => out.push(mk("_TYPE", ColType::B(1), ColRole::TypeCode, Part::Type)),
        TypeCode::E => return Err(PartError::Bad("repeated E".into())),
        TypeCode::V => return Err(PartError::Bad("V in a composite field".into())),
        TypeCode::L => out.push(mk("_L", ColType::L, ColRole::Bool, Part::L)),
        TypeCode::N => out.push(mk(
            "_N",
            ColType::N(t.length, t.precision),
            ColRole::Number,
            Part::N,
        )),
        TypeCode::T => out.push(mk("_T", ColType::DT, ColRole::DateTime, Part::T)),
        TypeCode::S => {
            if t.length == VAR {
                return Err(PartError::Unsupported(
                    "unlimited string in a composite field".into(),
                ));
            }
            out.push(mk("_S", str_type(t.length), ColRole::Str, Part::S));
        }
        TypeCode::B => out.push(mk("_B", bin_type(t.length), ColRole::Binary, Part::B)),
        TypeCode::R => match t.ref_kind {
            2 | 3 => out.push(mk("_RRRef", ColType::B(16), ColRole::Ref, Part::RRRef)),
            4 => {
                out.push(mk("_RTRef", ColType::B(4), ColRole::TableNo, Part::RTRef));
                out.push(mk("_RRRef", ColType::B(16), ColRole::Ref, Part::RRRef));
            }
            k => return Err(PartError::Bad(format!("reference kind {k}"))),
        },
    }
    Ok(())
}

pub fn of_table(t: &Table, owner: Option<&Table>) -> Result<Vec<Column>> {
    let mut out = Vec::new();
    if let Some(o) = owner {
        out.push(Column {
            name: format!("{}_IDRRef", phys_table_name(o, None)),
            ty: ColType::B(16),
            role: ColRole::Ref,
            nullable: false,
            origin: Origin::OwnerId,
            part: Part::Single,
        });
        let names = o.splitters[1].iter().flatten();
        for name in names {
            let at = format!("{}.splitters[1]", o.name);
            let (i, f) = o
                .fields
                .iter()
                .enumerate()
                .find(|(_, f)| f.name == *name)
                .ok_or_else(|| Error::Parse {
                    what: "DBSCHEMA",
                    at,
                    why: format!("splitter field {name} not found in the owner"),
                })?;
            if f.types.iter().any(|t| t.code == TypeCode::V) {
                return Err(Error::Parse {
                    what: "DBSCHEMA",
                    at: format!("{}.splitters[1]", o.name),
                    why: format!("splitter field {name} is a row version"),
                });
            }
            for mut c in field_columns(f, i, &o.name)? {
                c.origin = Origin::OwnerSplitter(name.clone());
                out.push(c);
            }
        }
        out.push(Column {
            name: "_KeyField".into(),
            ty: ColType::B(4),
            role: ColRole::KeyField,
            nullable: false,
            origin: Origin::KeyField,
            part: Part::Single,
        });
    }
    for (i, f) in t.fields.iter().enumerate() {
        out.extend(field_columns(f, i, &t.name)?);
    }
    let mut seen = std::collections::HashSet::with_capacity(out.len());
    for c in &out {
        if !seen.insert(c.name.to_lowercase()) {
            let at = match &c.origin {
                Origin::OwnerSplitter(_) => {
                    format!("{}.splitters[1]", owner.map_or("", |o| o.name.as_str()))
                }
                Origin::Field(i) => {
                    let n = t.fields.get(*i).map_or("", |f| f.name.as_str());
                    format!("{}.{n}", t.name)
                }
                _ => t.name.clone(),
            };
            return Err(Error::Parse {
                what: "DBSCHEMA",
                at,
                why: format!("duplicate column {}", c.name),
            });
        }
    }
    Ok(out)
}

pub fn dt_columns(t: &Table, owner: Option<&Table>) -> Result<Vec<Column>> {
    Ok(of_table(t, owner)?
        .into_iter()
        .filter(Column::in_dt)
        .collect())
}
