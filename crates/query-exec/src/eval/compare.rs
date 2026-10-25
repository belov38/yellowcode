use std::cmp::Ordering;

use bsl_prim::Collation;
use query_ir::QValue;
use query_syntax::ast::CmpOp;

use crate::key::key_of;

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
    match (a, b) {
        (QValue::Bool(x), QValue::Bool(y)) => x.cmp(y),
        (QValue::Number(x), QValue::Number(y)) => x.cmp(y),
        (QValue::Date(x), QValue::Date(y)) => x.ticks().cmp(&y.ticks()),
        (QValue::PointInTime(x), QValue::PointInTime(y)) => x
            .date
            .ticks()
            .cmp(&y.date.ticks())
            .then_with(|| x.reference.cmp(&y.reference)),
        _ => rank(a).cmp(&rank(b)).then_with(|| {
            if rank(a) == 0 || rank(a) == 1 {
                Ordering::Equal
            } else {
                key_of(a, c).cmp(&key_of(b, c))
            }
        }),
    }
}

pub fn eq3(a: &QValue, b: &QValue, c: &dyn Collation) -> Option<bool> {
    if matches!(a, QValue::Null) || matches!(b, QValue::Null) {
        return None;
    }
    Some(rank(a) == rank(b) && cmp(a, b, c) == Ordering::Equal)
}

pub fn compare(op: CmpOp, a: &QValue, b: &QValue, c: &dyn Collation) -> Option<bool> {
    if matches!(a, QValue::Null) || matches!(b, QValue::Null) {
        return None;
    }
    let o = cmp(a, b, c);
    Some(match op {
        CmpOp::Eq => o == Ordering::Equal,
        CmpOp::Ne => o != Ordering::Equal,
        CmpOp::Lt => o == Ordering::Less,
        CmpOp::Gt => o == Ordering::Greater,
        CmpOp::Le => o != Ordering::Greater,
        CmpOp::Ge => o != Ordering::Less,
    })
}
