use std::cmp::Ordering;

use bsl_prim::Collation;
use query_ir::QValue;
use query_syntax::ast::CmpOp;

use crate::coll::{ExecCollation, Keys, infallible};
use crate::key::key_with;
use query_ir::ExecError;

pub fn rank(v: &QValue) -> u8 {
    match v {
        QValue::Null => 0,
        QValue::Undefined => 1,
        QValue::Bool(_) => 2,
        QValue::Number(_) => 3,
        QValue::Date(_) => 4,
        QValue::String(_) => 5,
        QValue::Uuid(_) => 6,
        QValue::Ref { .. } => 7,
        QValue::Type(_) => 8,
        QValue::SysEnum(..) => 9,
        QValue::Binary(_) => 10,
        QValue::Storage(_) => 11,
        QValue::Nested(_) => 12,
        QValue::PointInTime(_) => 13,
        QValue::Boundary(_) => 14,
    }
}

pub fn cmp(a: &QValue, b: &QValue, c: &dyn Collation) -> Ordering {
    infallible(compare_cmp_with(a, b, c))
}

pub fn try_cmp(a: &QValue, b: &QValue, c: ExecCollation<'_>) -> Result<Ordering, ExecError> {
    compare_cmp_with(a, b, c)
}

pub(crate) fn compare_cmp_with<C: Keys>(
    a: &QValue,
    b: &QValue,
    c: C,
) -> Result<Ordering, C::Error> {
    Ok(match (a, b) {
        (QValue::Bool(x), QValue::Bool(y)) => x.cmp(y),
        (QValue::Number(x), QValue::Number(y)) => x.cmp(y),
        (QValue::Date(x), QValue::Date(y)) => x.ticks().cmp(&y.ticks()),
        (QValue::PointInTime(x), QValue::PointInTime(y)) => x
            .date
            .ticks()
            .cmp(&y.date.ticks())
            .then_with(|| x.reference.cmp(&y.reference)),
        _ => match rank(a).cmp(&rank(b)) {
            Ordering::Equal if rank(a) == 0 || rank(a) == 1 => Ordering::Equal,
            Ordering::Equal => key_with(a, c)?.cmp(&key_with(b, c)?),
            order => order,
        },
    })
}

pub fn eq3(a: &QValue, b: &QValue, c: &dyn Collation) -> Option<bool> {
    infallible(eq3_with(a, b, c))
}

pub fn try_eq3(a: &QValue, b: &QValue, c: ExecCollation<'_>) -> Result<Option<bool>, ExecError> {
    eq3_with(a, b, c)
}

fn eq3_with<C: Keys>(a: &QValue, b: &QValue, c: C) -> Result<Option<bool>, C::Error> {
    if matches!(a, QValue::Null) || matches!(b, QValue::Null) {
        return Ok(None);
    }
    Ok(Some(
        rank(a) == rank(b) && compare_cmp_with(a, b, c)? == Ordering::Equal,
    ))
}

pub fn compare(op: CmpOp, a: &QValue, b: &QValue, c: &dyn Collation) -> Option<bool> {
    infallible(compare_with(op, a, b, c))
}

pub fn try_compare(
    op: CmpOp,
    a: &QValue,
    b: &QValue,
    c: ExecCollation<'_>,
) -> Result<Option<bool>, ExecError> {
    compare_with(op, a, b, c)
}

fn compare_with<C: Keys>(
    op: CmpOp,
    a: &QValue,
    b: &QValue,
    c: C,
) -> Result<Option<bool>, C::Error> {
    if matches!(a, QValue::Null) || matches!(b, QValue::Null) {
        return Ok(None);
    }
    let o = compare_cmp_with(a, b, c)?;
    Ok(Some(match op {
        CmpOp::Eq => o == Ordering::Equal,
        CmpOp::Ne => o != Ordering::Equal,
        CmpOp::Lt => o == Ordering::Less,
        CmpOp::Gt => o == Ordering::Greater,
        CmpOp::Le => o != Ordering::Greater,
        CmpOp::Ge => o != Ordering::Less,
    }))
}
