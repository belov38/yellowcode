use crate::ir::{Expr, ExprKind};
use std::hash::{Hash, Hasher};

pub(super) fn shallow(e: &Expr, h: &mut impl Hasher) {
    shallow_d(e, h, SHALLOW_DEPTH);
}

const SHALLOW_DEPTH: u8 = 3;

fn shallow_d(e: &Expr, h: &mut impl Hasher, d: u8) {
    std::mem::discriminant(&e.kind).hash(h);
    match &e.kind {
        ExprKind::Col { src, col } => {
            src.hash(h);
            col.hash(h);
        }
        ExprKind::Deref { base, steps } => {
            shallow_d(base, h, d);
            for s in steps {
                s.name.hash(h);
            }
        }
        ExprKind::Lit(v) => value(v, h),
        ExprKind::Param(i) | ExprKind::Value(i) | ExprKind::Out(i) => i.hash(h),
        ExprKind::KeyRef(i) | ExprKind::AggRef(i) => i.hash(h),
        ExprKind::OuterKey { src, key, .. } => {
            src.hash(h);
            key.hash(h);
        }
        ExprKind::Neg(x) | ExprKind::Not(x) => kids([&**x].into_iter(), h, d),
        ExprKind::Arith { first, steps } => {
            for (op, _, _) in steps {
                std::mem::discriminant(op).hash(h);
            }
            kids(
                std::iter::once(&**first).chain(steps.iter().map(|(_, x, _)| x)),
                h,
                d,
            );
        }
        ExprKind::And(v) | ExprKind::Or(v) => kids(v.iter(), h, d),
        ExprKind::Compare(op, a, b) => {
            std::mem::discriminant(op).hash(h);
            kids([&**a, &**b].into_iter(), h, d);
        }
        ExprKind::InList {
            l,
            not,
            hierarchy,
            rhs,
        } => {
            not.hash(h);
            hierarchy.hash(h);
            kids(l.iter().chain(rhs), h, d);
        }
        ExprKind::InQuery {
            l, not, hierarchy, ..
        } => {
            not.hash(h);
            hierarchy.hash(h);
            kids(l.iter(), h, d);
        }
        ExprKind::SectionAgg { f, owner, .. } => {
            f.hash(h);
            kids([&**owner].into_iter(), h, d);
        }
        ExprKind::Between { l, not, lo, hi } => {
            not.hash(h);
            kids([&**l, &**lo, &**hi].into_iter(), h, d);
        }
        ExprKind::IsNull { l, not } => {
            not.hash(h);
            kids([&**l].into_iter(), h, d);
        }
        ExprKind::Refs { l, table } => {
            table.hash(h);
            kids([&**l].into_iter(), h, d);
        }
        ExprKind::Like { l, not, pat, esc } => {
            not.hash(h);
            esc.hash(h);
            kids([&**l, &**pat].into_iter(), h, d);
        }
        ExprKind::Case {
            operand,
            whens,
            else_,
        } => {
            kids(
                operand
                    .iter()
                    .map(|x| &**x)
                    .chain(
                        whens
                            .iter()
                            .flat_map(|(condition, result)| [condition, result]),
                    )
                    .chain(else_.iter().map(|x| &**x)),
                h,
                d,
            );
        }
        ExprKind::Cast { x, to, num } => {
            to.hash(h);
            num.hash(h);
            kids([&**x].into_iter(), h, d);
        }
        ExprKind::Func { f, args } => {
            std::mem::discriminant(f).hash(h);
            kids(args.iter(), h, d);
        }
        ExprKind::Unit(k) => std::mem::discriminant(k).hash(h),
    }
}

fn kids<'e>(values: impl Iterator<Item = &'e Expr> + Clone, h: &mut impl Hasher, d: u8) {
    values.clone().count().hash(h);
    if d > 0 {
        for value in values {
            shallow_d(value, h, d - 1);
        }
    }
}

fn value(value: &crate::QValue, h: &mut impl Hasher) {
    use crate::QValue::*;
    std::mem::discriminant(value).hash(h);
    match value {
        Null | Undefined => {}
        Bool(v) => v.hash(h),
        Number(v) => v.hash(h),
        String(v) => v.hash(h),
        Date(v) => v.hash(h),
        Uuid(v) => v.hash(h),
        Ref { table, id } => {
            table.hash(h);
            id.hash(h);
        }
        Type(v) => v.hash(h),
        Binary(v) | Storage(v) => v.hash(h),
        SysEnum(t, v) => {
            t.hash(h);
            v.hash(h);
        }
        Nested(v) => {
            v.columns.len().hash(h);
            v.rows.len().hash(h);
        }
        PointInTime(m) => {
            m.date.hash(h);
            m.reference.hash(h);
        }
        Boundary(b) => {
            b.include.hash(h);
            self::value(&b.point, h);
        }
    }
}
