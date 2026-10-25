use std::borrow::Cow;

use md_model::{MdKind, MdObject, TypeDescription};

use super::Names;
use super::ctx::Ctx;
use crate::error::{Error, Result};
use crate::logical::{LType, TypeCode, VAR};
use crate::names::purpose;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Class {
    S { len: u32, fixed: bool },
    N { digits: u32, fraction: u32 },
    T,
    L,
    Storage,
    Uuid,
    Ref(Option<RefTarget>),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RefTarget {
    pub table: String,
    pub number: u32,
}

pub(crate) fn main_purpose(kind: MdKind) -> Option<&'static str> {
    Some(match kind {
        MdKind::Catalog => purpose::REFERENCE,
        MdKind::Document => purpose::DOCUMENT,
        MdKind::Enum => purpose::ENUM,
        MdKind::ChartOfCharacteristicTypes => purpose::CHRC,
        MdKind::ChartOfAccounts => purpose::ACC,
        MdKind::ExchangePlan => purpose::NODE,
        MdKind::ChartOfCalculationTypes => purpose::CKINDS,
        MdKind::BusinessProcess => purpose::BPR,
        MdKind::Task => purpose::TASK,
        _ => return None,
    })
}

pub(crate) fn ref_target(o: &MdObject, purpose: &str, names: &mut Names) -> Result<RefTarget> {
    let number = names.number(o.id, purpose)?;
    Ok(RefTarget {
        table: format!("{purpose}{number}"),
        number,
    })
}

pub(crate) fn main_target(o: &MdObject, names: &mut Names) -> Result<RefTarget> {
    let p = main_purpose(o.kind).ok_or_else(|| {
        Error::Unsupported(format!(
            "{}.{}: not a reference kind",
            o.kind.as_str(),
            o.name
        ))
    })?;
    ref_target(o, p, names)
}

fn ref_kind(prefix: &str) -> Option<(MdKind, &'static str)> {
    Some(match prefix {
        "CatalogRef" => (MdKind::Catalog, purpose::REFERENCE),
        "DocumentRef" => (MdKind::Document, purpose::DOCUMENT),
        "EnumRef" => (MdKind::Enum, purpose::ENUM),
        "ChartOfCharacteristicTypesRef" => (MdKind::ChartOfCharacteristicTypes, purpose::CHRC),
        "ChartOfAccountsRef" => (MdKind::ChartOfAccounts, purpose::ACC),
        "ExchangePlanRef" => (MdKind::ExchangePlan, purpose::NODE),
        "ChartOfCalculationTypesRef" => (MdKind::ChartOfCalculationTypes, purpose::CKINDS),
        "BusinessProcessRef" => (MdKind::BusinessProcess, purpose::BPR),
        "BusinessProcessRoutePointRef" => (MdKind::BusinessProcess, purpose::BPR_POINTS),
        "TaskRef" => (MdKind::Task, purpose::TASK),
        _ => return None,
    })
}

pub(crate) fn classes(
    cx: &Ctx,
    td: &TypeDescription,
    at: &str,
    names: &mut Names,
) -> Result<Vec<Class>> {
    let td = if td.types.iter().any(|t| t.set) {
        Cow::Owned(cx.cfg.expand_type(td))
    } else {
        Cow::Borrowed(td)
    };
    let bad = |why: String| Error::Unsupported(format!("{at}: {why}"));
    let mut out: Vec<Class> = Vec::new();
    for t in &td.types {
        let q = t.qname.as_str();
        let got = match q {
            "xs:string" => {
                let s = td
                    .string
                    .ok_or_else(|| bad("string without qualifiers".into()))?;
                vec![Class::S {
                    len: s.length,
                    fixed: s.fixed,
                }]
            }
            "xs:decimal" => {
                let n = td
                    .number
                    .ok_or_else(|| bad("number without qualifiers".into()))?;
                vec![Class::N {
                    digits: n.digits,
                    fraction: n.fraction,
                }]
            }
            "xs:dateTime" => vec![Class::T],
            "xs:boolean" => vec![Class::L],
            "v8:ValueStorage" => vec![Class::Storage],
            "v8:UUID" => vec![Class::Uuid],
            _ if t.set => {
                if q.starts_with("cfg:DefinedType.") || q.starts_with("cfg:Characteristic.") {
                    return Err(bad(format!("type set {q} does not expand")));
                }
                match q.strip_prefix("cfg:").and_then(ref_kind) {
                    Some((kind, purpose)) => match cx.md.of_kind(kind) {
                        [o] => vec![Class::Ref(Some(ref_target(o, purpose, names)?))],
                        _ => vec![Class::Ref(None), Class::Ref(None)],
                    },
                    None => vec![Class::Ref(None), Class::Ref(None)],
                }
            }
            _ => {
                let (prefix, name) = q
                    .strip_prefix("cfg:")
                    .and_then(|r| r.split_once('.'))
                    .ok_or_else(|| bad(format!("type {q} is not stored")))?;
                let (kind, purpose) =
                    ref_kind(prefix).ok_or_else(|| bad(format!("type {q} is not stored")))?;
                let o = cx
                    .md
                    .find(kind, name)
                    .ok_or_else(|| bad(format!("type {q}: unknown object")))?;
                vec![Class::Ref(Some(ref_target(o, purpose, names)?))]
            }
        };
        for c in got {
            if c == Class::Ref(None) || !out.contains(&c) {
                out.push(c);
            }
        }
    }
    Ok(out)
}

fn unsupported(why: &str) -> Error {
    Error::Unsupported(why.to_string())
}

fn str_length(len: u32, fixed: bool) -> Result<u32> {
    if len == 0 {
        Ok(VAR)
    } else if len >= VAR {
        Err(unsupported("string length out of range"))
    } else if fixed {
        Ok(len)
    } else {
        Ok(VAR | len)
    }
}

fn number(digits: u32, fraction: u32, totals: bool) -> Result<LType> {
    let digits = if totals {
        digits
            .checked_add(6)
            .ok_or_else(|| unsupported("number digits out of range"))?
    } else {
        digits
    };
    Ok(LType::new(TypeCode::N, digits, fraction))
}

pub fn logical_types(classes: &[Class], totals: bool) -> Result<Vec<LType>> {
    let mut cs: Vec<&Class> = Vec::with_capacity(classes.len());
    for c in classes {
        if !cs.contains(&c) {
            cs.push(c);
        }
    }
    match cs.as_slice() {
        [] => return Err(unsupported("a type without classes")),
        [c] if **c != Class::Ref(None) => {
            return Ok(vec![match c {
                Class::S { len, fixed } => LType::new(TypeCode::S, str_length(*len, *fixed)?, 0),
                Class::N { digits, fraction } => number(*digits, *fraction, totals)?,
                Class::T => LType::new(TypeCode::T, 0, 0),
                Class::L => LType::new(TypeCode::L, 0, 0),
                Class::Storage => LType::new(TypeCode::B, VAR, 0),
                Class::Uuid => LType::new(TypeCode::B, 16, 0),
                Class::Ref(t) => LType::reference(t.as_ref().map_or("", |t| t.table.as_str()), 3),
            }]);
        }
        _ => {}
    }
    if cs.contains(&&Class::Storage) {
        return Err(unsupported("value storage in a composite type"));
    }
    let mut out = vec![LType::new(TypeCode::E, 0, 0)];
    if cs.contains(&&Class::L) {
        out.push(LType::new(TypeCode::L, 0, 0));
    }
    if let Some(Class::N { digits, fraction }) = cs.iter().find(|c| matches!(c, Class::N { .. })) {
        out.push(number(*digits, *fraction, totals)?);
    }
    if cs.contains(&&Class::T) {
        out.push(LType::new(TypeCode::T, 0, 0));
    }
    if let Some(Class::S { len, fixed }) = cs.iter().find(|c| matches!(c, Class::S { .. })) {
        if *len == 0 {
            return Err(unsupported("unlimited string in a composite type"));
        }
        out.push(LType::new(TypeCode::S, str_length(*len, *fixed)?, 0));
    }
    if cs.contains(&&Class::Uuid) {
        out.push(LType::new(TypeCode::B, 16, 0));
    }
    let refs: Vec<&Option<RefTarget>> = cs
        .iter()
        .filter_map(|c| match c {
            Class::Ref(t) => Some(t),
            _ => None,
        })
        .collect();
    match refs.as_slice() {
        [] => {}
        [Some(t)] => out.push(LType::reference(t.table.as_str(), 3)),
        _ => out.push(LType::reference("", 4)),
    }
    Ok(out)
}
