use bsl_prim::Number;
use query_ir::{ExecError, QType, QValue, RuntimeError};
use query_syntax::{Func, MsgId, QueryError};

use super::scale::{guard, overflow};
use crate::source::ExecEnv;

fn one_cause(id: MsgId) -> ExecError {
    ExecError::Runtime(RuntimeError {
        chain: vec![QueryError::new(id, None)],
    })
}

fn num_arg(args: &[QValue], i: usize) -> Result<&Number, ExecError> {
    match args.get(i) {
        Some(QValue::Number(n)) => Ok(n),
        _ => Err(ExecError::Fatal(
            "num: a math function takes numbers".to_string(),
        )),
    }
}

pub fn math(
    f: Func,
    args: &[QValue],
    ty: &QType,
    constant: bool,
    env: &ExecEnv<'_>,
) -> Result<QValue, ExecError> {
    if args.iter().any(|a| matches!(a, QValue::Null)) {
        return Ok(QValue::Null);
    }
    let x = num_arg(args, 0)?;
    guard(env, x)?;
    let xf = x.to_f64();
    let v = match f {
        Func::Sqrt => {
            if xf < 0.0 {
                return Err(one_cause(MsgId::SELECT_ERROR_NEGATIVESQRT));
            }
            xf.sqrt()
        }
        Func::Log | Func::Log10 => {
            if xf <= 0.0 {
                return Err(one_cause(MsgId::SELECT_ERROR_LOGARITHMICERROR));
            }
            if f == Func::Log { xf.ln() } else { xf.log10() }
        }
        Func::Exp => xf.exp(),
        Func::Sin => xf.sin(),
        Func::Cos => xf.cos(),
        Func::ACos => xf.acos(),
        Func::ASin => xf.asin(),
        Func::ATan => xf.atan(),
        Func::Pow => {
            let y = num_arg(args, 1)?;
            guard(env, y)?;
            xf.powf(y.to_f64())
        }
        other => {
            return Err(ExecError::Fatal(format!(
                "num: {other:?} is not a math function"
            )));
        }
    };
    if !v.is_finite() {
        return Err(overflow());
    }
    let n = match ty.number {
        Some(q) if !constant && q.digits > 0 => {
            let text = format!("{:.*}", usize::from(q.scale), v);
            let n = Number::parse(&text).ok_or_else(overflow)?;
            let int = n.int_digits();
            if int.trim_start_matches('0').len() > usize::from(q.digits.saturating_sub(q.scale)) {
                return Err(overflow());
            }
            n
        }
        _ => Number::from_f64_15(v).ok_or_else(overflow)?,
    };
    Ok(QValue::Number(n))
}
