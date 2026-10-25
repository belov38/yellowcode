use bsl_prim::Number;
use query_syntax::ast::BinOp;

use crate::ir::{Expr, ExprKind};
use crate::types::QType;

pub const MAX_DIGITS: u8 = 38;
pub const MIN_DIV_SCALE: u8 = 6;
pub const MIN_MUL_SCALE: u8 = 9;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Dec {
    pub p: u8,
    pub s: u8,
}

impl Dec {
    pub const UNKNOWN: Dec = Dec { p: 0, s: 0 };

    pub fn is_known(self) -> bool {
        self.p != 0
    }

    pub fn int(self) -> u8 {
        self.p.saturating_sub(self.s)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Opd {
    Const,
    Cast,
    Value,
    Aggregate,
}

impl Opd {
    pub fn join(self, other: Opd) -> Opd {
        match (self, other) {
            (Opd::Cast, _) | (_, Opd::Cast) => Opd::Cast,
            (Opd::Value, _) | (_, Opd::Value) => Opd::Value,
            (Opd::Aggregate, _) | (_, Opd::Aggregate) => Opd::Aggregate,
            _ => Opd::Const,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArithDec {
    pub rhs: Dec,
    pub out: Dec,
    pub lk: Opd,
    pub rk: Opd,
}

pub fn dec_of(ty: &QType, lit: Option<&Number>) -> Dec {
    if let Some(n) = lit {
        return dec_of_lit(n);
    }
    match ty.number {
        Some(q) if q.digits > 0 && ty.atom_count() == 1 => Dec {
            p: q.digits.min(MAX_DIGITS),
            s: q.scale.min(q.digits).min(MAX_DIGITS),
        },
        _ => Dec::UNKNOWN,
    }
}

pub fn dec_of_lit(n: &Number) -> Dec {
    let int = n.int_digits();
    let int = int.trim_start_matches('0').len();
    let s = n.frac_digits().len();
    let p = (int + s).max(1);
    match (u8::try_from(p), u8::try_from(s)) {
        (Ok(p), Ok(s)) if p <= MAX_DIGITS => Dec { p, s },
        _ => Dec::UNKNOWN,
    }
}

fn cap(p: i32, s: i32) -> Dec {
    let p = p.clamp(1, i32::from(MAX_DIGITS));
    let s = s.clamp(0, p);
    Dec {
        p: p as u8,
        s: s as u8,
    }
}

pub fn dec_add(a: Dec, b: Dec) -> Dec {
    let s = i32::from(a.s.max(b.s));
    let int = i32::from(a.int().max(b.int())) + 1;
    cap(int + s, s)
}

pub fn dec_mul(a: Dec, b: Dec) -> Dec {
    dec_mul_floor(a, b, MIN_MUL_SCALE)
}

pub fn mul_floor(l: Opd, r: Opd) -> u8 {
    if l == Opd::Cast || r == Opd::Cast {
        MIN_DIV_SCALE
    } else {
        MIN_MUL_SCALE
    }
}

pub fn dec_mul_floor(a: Dec, b: Dec, floor: u8) -> Dec {
    let p = i32::from(a.p) + i32::from(b.p);
    let mut s = i32::from(a.s) + i32::from(b.s);
    if p > i32::from(MAX_DIGITS) {
        s = (i32::from(MAX_DIGITS) - (p - s)).max(s.min(i32::from(floor)));
    }
    cap(p, s)
}

pub fn dec_div(a: Dec, b: Dec) -> Dec {
    let mut s = i32::from(MIN_DIV_SCALE).max(i32::from(a.s) + i32::from(b.p) + 1);
    let int = i32::from(a.int()) + i32::from(b.s);
    if int + s > i32::from(MAX_DIGITS) {
        s = (i32::from(MAX_DIGITS) - int).max(i32::from(MIN_DIV_SCALE));
    }
    cap(int + s, s)
}

pub fn dec_op(op: BinOp, a: Dec, b: Dec) -> Dec {
    dec_op_k(op, a, Opd::Value, b, Opd::Value)
}

pub fn dec_op_k(op: BinOp, a: Dec, lk: Opd, b: Dec, rk: Opd) -> Dec {
    if !a.is_known() || !b.is_known() {
        return Dec::UNKNOWN;
    }
    match op {
        BinOp::Add | BinOp::Sub => dec_add(a, b),
        BinOp::Mul => dec_mul_floor(a, b, mul_floor(lk, rk)),
        BinOp::Div => dec_div(a, b),
        BinOp::And | BinOp::Or => Dec::UNKNOWN,
    }
}

pub fn agg_dec(f: query_syntax::Func, arg: Dec) -> Dec {
    use query_syntax::Func;
    match f {
        Func::Count => Dec {
            p: MAX_DIGITS,
            s: 0,
        },
        Func::Sum if arg.is_known() => Dec {
            p: MAX_DIGITS,
            s: arg.s,
        },
        Func::Avg if arg.is_known() => Dec {
            p: MAX_DIGITS,
            s: arg.s.max(MIN_DIV_SCALE),
        },
        Func::Min | Func::Max => arg,
        _ => Dec::UNKNOWN,
    }
}

pub fn opd_of(e: &Expr) -> Opd {
    match &e.kind {
        ExprKind::Lit(_) | ExprKind::Param(_) | ExprKind::Value(_) | ExprKind::Unit(_) => {
            Opd::Const
        }
        ExprKind::Cast { .. } => Opd::Cast,
        ExprKind::AggRef(_) => Opd::Aggregate,
        ExprKind::Neg(x) => opd_of(x),
        ExprKind::Arith { first, steps } => steps
            .iter()
            .fold(opd_of(first), |acc, (_, x, _)| acc.join(opd_of(x))),
        ExprKind::Case { whens, else_, .. } => whens
            .iter()
            .map(|(_, t)| opd_of(t))
            .chain(else_.iter().map(|x| opd_of(x)))
            .fold(Opd::Const, Opd::join),
        ExprKind::Func { args, .. } => {
            if args.iter().all(|x| opd_of(x) == Opd::Const) {
                Opd::Const
            } else {
                Opd::Value
            }
        }
        _ => Opd::Value,
    }
}
