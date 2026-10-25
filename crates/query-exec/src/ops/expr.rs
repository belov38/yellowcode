use query_ir::{Dec, ExecError, PExpr, QType, QValue, SizeKind};
use query_syntax::Func;
use query_syntax::ast::{BinOp, CmpOp};

use super::{Cx, fatal};
use crate::eval::{
    and3, case_truth, cast, compare, eq3, func, like, like_cost, load_cast_with_budget, not3, rank,
    to_value, truth, type_of_value, unit_arg,
};
use crate::model::{key_sizes_charged, walk_bytes};
use crate::num::{
    ArithDec, Opd, arith_chain_k, cant_fit_as_overflow, math, round_field, trunc_field,
};

type R<T> = Result<T, ExecError>;

pub fn eval<'a>(e: &PExpr, row: &[QValue], cx: &Cx<'a>) -> R<QValue> {
    let env = cx.env;
    let coll = env.collation;
    Ok(match e {
        PExpr::Col(i) => row
            .get(usize::from(*i))
            .cloned()
            .ok_or_else(|| fatal("column out of range"))?,
        PExpr::Outer { depth, col } => cx
            .outer
            .len()
            .checked_sub(usize::from(*depth))
            .and_then(|i| cx.outer.get(i))
            .and_then(|r| r.get(usize::from(*col)))
            .cloned()
            .ok_or_else(|| fatal("outer column out of range"))?,
        PExpr::Lit(v) => v.clone(),
        PExpr::Set(_) => return Err(fatal("a value set outside В (..)")),
        PExpr::Unit(k) => unit_arg(*k),
        PExpr::Neg(x) => match eval(x, row, cx)? {
            QValue::Number(n) => QValue::Number(n.neg()),
            QValue::Null => QValue::Null,
            _ => return Err(fatal("negation of a value that is not a number")),
        },
        PExpr::Not(x) => to_value(not3(truth(&eval(x, row, cx)?))),
        PExpr::Arith {
            first,
            first_dec,
            steps,
            decs,
        } => arith(first, *first_dec, steps, decs, false, row, cx)?,
        PExpr::And(xs) => {
            let mut unknown = false;
            for x in xs {
                match truth(&eval(x, row, cx)?) {
                    Some(false) => return Ok(QValue::Bool(false)),
                    None => unknown = true,
                    Some(true) => {}
                }
            }
            if unknown {
                QValue::Null
            } else {
                QValue::Bool(true)
            }
        }
        PExpr::Or(xs) => {
            let mut unknown = false;
            for x in xs {
                match truth(&eval(x, row, cx)?) {
                    Some(true) => return Ok(QValue::Bool(true)),
                    None => unknown = true,
                    Some(false) => {}
                }
            }
            if unknown {
                QValue::Null
            } else {
                QValue::Bool(false)
            }
        }
        PExpr::Compare(op, a, b) => {
            let (a, b) = (eval(a, row, cx)?, eval(b, row, cx)?);
            to_value(keys_charged(&a, &b, cx, || compare(*op, &a, &b, coll))?)
        }
        PExpr::InQuery {
            l,
            not,
            q,
            correlated,
        } => {
            cx.mem.grow_scratch(crate::model::vec_block(
                l.len(),
                std::mem::size_of::<crate::key::Key>(),
            ))?;
            let mut lv = Vec::with_capacity(l.len());
            for x in l {
                let v = eval(x, row, cx)?;
                lv.push(super::join::key_charged(&v, cx)?);
            }
            let found = super::subq::in_query(&lv, q, *correlated, row, cx)?;
            to_value(if *not { not3(found) } else { found })
        }
        PExpr::InList { l, not, rhs } => {
            cx.mem
                .grow_scratch(crate::model::vec_block(l.len(), crate::model::VALUE_SIZE))?;
            let lv = l
                .iter()
                .map(|x| eval(x, row, cx))
                .collect::<R<Vec<QValue>>>()?;
            let r = in_list(&lv, rhs, row, cx)?;
            to_value(if *not { not3(r) } else { r })
        }
        PExpr::Between { l, not, lo, hi } => {
            let (l, lo, hi) = (eval(l, row, cx)?, eval(lo, row, cx)?, eval(hi, row, cx)?);
            let r = and3([
                keys_charged(&l, &lo, cx, || compare(CmpOp::Ge, &l, &lo, coll))?,
                keys_charged(&l, &hi, cx, || compare(CmpOp::Le, &l, &hi, coll))?,
            ]);
            to_value(if *not { not3(r) } else { r })
        }
        PExpr::IsNull { l, not } => {
            let n = matches!(eval(l, row, cx)?, QValue::Null);
            QValue::Bool(n != *not)
        }
        PExpr::Refs { l, number } => match eval(l, row, cx)? {
            QValue::Null => QValue::Null,
            QValue::Ref { table, .. } => QValue::Bool(table == *number),
            _ => QValue::Bool(false),
        },
        PExpr::Like { l, not, pat, esc } => {
            let (l, p) = (eval(l, row, cx)?, eval(pat, row, cx)?);
            let n = like_cost(&l, &p, *esc, &mut |m| cx.mem.grow_scratch(m))?;
            let r = like(&l, &p, *esc, coll);
            cx.mem.shrink_scratch(n);
            to_value(if *not { not3(r) } else { r })
        }
        PExpr::Case {
            operand,
            whens,
            else_,
        } => {
            let op = match operand {
                Some(o) => Some(eval(o, row, cx)?),
                None => None,
            };
            for (w, t) in whens {
                let wv = eval(w, row, cx)?;
                let hit = match &op {
                    Some(o) => keys_charged(o, &wv, cx, || eq3(o, &wv, coll))?,
                    None => Some(case_truth(&wv)),
                };
                if hit == Some(true) {
                    return eval(t, row, cx);
                }
            }
            match else_ {
                Some(x) => eval(x, row, cx)?,
                None => QValue::Null,
            }
        }
        PExpr::Cast {
            x,
            to,
            num,
            ref_tables,
            load,
        } => {
            let v = match &**x {
                PExpr::Arith {
                    first,
                    first_dec,
                    steps,
                    decs,
                } if num.is_some() => arith(first, *first_dec, steps, decs, true, row, cx)?,
                other => eval(other, row, cx)?,
            };
            let t = match num {
                Some(q) => {
                    let mut t = to.clone();
                    t.number = Some(*q);
                    std::borrow::Cow::Owned(t)
                }
                None => std::borrow::Cow::Borrowed(to),
            };
            if *load {
                load_cast_with_budget(&v, &t, ref_tables, env, &cx.mem)?
            } else {
                cast(&v, &t, ref_tables, env, &cx.mem)?
            }
        }
        PExpr::SectionAgg { owner, rel, empty } => {
            let o = eval(owner, row, cx)?;
            super::subq::section_value(&o, rel, empty, cx)?
        }
        PExpr::TypeOf { x, refs } => {
            let v = eval(x, row, cx)?;
            type_of_value(&v, refs)?
        }
        PExpr::StoredSize { x, kind } => {
            let v = eval(x, row, cx)?;
            let chars = |v: &QValue| match v {
                QValue::String(s) => s.len_utf16() as i64,
                _ => 0,
            };
            let n = match kind {
                SizeKind::Fixed(n) => i64::from(*n),
                SizeKind::VarString => 2 * chars(&v),
                SizeKind::Composite => 70 + 2 * chars(&v),
            };
            QValue::Number(bsl_prim::Number::from_i64(n))
        }
        PExpr::Func {
            f,
            args,
            ty,
            arg_dec,
        } => call(*f, args, ty, *arg_dec, row, cx)?,
    })
}

fn keys_charged<T>(a: &QValue, b: &QValue, cx: &Cx<'_>, f: impl FnOnce() -> T) -> R<T> {
    let r = rank(a);
    if r != rank(b) || r <= 4 {
        return Ok(f());
    }
    let coll = cx.env.collation;
    let walks = walk_bytes(a) + walk_bytes(b);
    let (kept_a, peak_a) = key_sizes_charged(a, coll, &mut |m| cx.mem.grow_scratch(m))?;
    let (_, peak_b) = key_sizes_charged(b, coll, &mut |m| cx.mem.grow_scratch(m))?;
    let n = peak_a.max(kept_a + peak_b);
    cx.mem.grow_scratch(n - walks)?;
    let out = f();
    cx.mem.shrink_scratch(n);
    Ok(out)
}

fn in_list(l: &[QValue], rhs: &[PExpr], row: &[QValue], cx: &Cx<'_>) -> R<Option<bool>> {
    let coll = cx.env.collation;
    let k = l.len().max(1);
    let tuple_eq = |t: &[QValue]| -> R<Option<bool>> {
        let mut parts = Vec::with_capacity(l.len());
        for (a, b) in l.iter().zip(t) {
            parts.push(keys_charged(a, b, cx, || eq3(a, b, coll))?);
        }
        Ok(and3(parts))
    };
    let mut res = Some(false);
    if let [PExpr::Set(s)] = rhs {
        if k != 1 {
            return Err(fatal("a value set against a tuple"));
        }
        if let [QValue::Null] = s.values.as_slice() {
            return Ok(res);
        }
        for v in &s.values {
            res = merge(res, tuple_eq(std::slice::from_ref(v))?);
            if res == Some(true) {
                break;
            }
        }
        return Ok(res);
    }
    if !rhs.len().is_multiple_of(k) {
        return Err(fatal("В (..) list is not a multiple of the tuple"));
    }
    if let ([x], 1) = (rhs, k)
        && !matches!(x, PExpr::Set(_))
    {
        let v = eval(x, row, cx)?;
        if matches!(v, QValue::Null) {
            return Ok(res);
        }
        return Ok(merge(res, tuple_eq(std::slice::from_ref(&v))?));
    }
    for chunk in rhs.chunks(k) {
        let mut t = Vec::with_capacity(k);
        for x in chunk {
            if let PExpr::Set(s) = x {
                if k != 1 {
                    return Err(fatal("a value set against a tuple"));
                }
                for v in &s.values {
                    res = merge(res, tuple_eq(std::slice::from_ref(v))?);
                    if res == Some(true) {
                        return Ok(res);
                    }
                }
                t.clear();
                break;
            }
            t.push(eval(x, row, cx)?);
        }
        if t.len() == k {
            res = merge(res, tuple_eq(&t)?);
            if res == Some(true) {
                break;
            }
        }
    }
    Ok(res)
}

fn merge(res: Option<bool>, r: Option<bool>) -> Option<bool> {
    match r {
        Some(true) => Some(true),
        None if res == Some(false) => None,
        _ => res,
    }
}

fn arith(
    first: &PExpr,
    first_dec: Dec,
    steps: &[(BinOp, PExpr, QType)],
    decs: &[ArithDec],
    exact_div: bool,
    row: &[QValue],
    cx: &Cx<'_>,
) -> R<QValue> {
    let f = eval(first, row, cx)?;
    cx.mem.grow_scratch(crate::model::vec_block(
        steps.len(),
        std::mem::size_of::<(BinOp, QValue, Dec, Opd)>(),
    ))?;
    let mut st = Vec::with_capacity(steps.len());
    for (i, (op, x, _)) in steps.iter().enumerate() {
        let (d, k) = decs
            .get(i)
            .map_or((Dec::UNKNOWN, Opd::Value), |d| (d.rhs, d.rk));
        st.push((*op, eval(x, row, cx)?, d, k));
    }
    let first_kind = decs.first().map_or(Opd::Value, |d| d.lk);
    Ok(arith_chain_k(&f, first_dec, first_kind, &st, exact_div, cx.env, &cx.mem)?.0)
}

fn call(
    f: Func,
    args: &[PExpr],
    ty: &QType,
    arg_dec: Dec,
    row: &[QValue],
    cx: &Cx<'_>,
) -> R<QValue> {
    let env = cx.env;
    cx.mem.grow_scratch(crate::model::vec_block(
        args.len(),
        crate::model::VALUE_SIZE,
    ))?;
    let a = args
        .iter()
        .map(|x| {
            eval(x, row, cx).map_err(|e| {
                if f == Func::Int {
                    cant_fit_as_overflow(e)
                } else {
                    e
                }
            })
        })
        .collect::<R<Vec<QValue>>>()?;
    match f {
        Func::Sqrt
        | Func::Log
        | Func::Log10
        | Func::Exp
        | Func::Sin
        | Func::Cos
        | Func::ACos
        | Func::ASin
        | Func::ATan
        | Func::Pow => {
            let constant = args.iter().all(|x| matches!(x, PExpr::Lit(_)));
            math(f, &a, ty, constant, env)
        }
        Func::Round => match a.as_slice() {
            [QValue::Null, ..] | [_, QValue::Null] => Ok(QValue::Null),
            [QValue::Number(x)] => Ok(QValue::Number(round_field(x, 0, arg_dec, env)?)),
            [QValue::Number(x), QValue::Number(n)] => Ok(QValue::Number(round_field(
                x,
                n.trunc_to_i64_saturating(),
                arg_dec,
                env,
            )?)),
            _ => Err(fatal("ОКР takes numbers")),
        },
        Func::Int => match a.as_slice() {
            [QValue::Null] => Ok(QValue::Null),
            [QValue::Number(x)] => Ok(QValue::Number(trunc_field(x, env)?)),
            _ => Err(fatal("ЦЕЛ takes a number")),
        },
        Func::Sum | Func::Min | Func::Max | Func::Avg | Func::Count => {
            Err(fatal("an aggregate outside an aggregation"))
        }
        Func::BeginOfPeriod | Func::EndOfPeriod | Func::Week => match cx.week_start {
            Some(ws) => match crate::eval::week_call(f, &a, ws) {
                Some(v) => v,
                None => func(f, &a, ty, env, &cx.mem),
            },
            None if crate::eval::week_call(f, &a, 1).is_some() => Err(fatal(
                "a week is computed without the first day of a week of the base (BindHost::week_start)",
            )),
            None => func(f, &a, ty, env, &cx.mem),
        },
        other => func(other, &a, ty, env, &cx.mem),
    }
}
