use bsl_prim::RoundMode;
use query_ir::{DateQ, ExecError, Other, QStr, QType, QValue, RuntimeError};
use query_syntax::{MsgArg, MsgId, QueryError};

use super::{charge_string, str_value};
use crate::ops::MemBudget;
use crate::source::ExecEnv;

fn overflow() -> ExecError {
    ExecError::Runtime(RuntimeError::sql(QueryError::new(
        MsgId::DBERROR_SQLFIELDOVERFLOW,
        None,
    )))
}

fn cut_string(s: &QStr, len: u32, mem: &MemBudget<'_>) -> Result<QValue, ExecError> {
    let n = len as usize;
    if len == 0 || s.0.len() <= n {
        return Ok(QValue::String(s.clone()));
    }
    charge_string(u64::from(len), mem)?;
    let mut u = s.0.get(..n).unwrap_or(&[]).to_vec();
    if u.last().is_some_and(|&x| (0xD800..0xDC00).contains(&x)) {
        u.pop();
    }
    Ok(str_value(u))
}

pub fn cast(
    v: &QValue,
    to: &QType,
    ref_tables: &[u32],
    _env: &ExecEnv<'_>,
    mem: &MemBudget<'_>,
) -> Result<QValue, ExecError> {
    Ok(match v {
        QValue::Null => QValue::Null,
        QValue::Number(n) => match to.number {
            None => QValue::Null,
            Some(q) if q.digits == 0 => v.clone(),
            Some(q) => {
                let r = n.round(i64::from(q.scale), RoundMode::HalfAwayFromZero);
                let int = r.int_digits();
                let significant = int.trim_start_matches('0').len();
                if significant > usize::from(q.digits.saturating_sub(q.scale)) {
                    return Err(overflow());
                }
                QValue::Number(r)
            }
        },
        QValue::String(s) => match to.string {
            None => QValue::Null,
            Some(q) => cut_string(s, q.len, mem)?,
        },
        QValue::Bool(_) if to.boolean => v.clone(),
        QValue::Undefined if to.is_undefined() => v.clone(),
        QValue::Date(d) => match to.date {
            None => QValue::Null,
            Some(DateQ::DateTime) => v.clone(),
            Some(DateQ::Date) => QValue::Date(
                bsl_prim::BslDate::from_ticks(
                    d.ticks() - d.ticks() % (86_400 * bsl_prim::BslDate::TICKS_PER_SECOND),
                )
                .unwrap_or(*d),
            ),
            Some(DateQ::Time) => {
                let day = 86_400 * bsl_prim::BslDate::TICKS_PER_SECOND;
                QValue::Date(bsl_prim::BslDate::from_ticks(d.ticks() % day).unwrap_or(*d))
            }
        },
        QValue::Ref { table, .. } if ref_tables.contains(table) => v.clone(),
        QValue::Uuid(_) if to.others.contains(&Other::Uuid) => v.clone(),
        QValue::Binary(_) if to.others.contains(&Other::Binary) => v.clone(),
        QValue::Storage(_) if to.others.contains(&Other::Storage) => v.clone(),
        QValue::Type(_) if to.others.contains(&Other::Type) => v.clone(),
        QValue::SysEnum(t, _) if to.others.contains(&Other::SysEnum(*t)) => v.clone(),
        _ => QValue::Null,
    })
}

fn load_overflow() -> ExecError {
    ExecError::Runtime(RuntimeError {
        chain: vec![
            QueryError::new(MsgId::SELECT_ERROR_ARITHMETICERROR, None)
                .with_quoted(MsgArg::Msg(MsgId::SELECT_OVERFLOW)),
        ],
    })
}

pub fn load_cast(
    v: &QValue,
    to: &QType,
    ref_tables: &[u32],
    env: &ExecEnv<'_>,
) -> Result<QValue, ExecError> {
    load_cast_with_budget(v, to, ref_tables, env, &MemBudget::new(env.mem_limit))
}

pub(crate) fn load_cast_with_budget(
    v: &QValue,
    to: &QType,
    ref_tables: &[u32],
    env: &ExecEnv<'_>,
    mem: &MemBudget<'_>,
) -> Result<QValue, ExecError> {
    match (v, to.string, to.number) {
        (QValue::String(s), Some(q), _) if q.len > 0 => {
            charge_string(u64::from(q.len), mem)?;
            let len = usize::try_from(q.len).map_err(|_| memory_limit())?;
            let mut u = Vec::new();
            u.try_reserve_exact(len).map_err(|_| memory_limit())?;
            let prefix = s.0.get(..s.0.len().min(len)).unwrap_or(&[]);
            u.extend_from_slice(prefix);
            if s.0.len() > len && u.last().is_some_and(|&x| (0xD800..0xDC00).contains(&x)) {
                u.pop();
            }
            u.resize(len, 0x20);
            Ok(str_value(u))
        }
        (QValue::Number(n), _, Some(q)) if q.digits > 0 => {
            let r = n.round(i64::from(q.scale), RoundMode::HalfAwayFromZero);
            let significant = r.int_digits().trim_start_matches('0').len();
            if significant > usize::from(q.digits.saturating_sub(q.scale)) {
                return Err(load_overflow());
            }
            Ok(QValue::Number(r))
        }
        _ => cast(v, to, ref_tables, env, mem),
    }
}

fn memory_limit() -> ExecError {
    ExecError::Fatal("превышен лимит памяти запроса".into())
}
