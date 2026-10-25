use crate::error::{Exception, HostError};
use crate::object::CallError;
use crate::place::Arg;
use crate::runtime::Execution;
use crate::value::Value;
use bsl_prim::{Number, Str};
use std::cmp::Ordering;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmpOp {
    Lt,
    Le,
    Gt,
    Ge,
}

const CONVERT_2NUMERIC: &str = "IDS_RTE_CONVERT_2NUMERIC";
const CONVERT_2BOOL: &str = "IDS_RTE_CONVERT_2BOOL";
const CMPTYPES: &str = "IDS_RTE_CMPTYPES";
const ZERODIV: &str = "IDS_RTE_ZERODIV";

fn fail(ex: &mut Execution, key: &'static str) -> Exception {
    ex.error(HostError::key(key))
}

fn string_to_number(ex: &mut Execution, s: &Str) -> Result<Number, Exception> {
    if let Some(f) = ex.runtime().global_fn_by_name("Число") {
        let mut args = [Arg::Val(Value::String(s.clone()))];
        return match f(ex, &mut args) {
            Ok(Value::Number(n)) => Ok(n),
            Ok(_) | Err(CallError::Host(_)) => Err(fail(ex, CONVERT_2NUMERIC)),
            Err(CallError::Exc(e)) => Err(e),
        };
    }
    let text = s.to_string_lossy();
    match Number::parse(text.trim()) {
        Some(n) => Ok(n),
        None => Err(fail(ex, CONVERT_2NUMERIC)),
    }
}

pub fn to_number(ex: &mut Execution, v: &Value) -> Result<Number, Exception> {
    match v {
        Value::Number(n) => Ok(n.clone()),
        Value::Bool(b) => Ok(Number::from_i64(i64::from(*b))),
        Value::String(s) => string_to_number(ex, s),
        _ => Err(fail(ex, CONVERT_2NUMERIC)),
    }
}

fn charge_work(ex: &mut Execution, a: &Number, b: &Number) -> Result<(), Exception> {
    let work = a.size_hint_limbs().saturating_mul(b.size_hint_limbs());
    if work > 16 {
        ex.charge(work)?;
    }
    Ok(())
}

fn arith(ex: &mut Execution, op: BinOp, a: &Number, b: &Number) -> Result<Value, Exception> {
    let size = a.size_hint_limbs().saturating_add(b.size_hint_limbs());
    let r = match op {
        BinOp::Add => a.add(b),
        BinOp::Sub => a.sub(b),
        BinOp::Mul => {
            charge_work(ex, a, b)?;
            ex.reserve(size.saturating_mul(4))?;
            a.mul(b)
        }
        BinOp::Div | BinOp::Mod => {
            if b.is_zero() {
                return Err(fail(ex, ZERODIV));
            }
            charge_work(ex, a, b)?;
            ex.reserve(size.saturating_add(2).saturating_mul(4))?;
            let r = if op == BinOp::Div { a.div(b) } else { a.rem(b) };
            match r {
                Ok(n) => n,
                Err(_) => return Err(fail(ex, ZERODIV)),
            }
        }
    };
    Ok(Value::Number(r))
}

pub fn binary(ex: &mut Execution, op: BinOp, a: &Value, b: &Value) -> Result<Value, Exception> {
    match a {
        Value::String(sa) if op == BinOp::Add => {
            let rhs = match b {
                Value::String(s) => s.clone(),
                other => ex.present(other)?,
            };
            let units = (sa.len() as u64).saturating_add(rhs.len() as u64);
            ex.charge(units / 64)?;
            ex.reserve(units.saturating_mul(2))?;
            let mut v = Vec::new();
            if v.try_reserve_exact(sa.len() + rhs.len()).is_err() {
                return Err(ex.out_of_memory());
            }
            v.extend_from_slice(sa.units());
            v.extend_from_slice(rhs.units());
            Ok(Value::String(Str::from_vec(v)))
        }
        Value::Date(d) if matches!(op, BinOp::Add | BinOp::Sub) => {
            if let (BinOp::Sub, Value::Date(e)) = (op, b) {
                return Ok(Value::Number(d.sub_date(*e)));
            }
            if matches!(b, Value::Date(_)) {
                return Err(fail(ex, CONVERT_2NUMERIC));
            }
            let n = to_number(ex, b)?;
            let n = if op == BinOp::Sub { n.neg() } else { n };
            Ok(Value::Date(d.add_seconds(&n)))
        }
        Value::Number(_) | Value::Bool(_) | Value::String(_) => {
            let x = to_number(ex, a)?;
            let y = to_number(ex, b)?;
            arith(ex, op, &x, &y)
        }
        _ => Err(fail(ex, CONVERT_2NUMERIC)),
    }
}

fn charge_strings(ex: &mut Execution, x: &Str, y: &Str) -> Result<(), Exception> {
    let n = x.len().min(y.len());
    if n > 64 {
        ex.charge(n as u64 / 64)?;
    }
    Ok(())
}

pub fn charge_equal(ex: &mut Execution, a: &Value, b: &Value) -> Result<(), Exception> {
    match (a, b) {
        (Value::String(x), Value::String(y)) => charge_strings(ex, x, y),
        _ => Ok(()),
    }
}

pub fn equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Undefined, Value::Undefined) | (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Number(x), Value::Number(y)) => x == y,
        (Value::Bool(x), Value::Number(n)) | (Value::Number(n), Value::Bool(x)) => {
            *n == Number::from_i64(i64::from(*x))
        }
        (Value::String(x), Value::String(y)) => x.units() == y.units(),
        (Value::Date(x), Value::Date(y)) => x == y,
        (Value::Type(x), Value::Type(y)) => x == y,
        (Value::Object(x), Value::Object(y)) => {
            std::rc::Rc::ptr_eq(x, y) || x.equals(&**y).or_else(|| y.equals(&**x)).unwrap_or(false)
        }
        _ => false,
    }
}

fn order_num(v: &Value) -> Option<Number> {
    match v {
        Value::Number(n) => Some(n.clone()),
        Value::Bool(b) => Some(Number::from_i64(i64::from(*b))),
        _ => None,
    }
}

pub fn compare(ex: &mut Execution, op: CmpOp, a: &Value, b: &Value) -> Result<bool, Exception> {
    let ord = match (a, b) {
        (Value::Date(x), Value::Date(y)) => x.cmp(y),
        (Value::String(x), Value::String(y)) => {
            charge_strings(ex, x, y)?;
            let c = ex.env().collation();
            let (xu, yu) = (x.units(), y.units());
            if c.less(xu, yu) {
                Ordering::Less
            } else if c.less(yu, xu) {
                Ordering::Greater
            } else {
                Ordering::Equal
            }
        }
        _ => match (order_num(a), order_num(b)) {
            (Some(x), Some(y)) => x.cmp(&y),
            _ => return Err(fail(ex, CMPTYPES)),
        },
    };
    Ok(match op {
        CmpOp::Lt => ord == Ordering::Less,
        CmpOp::Le => ord != Ordering::Greater,
        CmpOp::Gt => ord == Ordering::Greater,
        CmpOp::Ge => ord != Ordering::Less,
    })
}

fn string_to_bool(units: &[u16]) -> Option<bool> {
    let is_space = |u: &u16| matches!(*u, 0x20 | 0x09 | 0x0A | 0x0D | 0xA0);
    let start = units.iter().position(|u| !is_space(u))?;
    let end = units.iter().rposition(|u| !is_space(u))? + 1;
    let word = &units[start..end];
    let eq = |name: &str| {
        let mut it = name.chars();
        let mut w = char::decode_utf16(word.iter().copied());
        loop {
            match (it.next(), w.next()) {
                (None, None) => return true,
                (Some(a), Some(Ok(b))) if a == b || b.to_lowercase().eq(a.to_lowercase()) => {}
                _ => return false,
            }
        }
    };
    if ["истина", "да", "true"].into_iter().any(eq) {
        Some(true)
    } else if ["ложь", "нет", "false"].into_iter().any(eq) {
        Some(false)
    } else {
        None
    }
}

pub fn to_bool(ex: &mut Execution, v: &Value) -> Result<bool, Exception> {
    match v {
        Value::Bool(b) => Ok(*b),
        Value::Number(n) => Ok(!n.is_zero()),
        Value::String(s) => match string_to_bool(s.units()) {
            Some(b) => Ok(b),
            None => Err(fail(ex, CONVERT_2BOOL)),
        },
        _ => Err(fail(ex, CONVERT_2BOOL)),
    }
}

pub fn neg(ex: &mut Execution, v: &Value) -> Result<Value, Exception> {
    Ok(Value::Number(to_number(ex, v)?.neg()))
}
