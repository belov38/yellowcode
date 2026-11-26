mod cast;
mod compare;
pub(crate) mod dates;
mod like;
mod logic;
mod strings;

pub(crate) use cast::load_cast_with_budget;
pub use cast::{cast, load_cast};
pub(crate) use compare::compare_cmp_with;
pub use compare::{cmp, compare, eq3, rank, try_cmp, try_compare, try_eq3};
pub use dates::{unit_arg, unit_of, week_begin, week_call};
pub use like::{LIKE_CP_KEY, like_cost};
pub use like::{LikePattern, like, try_like};
pub use logic::{and3, case_truth, not3, or3, to_value, truth};
pub use strings::present as text_of;

use query_ir::{DateQ, ExecError, NumQ, Other, QStr, QType, QValue, RefAtom, StrQ};
use query_syntax::Func;

use crate::model::{block, vec_block};
use crate::ops::MemBudget;
use crate::source::ExecEnv;

const SCALAR_TEXT_UNITS: u64 = 128;

pub(crate) fn charge_string(units: u64, mem: &MemBudget<'_>) -> Result<(), ExecError> {
    let arc = block(16 + 2 * units);
    mem.check_value(arc)?;
    mem.grow_scratch(vec_block(usize::try_from(units).unwrap_or(usize::MAX), 2) + arc)
}

pub fn text_of_charged(v: &QValue, mem: &MemBudget<'_>) -> Result<Vec<u16>, ExecError> {
    let units = match v {
        QValue::String(s) => s.0.len() as u64,
        _ => SCALAR_TEXT_UNITS,
    };
    charge_string(units, mem)?;
    text_of(v)
}

pub(crate) fn code_points(text: &[u16]) -> impl Iterator<Item = u32> + '_ {
    char::decode_utf16(text.iter().copied()).map(|r| match r {
        Ok(c) => u32::from(c),
        Err(e) => u32::from(e.unpaired_surrogate()),
    })
}

pub(crate) fn push_units(cp: u32, out: &mut Vec<u16>) {
    match char::from_u32(cp) {
        Some(c) => {
            let mut buf = [0u16; 2];
            out.extend_from_slice(c.encode_utf16(&mut buf));
        }
        None => out.push(cp as u16),
    }
}

pub(crate) fn fatal(what: &str) -> ExecError {
    ExecError::Fatal(format!("eval: {what}"))
}

pub(crate) fn str_value(units: Vec<u16>) -> QValue {
    QValue::String(QStr(units.into()))
}

pub(crate) fn format_orphaned_predef_id(raw: &[u8; 16]) -> [u16; 37] {
    let hex = |n: u8| u16::from(if n < 10 { b'0' + n } else { b'a' + (n - 10) });
    let hi = |b: u8| hex(b >> 4);
    let lo = |b: u8| hex(b & 0x0f);
    let [
        b0,
        b1,
        b2,
        b3,
        b4,
        b5,
        b6,
        b7,
        b8,
        b9,
        b10,
        b11,
        b12,
        b13,
        b14,
        b15,
    ] = *query_ir::uuid_of(raw).as_bytes();
    [
        u16::from(b'#'),
        hi(b0),
        lo(b0),
        hi(b1),
        lo(b1),
        hi(b2),
        lo(b2),
        hi(b3),
        lo(b3),
        u16::from(b'-'),
        hi(b4),
        lo(b4),
        hi(b5),
        lo(b5),
        u16::from(b'-'),
        hi(b6),
        lo(b6),
        hi(b7),
        lo(b7),
        u16::from(b'-'),
        hi(b8),
        lo(b8),
        hi(b9),
        lo(b9),
        u16::from(b'-'),
        hi(b10),
        lo(b10),
        hi(b11),
        lo(b11),
        hi(b12),
        lo(b12),
        hi(b13),
        lo(b13),
        hi(b14),
        lo(b14),
        hi(b15),
        lo(b15),
    ]
}

pub(crate) fn predef_name_value(
    v: &QValue,
    names: &[([u8; 16], QStr)],
    mem: &MemBudget<'_>,
) -> Result<QValue, ExecError> {
    match v {
        QValue::Null => Ok(QValue::Null),
        QValue::Binary(b) => {
            let Ok(raw) = <[u8; 16]>::try_from(b.as_ref()) else {
                return Ok(QValue::String(QStr::from_str("")));
            };
            if raw == [0u8; 16] {
                return Ok(QValue::String(QStr::from_str("")));
            }
            if let Some((_, name)) = names.iter().find(|(id, _)| *id == raw) {
                return Ok(QValue::String(name.clone()));
            }
            let units = format_orphaned_predef_id(&raw);
            charge_string(units.len() as u64, mem)?;
            Ok(QValue::String(QStr(units.as_slice().into())))
        }
        _ => Ok(QValue::String(QStr::from_str(""))),
    }
}

pub fn type_of_value(v: &QValue, refs: &[(u32, RefAtom)]) -> Result<QValue, ExecError> {
    let t = match v {
        QValue::Null => QType::null_only(),
        QValue::Undefined => QType::undefined(),
        QValue::Bool(_) => QType::boolean(),
        QValue::Number(_) => QType::num(NumQ {
            digits: 0,
            scale: 0,
            nonneg: false,
        }),
        QValue::String(_) => QType::str_(StrQ {
            len: 0,
            fixed: false,
        }),
        QValue::Date(_) => QType::date(DateQ::DateTime),
        QValue::Uuid(_) => QType::other(Other::Uuid),
        QValue::Ref { table, .. } => {
            let atom = refs
                .binary_search_by_key(table, |(n, _)| *n)
                .ok()
                .and_then(|i| refs.get(i));
            let Some((_, atom)) = atom else {
                return Err(fatal("ТИПЗНАЧЕНИЯ of a reference to an unknown table"));
            };
            QType::reference(*atom)
        }
        QValue::Type(_) => QType::other(Other::Type),
        QValue::Binary(_) => QType::other(Other::Binary),
        QValue::Storage(_) => QType::other(Other::Storage),
        QValue::SysEnum(t, _) => QType::other(Other::SysEnum(*t)),
        QValue::Nested(_) => QType::other(Other::QueryResult),
        QValue::PointInTime(_) => QType::other(Other::PointInTime),
        QValue::Boundary(_) => QType::other(Other::Boundary),
    };
    Ok(QValue::Type(t))
}

pub fn func(
    f: Func,
    args: &[QValue],
    ty: &QType,
    env: &ExecEnv<'_>,
    mem: &MemBudget<'_>,
) -> Result<QValue, ExecError> {
    func_with(
        f,
        args,
        ty,
        crate::coll::ExecCollation::Legacy(env.collation),
        mem,
    )
}

pub fn func_with(
    f: Func,
    args: &[QValue],
    ty: &QType,
    coll: crate::coll::ExecCollation<'_>,
    mem: &MemBudget<'_>,
) -> Result<QValue, ExecError> {
    if f == Func::IsNull {
        return match args {
            [a, b] => Ok(if matches!(a, QValue::Null) {
                b.clone()
            } else {
                a.clone()
            }),
            _ => Err(fatal("ЕСТЬNULL takes two arguments")),
        };
    }
    if args.iter().any(|a| matches!(a, QValue::Null)) {
        return Ok(QValue::Null);
    }
    let v = match f {
        Func::Uuid => match args {
            [QValue::Ref { id, .. }] => QValue::Uuid(*query_ir::uuid_of(id).as_bytes()),
            _ => return Err(fatal("УНИКАЛЬНЫЙИДЕНТИФИКАТОР takes a reference")),
        },
        Func::Substring
        | Func::String
        | Func::StringLength
        | Func::TrimL
        | Func::TrimR
        | Func::TrimAll
        | Func::Left
        | Func::Right
        | Func::StrFind
        | Func::Upper
        | Func::Lower
        | Func::StrReplace => strings::call(f, args, coll, mem)?,
        Func::Year
        | Func::Quarter
        | Func::Month
        | Func::DayOfYear
        | Func::Day
        | Func::Week
        | Func::WeekDay
        | Func::Hour
        | Func::Minute
        | Func::Second
        | Func::BeginOfPeriod
        | Func::EndOfPeriod
        | Func::DateAdd
        | Func::DateDiff => dates::call(f, args)?,
        other => {
            return Err(fatal(&format!(
                "function {other:?} is not a value function"
            )));
        }
    };
    strings::pad_fixed(v, ty, mem)
}
