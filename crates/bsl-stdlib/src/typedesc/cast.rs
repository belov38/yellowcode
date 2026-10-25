use super::{AllowedLength, AllowedSign, DateParts, TypeDescription};
use crate::bind::parse_number;
use bsl_prim::{BslDate, Number, RoundMode, Str};
use bsl_vm::{Exception, Execution, TypeId, Value};

pub fn cast_value(ex: &mut Execution, td: &TypeDescription, v: Value) -> Result<Value, Exception> {
    if td.types.is_empty() || (td.keeps_undefined && matches!(v, Value::Undefined)) {
        return Ok(v);
    }
    let value = if td.contains(v.type_id()) {
        v
    } else if matches!(v, Value::Undefined) {
        default(ex, td.types[0])?
    } else {
        let mut converted = None;
        for t in &td.types {
            if let Some(value) = convert(ex, *t, &v)? {
                converted = Some(value);
                break;
            }
        }
        match converted {
            Some(v) => v,
            None => default(ex, td.types[0])?,
        }
    };
    match value {
        Value::Number(n) => {
            let q = td.number();
            if q.sign == AllowedSign::Nonnegative && n.is_negative() {
                return Ok(Value::Number(Number::zero()));
            }
            if q.digits == 0 {
                return Ok(Value::Number(n));
            }
            ex.reserve(n.size_hint_limbs().saturating_mul(32).saturating_add(256))?;
            let digits = q.digits.min(32);
            let fraction = q.fraction.min(digits);
            let mut n = n.round(i64::from(fraction), RoundMode::HalfAwayFromZero);
            let whole = digits - fraction;
            let mut maximum = if whole == 0 {
                "0".to_owned()
            } else {
                "9".repeat(whole as usize)
            };
            if fraction != 0 {
                maximum.push('.');
                maximum.push_str(&"9".repeat(fraction as usize));
            }
            let bound = Number::parse(&maximum).unwrap_or_else(Number::zero);
            if n.abs() > bound {
                n = if n.is_negative() { bound.neg() } else { bound };
            }
            Ok(Value::Number(n))
        }
        Value::String(s) => {
            let q = td.string();
            if q.length == 0 {
                return Ok(Value::String(s));
            }
            let limit = q.length as usize;
            let len = if q.allowed == AllowedLength::Fixed {
                limit
            } else {
                limit.min(s.len())
            };
            let mut units = adjusted_vec(ex, len)?;
            units.extend_from_slice(&s.units()[..s.len().min(limit)]);
            units.resize(len, u16::from(b' '));
            Ok(Value::String(Str::from_vec(units)))
        }
        Value::Date(d) => {
            let day = BslDate::TICKS_PER_SECOND * 86400;
            let ticks = match td.date().parts {
                DateParts::DateTime => d.ticks(),
                DateParts::Date => d.ticks() / day * day,
                DateParts::Time => d.ticks() % day,
            };
            Ok(Value::Date(
                BslDate::from_ticks(ticks).unwrap_or(BslDate::EMPTY),
            ))
        }
        v => {
            if let Some(bytes) = crate::values::binary_data_bytes(&v) {
                let q = td.binary();
                if q.length != 0 {
                    let limit = q.length as usize;
                    let len = if q.allowed == AllowedLength::Fixed {
                        limit
                    } else {
                        limit.min(bytes.len())
                    };
                    let mut adjusted = adjusted_vec(ex, len)?;
                    adjusted.extend_from_slice(&bytes[..bytes.len().min(limit)]);
                    adjusted.resize(len, 0);
                    return Ok(crate::values::new_binary_data(adjusted));
                }
            }
            Ok(v)
        }
    }
}

fn adjusted_vec<T>(ex: &mut Execution, len: usize) -> Result<Vec<T>, Exception> {
    const MAX_ADJUSTED_BYTES: u64 = 1 << 28;
    let bytes = (len as u64).saturating_mul(std::mem::size_of::<T>() as u64);
    if bytes > MAX_ADJUSTED_BYTES {
        return Err(ex.out_of_memory());
    }
    ex.reserve(bytes)?;
    ex.charge(bytes / 64)?;
    let mut out = Vec::new();
    out.try_reserve_exact(len).map_err(|_| ex.out_of_memory())?;
    Ok(out)
}

fn convert(ex: &mut Execution, t: TypeId, v: &Value) -> Result<Option<Value>, Exception> {
    Ok(match (t, v) {
        (TypeId::STRING, _) => Some(Value::String(ex.present(v)?)),
        (TypeId::NUMBER, Value::String(s)) => parse_number(ex, s.units()).map(Value::Number),
        (TypeId::NUMBER, Value::Bool(b)) => Some(Value::Number(Number::from_u64(u64::from(*b)))),
        (TypeId::BOOL, Value::Number(n)) => Some(Value::Bool(!n.is_zero())),
        (TypeId::BOOL, Value::String(s)) => {
            let text = bsl_prim::fold_name(&s.to_string_lossy());
            match text.as_str() {
                "истина" | "true" | "да" | "yes" => Some(Value::Bool(true)),
                "ложь" | "false" | "нет" | "no" => Some(Value::Bool(false)),
                _ => None,
            }
        }
        (TypeId::DATE, Value::String(s)) => bsl_intl::calendar::date_from_digits(s.units())
            .ok()
            .map(Value::Date),
        _ => None,
    })
}
fn default(ex: &mut Execution, t: TypeId) -> Result<Value, Exception> {
    let app = ex
        .runtime()
        .types()
        .app_type(t)
        .and_then(|a| a.default_value.clone());
    if let Some(v) = app {
        return v.to_value(ex);
    }
    Ok(match t {
        TypeId::NUMBER => Value::Number(Number::zero()),
        TypeId::STRING => Value::String(Str::from("")),
        TypeId::DATE => Value::Date(BslDate::EMPTY),
        TypeId::BOOL => Value::Bool(false),
        TypeId::NULL => Value::Null,
        _ if t == TypeId::of_catalog(crate::collections::types().uuid) => {
            crate::values::new_uuid([0; 16])
        }
        _ if t == TypeId::of_catalog(crate::collections::types().binary) => {
            crate::values::new_binary_data(Vec::new())
        }
        _ => Value::Undefined,
    })
}
