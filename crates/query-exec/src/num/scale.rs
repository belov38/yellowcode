use bsl_prim::{Number, RoundMode};
use query_ir::{Dec, ExecError, QStr, QValue, RuntimeError};
use query_syntax::ast::BinOp;
use query_syntax::{MsgId, QueryError};

use crate::eval::charge_string;
use crate::model::value_model;
use crate::ops::MemBudget;
use crate::source::ExecEnv;

use query_ir::lower::{Opd, dec_mul_floor, mul_floor};
pub use query_ir::lower::{dec_add, dec_div, dec_mul, dec_of, dec_of_lit};

const HALF: RoundMode = RoundMode::HalfAwayFromZero;

fn limit_error() -> ExecError {
    ExecError::Fatal("превышен лимит размера числа".to_string())
}

pub fn guard(env: &ExecEnv<'_>, n: &Number) -> Result<(), ExecError> {
    if n.size_hint_limbs() > env.max_number_limbs {
        Err(limit_error())
    } else {
        Ok(())
    }
}

pub fn cant_fit() -> ExecError {
    ExecError::Runtime(RuntimeError::sql(QueryError::new(
        MsgId::DBERROR_FIELDVALUECANTFIT,
        None,
    )))
}

pub fn cant_fit_as_overflow(e: ExecError) -> ExecError {
    match &e {
        ExecError::Runtime(r)
            if r.chain
                .last()
                .is_some_and(|c| c.msg == MsgId::DBERROR_FIELDVALUECANTFIT) =>
        {
            overflow()
        }
        _ => e,
    }
}

pub fn overflow() -> ExecError {
    ExecError::Runtime(RuntimeError::sql(QueryError::new(
        MsgId::DBERROR_SQLFIELDOVERFLOW,
        None,
    )))
}

pub fn div_by_zero() -> ExecError {
    ExecError::Runtime(
        RuntimeError::sql(QueryError::new(MsgId::DBERROR_SQLDIVISIONBYZERO, None))
            .with_tail(QueryError::new(MsgId::NUMERROR_DIVIDEBYZERO, None)),
    )
}

pub fn fits(v: &Number, d: Dec) -> bool {
    let int = v.int_digits();
    int.trim_start_matches('0').len() <= usize::from(d.int())
}

fn internal(what: &str) -> ExecError {
    ExecError::Fatal(format!("num: {what}"))
}

pub fn add(
    a: &Number,
    da: Dec,
    b: &Number,
    db: Dec,
    sub: bool,
) -> Result<(Number, Dec), ExecError> {
    let v = if sub { a.sub(b) } else { a.add(b) };
    if !da.is_known() || !db.is_known() {
        return Ok((v, Dec::UNKNOWN));
    }
    let out = dec_add(da, db);
    let v = v.round(i64::from(out.s), HALF);
    if !fits(&v, out) {
        return Err(cant_fit());
    }
    Ok((v, out))
}

fn int_len(v: &Number) -> usize {
    v.int_digits().trim_start_matches('0').len()
}

fn const_digits(v: &Number, col_scale: u8) -> usize {
    let frac = v.frac_digits();
    let frac = frac.trim_end_matches('0').len();
    int_len(v) + frac.saturating_sub(usize::from(col_scale))
}

fn const_overflows(a: &Number, da: Dec, aggregate: bool, b: &Number, out: Dec) -> bool {
    let limit = if da.s <= 8 || aggregate {
        37_usize.saturating_sub(usize::from(da.s))
    } else {
        38 - usize::from(out.s)
    };
    int_len(a) + const_digits(b, da.s) > limit
}

fn columns_overflow(a: &Number, da: Dec, b: &Number, db: Dec, out: Dec) -> bool {
    let (q1, q2) = (usize::from(da.int()), usize::from(db.int()));
    let s = usize::from(da.s) + usize::from(db.s);
    let total = if s <= 8 {
        37 - s
    } else {
        38 - usize::from(out.s)
    };
    if q1 + q2 <= total {
        return false;
    }
    let cap2 = (total * q2).div_ceil(q1 + q2);
    let cap1 = total - cap2;
    int_len(a) > cap1 || int_len(b) > cap2
}

pub fn mul(
    a: &Number,
    da: Dec,
    b: &Number,
    db: Dec,
    env: &ExecEnv<'_>,
) -> Result<(Number, Dec), ExecError> {
    mul_k(a, da, Opd::Value, b, db, Opd::Value, env)
}

pub fn mul_k(
    a: &Number,
    da: Dec,
    lk: Opd,
    b: &Number,
    db: Dec,
    rk: Opd,
    env: &ExecEnv<'_>,
) -> Result<(Number, Dec), ExecError> {
    guard(env, a)?;
    guard(env, b)?;
    let v = a.mul(b);
    let db = if rk == Opd::Const && !db.is_known() {
        dec_of_lit(b)
    } else {
        db
    };
    if !da.is_known() || !db.is_known() {
        guard(env, &v)?;
        return Ok((v, Dec::UNKNOWN));
    }
    let rk = if rk == Opd::Const && const_digits(b, 0) >= 37 {
        Opd::Cast
    } else {
        rk
    };
    let out = dec_mul_floor(da, db, mul_floor(lk, rk));
    let v = v.round(i64::from(out.s), HALF);
    match (lk, rk) {
        (Opd::Const, _) => legacy_operand_check(a, b, out)?,
        (_, Opd::Const) => {
            if const_overflows(a, da, lk == Opd::Aggregate, b, out) {
                return Err(overflow());
            }
        }
        (_, Opd::Cast) => {
            return if fits(&v, out) {
                Ok((v, out))
            } else {
                Err(cant_fit())
            };
        }
        (_, Opd::Value | Opd::Aggregate) => {
            if columns_overflow(a, da, b, db, out) {
                return Err(overflow());
            }
        }
    }
    if !fits(&v, out) {
        return Err(overflow());
    }
    Ok((v, out))
}

fn legacy_operand_check(a: &Number, b: &Number, out: Dec) -> Result<(), ExecError> {
    let operand_room = Dec { p: 38, s: out.s };
    if !fits(a, operand_room) || !fits(b, operand_room) {
        return Err(overflow());
    }
    Ok(())
}

pub(crate) fn div_round(
    a: &Number,
    b: &Number,
    s: u8,
    env: &ExecEnv<'_>,
) -> Result<Number, ExecError> {
    let q = a.div(b).map_err(|_| div_by_zero())?;
    guard(env, &q)?;
    Ok(q.round(i64::from(s), HALF))
}

pub fn div(
    a: &Number,
    da: Dec,
    b: &Number,
    db: Dec,
    env: &ExecEnv<'_>,
) -> Result<(Number, Dec), ExecError> {
    guard(env, a)?;
    guard(env, b)?;
    if b.is_zero() {
        return Err(div_by_zero());
    }
    if !da.is_known() || !db.is_known() {
        let v = a.div(b).map_err(|_| div_by_zero())?;
        guard(env, &v)?;
        return Ok((v, Dec::UNKNOWN));
    }
    let out = dec_div(da, db);
    let v = div_round(a, b, out.s, env)?;
    if !fits(&v, out) {
        return Err(cant_fit());
    }
    Ok((v, out))
}

pub fn round_to(v: &Number, d: Dec, env: &ExecEnv<'_>) -> Result<Number, ExecError> {
    guard(env, v)?;
    if !d.is_known() {
        return Ok(v.clone());
    }
    let r = v.round(i64::from(d.s), HALF);
    if fits(&r, d) { Ok(r) } else { Err(overflow()) }
}

pub fn round_field(v: &Number, n: i64, d: Dec, env: &ExecEnv<'_>) -> Result<Number, ExecError> {
    guard(env, v)?;
    let r = v.round(n, HALF);
    if !d.is_known() || fits(&r, d) {
        Ok(r)
    } else {
        Err(overflow())
    }
}

pub fn trunc_field(v: &Number, env: &ExecEnv<'_>) -> Result<Number, ExecError> {
    guard(env, v)?;
    Ok(v.trunc())
}

pub fn arith_step(
    op: BinOp,
    l: &QValue,
    dl: Dec,
    r: &QValue,
    dr: Dec,
    env: &ExecEnv<'_>,
    mem: &MemBudget<'_>,
) -> Result<(QValue, Dec), ExecError> {
    arith_step_k(op, l, dl, Opd::Value, r, dr, Opd::Value, env, mem)
}

#[allow(clippy::too_many_arguments)]
pub fn arith_step_k(
    op: BinOp,
    l: &QValue,
    dl: Dec,
    lk: Opd,
    r: &QValue,
    dr: Dec,
    rk: Opd,
    env: &ExecEnv<'_>,
    mem: &MemBudget<'_>,
) -> Result<(QValue, Dec), ExecError> {
    let empty = |v: &QValue| matches!(v, QValue::Null | QValue::Undefined);
    if empty(l) || empty(r) {
        return Ok((QValue::Null, Dec::UNKNOWN));
    }
    match (l, r) {
        (QValue::Number(a), QValue::Number(b)) => {
            let (v, d) = match op {
                BinOp::Add => add(a, dl, b, dr, false)?,
                BinOp::Sub => add(a, dl, b, dr, true)?,
                BinOp::Mul => mul_k(a, dl, lk, b, dr, rk, env)?,
                BinOp::Div => div(a, dl, b, dr, env)?,
                BinOp::And | BinOp::Or => return Err(internal("a logical operator in a chain")),
            };
            Ok((QValue::Number(v), d))
        }
        (QValue::String(a), QValue::String(b)) if op == BinOp::Add => {
            charge_string(a.0.len() as u64 + b.0.len() as u64, mem)?;
            let mut u = Vec::with_capacity(a.0.len() + b.0.len());
            u.extend_from_slice(&a.0);
            u.extend_from_slice(&b.0);
            Ok((QValue::String(QStr(u.into())), Dec::UNKNOWN))
        }
        _ => Err(internal(
            "operands of an arithmetic operator have no common type",
        )),
    }
}

pub fn arith_chain(
    first: &QValue,
    first_dec: Dec,
    steps: &[(BinOp, QValue, Dec)],
    env: &ExecEnv<'_>,
    mem: &MemBudget<'_>,
) -> Result<(QValue, Dec), ExecError> {
    let mut acc = (first.clone(), first_dec);
    let mut acc_scratch = 0u64;
    for (op, rhs, dr) in steps {
        let next = arith_step(*op, &acc.0, acc.1, rhs, *dr, env, mem)?;
        mem.shrink_scratch(acc_scratch);
        acc_scratch = match &next.0 {
            QValue::String(_) => value_model(&next.0),
            _ => 0,
        };
        acc = next;
    }
    Ok(acc)
}

pub fn arith_chain_k(
    first: &QValue,
    first_dec: Dec,
    first_kind: Opd,
    steps: &[(BinOp, QValue, Dec, Opd)],
    exact_div: bool,
    env: &ExecEnv<'_>,
    mem: &MemBudget<'_>,
) -> Result<(QValue, Dec), ExecError> {
    let mut acc = (first.clone(), first_dec);
    let mut lk = first_kind;
    let mut acc_scratch = 0u64;
    for (op, rhs, dr, rk) in steps {
        let next = match (op, &acc.0, rhs) {
            (BinOp::Div, QValue::Number(a), QValue::Number(b)) if exact_div => {
                guard(env, a)?;
                guard(env, b)?;
                let q = a.div(b).map_err(|_| div_by_zero())?;
                guard(env, &q)?;
                (QValue::Number(q), Dec::UNKNOWN)
            }
            _ => arith_step_k(*op, &acc.0, acc.1, lk, rhs, *dr, *rk, env, mem)?,
        };
        mem.shrink_scratch(acc_scratch);
        acc_scratch = match &next.0 {
            QValue::String(_) => value_model(&next.0),
            _ => 0,
        };
        acc = next;
        lk = lk.join(*rk);
    }
    Ok(acc)
}
