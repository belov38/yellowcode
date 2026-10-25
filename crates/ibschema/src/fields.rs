use onecd::{DateTime, Numeric};

use crate::columns::{ColRole, ColType, Column, Part};
use crate::error::{Error, Result};
use crate::logical::TypeCode;
use crate::storage::{FieldId, FieldInfo, TableInfo};
use crate::values::{ColumnValue, RefId, Uuid16, type_code, utf16_len};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldValue {
    Null,
    Undefined,
    Bool(bool),
    Number(Numeric),
    Date(DateTime),
    String(String),
    Uuid(Uuid16),
    Ref { table: u32, id: RefId },
    Storage(Vec<u8>),
    Binary(Vec<u8>),
    RowVersion([u32; 4]),
}

pub(crate) const NULL_NOT_ALLOWED: &str = "NULL not allowed";

pub const GUID_LE_FIELDS: [&str; 3] = ["RegID", "ConstID", "MDID"];

pub fn ref_number(ref_table: &str) -> Option<u32> {
    let head = ref_table.trim_end_matches(|c: char| c.is_ascii_digit());
    ref_table
        .get(head.len()..)
        .filter(|d| !d.is_empty())?
        .parse()
        .ok()
}

pub fn empty_value(c: &Column) -> ColumnValue {
    match c.ty {
        ColType::L => ColumnValue::Bool(false),
        ColType::N(_, f) => {
            let text = if f == 0 {
                "0".to_string()
            } else {
                format!("0.{}", "0".repeat(f as usize))
            };
            ColumnValue::Number(Numeric::parse(&text).unwrap_or_else(|| Numeric::from_i64(0)))
        }
        ColType::NC(n) => ColumnValue::Str(" ".repeat(n as usize)),
        ColType::NVC(_) | ColType::NT => ColumnValue::Str(String::new()),
        ColType::DT => ColumnValue::DateTime(DateTime::EMPTY),
        ColType::B(n) => ColumnValue::Bytes(vec![0; n as usize]),
        ColType::VB(_) | ColType::I => ColumnValue::Bytes(Vec::new()),
        ColType::RV => ColumnValue::Null,
    }
}

pub fn is_version(f: &FieldInfo) -> bool {
    matches!(f.types.as_slice(), [t] if t.code == TypeCode::V)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    Single,
    Pair,
    Composite,
}

fn shape<'a>(
    t: &'a TableInfo,
    f: &FieldInfo,
) -> std::result::Result<(Shape, &'a [Column]), String> {
    let cols = t
        .columns
        .get(f.columns.clone())
        .filter(|c| !c.is_empty())
        .ok_or_else(|| "field has no columns".to_string())?;
    let first = cols.first().map(|c| c.part);
    let sh = match first {
        Some(Part::TRef) => Shape::Pair,
        Some(Part::Type) => Shape::Composite,
        _ => Shape::Single,
    };
    if sh == Shape::Single && cols.len() != 1 {
        return Err(format!("{} columns in a single-column field", cols.len()));
    }
    if sh == Shape::Pair && cols.len() != 2 {
        return Err(format!("{} columns in a reference pair", cols.len()));
    }
    Ok((sh, cols))
}

fn no_fit(t: &TableInfo, f: &FieldInfo, why: impl Into<String>) -> Error {
    Error::ValueDoesNotFit {
        table: t.logical.clone(),
        column: f.name.clone(),
        why: why.into(),
    }
}

fn bad_value(t: &TableInfo, f: &FieldInfo, why: impl Into<String>) -> Error {
    Error::BadValue {
        table: t.logical.clone(),
        field: f.name.clone(),
        why: why.into(),
    }
}

fn is_fld_name(name: &str) -> bool {
    name.strip_prefix("Fld")
        .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
}

fn type_table(f: &FieldInfo) -> Option<u32> {
    f.types
        .iter()
        .find(|t| t.code == TypeCode::R)
        .and_then(|t| ref_number(&t.ref_table))
}

pub fn single_ref_table(f: &FieldInfo) -> Option<u32> {
    match f.types.as_slice() {
        [t] if t.code == TypeCode::R => ref_number(&t.ref_table),
        _ => None,
    }
}

fn key_u32(n: &Numeric) -> Option<u32> {
    let all = n.digits();
    let int_len = all.len().saturating_sub(n.scale() as usize);
    let (int, frac) = all.split_at(int_len);
    if frac.iter().any(|&d| d != 0) {
        return None;
    }
    let mut v: u64 = 0;
    for &d in int {
        v = v * 10 + u64::from(d);
        if v > u64::from(u32::MAX) {
            return None;
        }
    }
    if n.is_negative() && v != 0 {
        return None;
    }
    u32::try_from(v).ok()
}

fn bytes16(v: &ColumnValue) -> Option<[u8; 16]> {
    match v {
        ColumnValue::Bytes(b) => b.as_slice().try_into().ok(),
        _ => None,
    }
}

fn be4(v: &ColumnValue) -> Option<u32> {
    match v {
        ColumnValue::Bytes(b) => <[u8; 4]>::try_from(b.as_slice())
            .ok()
            .map(u32::from_be_bytes),
        _ => None,
    }
}

pub fn encode(t: &TableInfo, f: FieldId, v: &FieldValue) -> Result<Vec<ColumnValue>> {
    let fi = t.field(f).ok_or_else(|| Error::ValueDoesNotFit {
        table: t.logical.clone(),
        column: format!("#{}", f.0),
        why: "no such field".into(),
    })?;
    if is_version(fi) {
        return Err(Error::Unsupported(format!(
            "{}.{}: the row version is not written",
            t.logical, fi.name
        )));
    }
    let (sh, cols) = shape(t, fi).map_err(|w| no_fit(t, fi, w))?;
    let out = match sh {
        Shape::Single => encode_single(t, fi, cols, v)?,
        Shape::Pair => encode_pair(t, fi, cols, v)?,
        Shape::Composite => encode_composite(t, fi, cols, v)?,
    };
    for (c, x) in cols.iter().zip(&out) {
        c.check(x)
            .map_err(|w| no_fit(t, fi, format!("{}: {w}", c.name)))?;
    }
    Ok(out)
}

pub fn encode_bound(t: &TableInfo, f: FieldId, v: &FieldValue) -> Result<Vec<ColumnValue>> {
    if let FieldValue::String(s) = v
        && let Some(fi) = t.field(f)
        && let Some(slot) = t
            .columns
            .get(fi.columns.clone())
            .and_then(|cols| cols.iter().find(|c| c.role == ColRole::Str))
            .and_then(|c| match c.ty {
                ColType::NC(n) | ColType::NVC(n) => Some(n as usize),
                _ => None,
            })
        && utf16_len(s) > slot
    {
        let mut cut = String::new();
        let mut used = 0;
        for ch in s.chars() {
            used += ch.len_utf16();
            if used > slot {
                break;
            }
            cut.push(ch);
        }
        return encode(t, f, &FieldValue::String(cut));
    }
    encode(t, f, v)
}

fn null_cols(t: &TableInfo, fi: &FieldInfo, cols: &[Column]) -> Result<Vec<ColumnValue>> {
    if fi.nullable {
        Ok(cols.iter().map(|_| ColumnValue::Null).collect())
    } else {
        Err(no_fit(t, fi, NULL_NOT_ALLOWED))
    }
}

fn kind_error(t: &TableInfo, fi: &FieldInfo, v: &FieldValue, want: &str) -> Error {
    no_fit(t, fi, format!("{v:?} given, the column holds {want}"))
}

fn pad_nc(c: &Column, s: &str) -> String {
    match c.ty {
        ColType::NC(n) => {
            let have = utf16_len(s);
            let mut out = s.to_string();
            out.extend(std::iter::repeat_n(' ', (n as usize).saturating_sub(have)));
            out
        }
        _ => s.to_string(),
    }
}

fn encode_single(
    t: &TableInfo,
    fi: &FieldInfo,
    cols: &[Column],
    v: &FieldValue,
) -> Result<Vec<ColumnValue>> {
    let Some(c) = cols.first() else {
        return Err(no_fit(t, fi, "field has no columns"));
    };
    if *v == FieldValue::Null {
        return null_cols(t, fi, cols);
    }
    let one = |x: ColumnValue| Ok(vec![x]);
    match (c.role, v) {
        (ColRole::Ref, FieldValue::Uuid(u)) if type_table(fi).is_none() => {
            one(ColumnValue::Bytes(RefId::from_uuid(*u).0.to_vec()))
        }
        (ColRole::Ref, FieldValue::Ref { table, id }) => {
            match type_table(fi) {
                Some(n) if n == *table => {}
                Some(n) => {
                    return Err(no_fit(
                        t,
                        fi,
                        format!("reference to table {table}, the type is table {n}"),
                    ));
                }
                None => return Err(no_fit(t, fi, "reference type without a table number")),
            }
            one(ColumnValue::Bytes(id.0.to_vec()))
        }
        (ColRole::Bool, FieldValue::Bool(b)) => one(ColumnValue::Bool(*b)),
        (ColRole::Number, FieldValue::Number(n)) => one(ColumnValue::Number(n.clone())),
        (ColRole::DateTime, FieldValue::Date(d)) => one(ColumnValue::DateTime(*d)),
        (ColRole::Str, FieldValue::String(s)) => one(ColumnValue::Str(pad_nc(c, s))),
        (ColRole::Binary, FieldValue::Uuid(u)) if c.ty == ColType::B(16) => {
            let b = if GUID_LE_FIELDS.contains(&fi.name.as_str()) {
                u.to_bytes_le()
            } else {
                RefId::from_uuid(*u).0
            };
            one(ColumnValue::Bytes(b.to_vec()))
        }
        (ColRole::Binary, FieldValue::Storage(b)) if c.ty == ColType::I => {
            one(ColumnValue::Bytes(b.clone()))
        }
        (ColRole::Binary, FieldValue::Binary(b))
            if c.ty != ColType::B(16) && !(c.ty == ColType::I && is_fld_name(&fi.name)) =>
        {
            one(ColumnValue::Bytes(b.clone()))
        }
        (ColRole::KeyField, FieldValue::Number(n)) => match key_u32(n) {
            Some(k) => one(ColumnValue::Bytes(k.to_be_bytes().to_vec())),
            None => Err(no_fit(t, fi, "key is not an integer in 0..=4294967295")),
        },
        _ => Err(kind_error(t, fi, v, &format!("{:?} ({:?})", c.role, c.ty))),
    }
}

fn encode_pair(
    t: &TableInfo,
    fi: &FieldInfo,
    cols: &[Column],
    v: &FieldValue,
) -> Result<Vec<ColumnValue>> {
    match v {
        FieldValue::Null => null_cols(t, fi, cols),
        FieldValue::Ref { table, id } => Ok(vec![
            ColumnValue::Bytes(table.to_be_bytes().to_vec()),
            ColumnValue::Bytes(id.0.to_vec()),
        ]),
        _ => Err(kind_error(t, fi, v, "a reference with a table")),
    }
}

fn encode_composite(
    t: &TableInfo,
    fi: &FieldInfo,
    cols: &[Column],
    v: &FieldValue,
) -> Result<Vec<ColumnValue>> {
    if *v == FieldValue::Null {
        return null_cols(t, fi, cols);
    }
    let mut out: Vec<ColumnValue> = cols.iter().map(empty_value).collect();
    let missing = || no_fit(t, fi, format!("the field has no column for {v:?}"));
    let mut set = |part: Part, x: ColumnValue| -> Result<()> {
        let i = cols
            .iter()
            .position(|c| c.part == part)
            .ok_or_else(missing)?;
        if let Some(slot) = out.get_mut(i) {
            *slot = x;
        }
        Ok(())
    };
    let str_col = || cols.iter().find(|c| c.part == Part::S);
    let code = match v {
        FieldValue::Undefined => type_code::UNDEFINED,
        FieldValue::Bool(b) => {
            set(Part::L, ColumnValue::Bool(*b))?;
            type_code::BOOL
        }
        FieldValue::Number(n) => {
            set(Part::N, ColumnValue::Number(n.clone()))?;
            type_code::NUMBER
        }
        FieldValue::Date(d) => {
            set(Part::T, ColumnValue::DateTime(*d))?;
            type_code::DATE
        }
        FieldValue::String(s) => {
            let s = str_col().map_or_else(|| s.clone(), |c| pad_nc(c, s));
            set(Part::S, ColumnValue::Str(s))?;
            type_code::STRING
        }
        FieldValue::Uuid(u) => {
            set(Part::B, ColumnValue::Bytes(RefId::from_uuid(*u).0.to_vec()))?;
            type_code::UUID
        }
        FieldValue::Ref { table, id } => {
            if cols.iter().any(|c| c.part == Part::RTRef) {
                set(
                    Part::RTRef,
                    ColumnValue::Bytes(table.to_be_bytes().to_vec()),
                )?;
            } else {
                match type_table(fi) {
                    Some(n) if n == *table => {}
                    Some(n) => {
                        return Err(no_fit(
                            t,
                            fi,
                            format!("reference to table {table}, the type is table {n}"),
                        ));
                    }
                    None => return Err(no_fit(t, fi, "reference type without a table number")),
                }
            }
            set(Part::RRRef, ColumnValue::Bytes(id.0.to_vec()))?;
            type_code::REF
        }
        _ => return Err(missing()),
    };
    set(Part::Type, ColumnValue::Bytes(vec![code]))?;
    Ok(out)
}

pub fn decode(t: &TableInfo, f: FieldId, cols: &[ColumnValue]) -> Result<FieldValue> {
    let fi = t.field(f).ok_or_else(|| Error::BadValue {
        table: t.logical.clone(),
        field: format!("#{}", f.0),
        why: "no such field".into(),
    })?;
    if is_version(fi) {
        return Err(Error::Unsupported(format!(
            "{}.{}: the row version is read by the store",
            t.logical, fi.name
        )));
    }
    let (sh, meta) = shape(t, fi).map_err(|w| bad_value(t, fi, w))?;
    if meta.len() != cols.len() {
        return Err(bad_value(
            t,
            fi,
            format!("{} values for {} columns", cols.len(), meta.len()),
        ));
    }
    let r = match sh {
        Shape::Single => decode_single(fi, meta, cols),
        Shape::Pair => decode_pair(cols),
        Shape::Composite => decode_composite(fi, meta, cols),
    };
    r.map_err(|w| bad_value(t, fi, w))
}

type Dec = std::result::Result<FieldValue, String>;

fn decode_single(fi: &FieldInfo, meta: &[Column], cols: &[ColumnValue]) -> Dec {
    let (Some(c), Some(v)) = (meta.first(), cols.first()) else {
        return Err("no column".into());
    };
    if *v == ColumnValue::Null {
        return Ok(FieldValue::Null);
    }
    let wrong = || Err(format!("{v:?} in a {:?} column", c.role));
    match (c.role, v) {
        (ColRole::Ref, _) => {
            let id = bytes16(v).ok_or_else(|| format!("{v:?} in a reference column"))?;
            let Some(table) = type_table(fi) else {
                return Ok(FieldValue::Uuid(RefId(id).to_uuid()));
            };
            Ok(FieldValue::Ref {
                table,
                id: RefId(id),
            })
        }
        (ColRole::Bool, ColumnValue::Bool(b)) => Ok(FieldValue::Bool(*b)),
        (ColRole::Number, ColumnValue::Number(n)) => Ok(FieldValue::Number(n.clone())),
        (ColRole::DateTime, ColumnValue::DateTime(d)) => Ok(FieldValue::Date(*d)),
        (ColRole::Str, ColumnValue::Str(s)) => Ok(FieldValue::String(s.clone())),
        (ColRole::KeyField, _) => match be4(v) {
            Some(k) => Ok(FieldValue::Number(Numeric::from_i64(i64::from(k)))),
            None => wrong(),
        },
        (ColRole::Binary, ColumnValue::Bytes(b)) => match c.ty {
            ColType::B(16) => {
                let a = bytes16(v).ok_or("B(16) value of another length")?;
                Ok(FieldValue::Uuid(
                    if GUID_LE_FIELDS.contains(&fi.name.as_str()) {
                        Uuid16::from_bytes_le(a)
                    } else {
                        RefId(a).to_uuid()
                    },
                ))
            }
            ColType::I if is_fld_name(&fi.name) => Ok(FieldValue::Storage(b.clone())),
            _ => Ok(FieldValue::Binary(b.clone())),
        },
        _ => wrong(),
    }
}

fn decode_pair(cols: &[ColumnValue]) -> Dec {
    let (Some(tr), Some(rr)) = (cols.first(), cols.get(1)) else {
        return Err("no columns".into());
    };
    match (*tr == ColumnValue::Null, *rr == ColumnValue::Null) {
        (true, true) => return Ok(FieldValue::Null),
        (false, false) => {}
        _ => return Err("half-NULL reference pair".into()),
    }
    let table = be4(tr).ok_or("table number is not 4 bytes")?;
    let id = bytes16(rr).ok_or("reference is not 16 bytes")?;
    Ok(FieldValue::Ref {
        table,
        id: RefId(id),
    })
}

fn decode_composite(fi: &FieldInfo, meta: &[Column], cols: &[ColumnValue]) -> Dec {
    let at = |part: Part| -> Option<&ColumnValue> {
        let i = meta.iter().position(|c| c.part == part)?;
        cols.get(i)
    };
    let Some(ty) = at(Part::Type) else {
        return Err("no _TYPE column".into());
    };
    let code = match ty {
        ColumnValue::Null => return Ok(FieldValue::Null),
        ColumnValue::Bytes(b) => match b.as_slice() {
            [c] => *c,
            _ => return Err(format!("_TYPE of {} bytes", b.len())),
        },
        other => return Err(format!("{other:?} in _TYPE")),
    };
    let need = |part: Part| at(part).ok_or_else(|| format!("no column for _TYPE {code:02x}"));
    match code {
        type_code::UNDEFINED => Ok(FieldValue::Undefined),
        type_code::BOOL => match need(Part::L)? {
            ColumnValue::Bool(b) => Ok(FieldValue::Bool(*b)),
            v => Err(format!("{v:?} in _L")),
        },
        type_code::NUMBER => match need(Part::N)? {
            ColumnValue::Number(n) => Ok(FieldValue::Number(n.clone())),
            v => Err(format!("{v:?} in _N")),
        },
        type_code::DATE => match need(Part::T)? {
            ColumnValue::DateTime(d) => Ok(FieldValue::Date(*d)),
            v => Err(format!("{v:?} in _T")),
        },
        type_code::STRING => match need(Part::S)? {
            ColumnValue::Str(s) => Ok(FieldValue::String(s.clone())),
            v => Err(format!("{v:?} in _S")),
        },
        type_code::UUID => {
            let v = need(Part::B)?;
            let a = bytes16(v).ok_or_else(|| format!("{v:?} in _B"))?;
            Ok(FieldValue::Uuid(RefId(a).to_uuid()))
        }
        type_code::REF => {
            let v = need(Part::RRRef)?;
            let id = bytes16(v).ok_or_else(|| format!("{v:?} in _RRRef"))?;
            let table = match at(Part::RTRef) {
                Some(tr) => be4(tr).ok_or_else(|| format!("{tr:?} in _RTRef"))?,
                None => type_table(fi).ok_or("reference type without a table number")?,
            };
            Ok(FieldValue::Ref {
                table,
                id: RefId(id),
            })
        }
        other => Err(format!("unknown _TYPE {other:02x}")),
    }
}
