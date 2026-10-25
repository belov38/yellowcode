use super::{DateQ, NumQ, Other, QType, StrQ};
use query_syntax::{Func, Kw, ast::BinOp};

#[derive(Clone, Copy, Debug)]
pub struct Arg<'a> {
    pub ty: &'a QType,
    pub constant: bool,
    pub int: Option<i64>,
    pub unit: Option<Kw>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArgError {
    At(usize),
    Func,
    Count,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AggError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArithError {
    Operand,
    Mix,
}

#[derive(Clone, Copy)]
enum P {
    Str,
    Num,
    Date,
    Any,
    Prim,
    Ref,
    NumConst,
    Unit(Units),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Units {
    Period,
    Add,
    Diff,
}

#[derive(Clone, Copy)]
enum R {
    Num(u8, u8, bool),
    Str(u32),
    Date(DateQ),
    Bool,
    Other(Other),
    Substring,
    String,
    ArgLen,
    StrReplace,
    BeginOfPeriod,
    Round,
    Int,
    Sqrt,
    RefPres,
    IsNull,
}

struct Row {
    args: &'static [P],
    optional: usize,
    res: R,
}

fn row(f: Func) -> Option<Row> {
    use P::*;
    let (args, optional, res): (&'static [P], usize, R) = match f {
        Func::Substring => (&[Str, Num, Num], 0, R::Substring),
        Func::String => (&[Prim], 0, R::String),
        Func::StringLength => (&[Str], 0, R::Num(12, 0, false)),
        Func::StrFind => (&[Str, Str], 0, R::Num(12, 0, false)),
        Func::TrimL | Func::TrimR | Func::TrimAll | Func::Upper | Func::Lower => {
            (&[Str], 0, R::ArgLen)
        }
        Func::Left | Func::Right => (&[Str, Num], 0, R::Str(0)),
        Func::StrReplace => (&[Str, Str, Str], 0, R::StrReplace),
        Func::Year => (&[Date], 0, R::Num(4, 0, true)),
        Func::Quarter | Func::WeekDay => (&[Date], 0, R::Num(1, 0, true)),
        Func::Month | Func::Day | Func::Week | Func::Hour | Func::Minute | Func::Second => {
            (&[Date], 0, R::Num(2, 0, true))
        }
        Func::DayOfYear => (&[Date], 0, R::Num(3, 0, true)),
        Func::BeginOfPeriod => (&[Date, Unit(Units::Period)], 0, R::BeginOfPeriod),
        Func::EndOfPeriod => (&[Date, Unit(Units::Period)], 0, R::Date(DateQ::DateTime)),
        Func::DateAdd => (&[Date, Unit(Units::Add), Num], 0, R::Date(DateQ::DateTime)),
        Func::DateDiff => (&[Date, Date, Unit(Units::Diff)], 0, R::Num(12, 0, false)),
        Func::Round => (&[Num, NumConst], 1, R::Round),
        Func::Int => (&[Num], 0, R::Int),
        Func::Sqrt => (&[Num], 0, R::Sqrt),
        Func::Pow => (&[Num, Num], 0, R::Num(38, 6, false)),
        Func::Exp => (&[Num], 0, R::Num(38, 9, false)),
        Func::Log | Func::Log10 => (&[Num], 0, R::Num(12, 9, false)),
        Func::Sin | Func::Cos | Func::ACos | Func::ASin | Func::ATan => {
            (&[Num], 0, R::Num(10, 9, false))
        }
        Func::Presentation => (&[Any], 0, R::Str(0)),
        Func::RefPresentation => (&[Any], 0, R::RefPres),
        Func::ValueType => (&[Any], 0, R::Other(Other::Type)),
        Func::Uuid => (&[Ref], 0, R::Other(Other::Uuid)),
        Func::RecordAutoNumber => (&[], 0, R::Num(0, 0, false)),
        Func::GroupedBy => (&[Any], 0, R::Bool),
        Func::StoredDataSize => (&[Any], 0, R::Num(12, 0, false)),
        Func::IsNull => (&[Any, Any], 0, R::IsNull),
        Func::Sum | Func::Avg | Func::Min | Func::Max | Func::Count => return None,
    };
    Some(Row {
        args,
        optional,
        res,
    })
}

pub fn arg_type(f: Func, i: usize) -> Option<QType> {
    let row = row(f)?;
    match row.args.get(i)? {
        P::Str => Some(string(0)),
        P::Num | P::NumConst => Some(number(0, 0, false)),
        P::Date => Some(QType::date(DateQ::DateTime)),
        P::Any | P::Prim | P::Ref | P::Unit(_) => None,
    }
}

pub(crate) enum TypeResult<'a> {
    Scalar(QType),
    Other(Other),
    RefPresentation(&'a QType),
    WithoutNull(&'a QType),
    UnionWithoutNull(&'a QType, &'a QType),
}
impl TypeResult<'_> {
    pub(crate) fn into_legacy(self) -> QType {
        match self {
            Self::Scalar(value) => value,
            Self::Other(atom) => QType::other(atom),
            Self::WithoutNull(value) => value.clone().without_null(),
            Self::UnionWithoutNull(left, right) => left.merge(right).without_null(),
            Self::RefPresentation(value) => {
                let mut output = string(0);
                output.refs.clone_from(&value.refs);
                output.others = value
                    .others
                    .iter()
                    .copied()
                    .filter(|atom| matches!(atom, Other::RoutePoint(_)))
                    .collect();
                output
            }
        }
    }
}

pub fn func(f: Func, args: &[Arg<'_>]) -> Result<QType, ArgError> {
    func_recipe(f, args).map(TypeResult::into_legacy)
}

pub(crate) fn func_recipe<'a>(f: Func, args: &[Arg<'a>]) -> Result<TypeResult<'a>, ArgError> {
    let Some(row) = row(f) else {
        let [a] = args else {
            return Err(ArgError::Count);
        };
        return aggregate_recipe(f, Some(a.ty)).map_err(|AggError| ArgError::At(0));
    };
    let min = row.args.len().saturating_sub(row.optional);
    if args.len() > row.args.len() {
        return Err(ArgError::Count);
    }
    if args.len() < min {
        for (i, (a, p)) in args.iter().zip(row.args).enumerate() {
            if !fits(*p, a) {
                return Err(ArgError::At(i));
            }
        }
        return Err(ArgError::Count);
    }
    for (i, (a, p)) in args.iter().zip(row.args).enumerate() {
        if !fits(*p, a) {
            return Err(ArgError::At(i));
        }
    }
    for (i, (a, p)) in args.iter().zip(row.args).enumerate() {
        if let P::Unit(units) = p {
            check_unit(*units, a.unit, i)?;
        }
    }
    Ok(result(row.res, args))
}

pub fn aggregate(f: Func, arg: Option<&QType>) -> Result<QType, AggError> {
    aggregate_recipe(f, arg).map(TypeResult::into_legacy)
}

pub(crate) fn aggregate_recipe(f: Func, arg: Option<&QType>) -> Result<TypeResult<'_>, AggError> {
    match (f, arg) {
        (Func::Count, None) => Ok(TypeResult::Scalar(number(0, 0, true))),
        (Func::Count, Some(t)) if !t.has_unlimited_string() => {
            Ok(TypeResult::Scalar(number(0, 0, true)))
        }
        (Func::Sum | Func::Avg, Some(t)) if of_kind(t, t.only_number()) => {
            Ok(TypeResult::Scalar(number(0, 0, false)))
        }
        (Func::Min | Func::Max, Some(t)) if !t.has_unlimited_string() => {
            Ok(TypeResult::WithoutNull(t))
        }
        _ => Err(AggError),
    }
}

pub fn arith(op: BinOp, l: &Arg<'_>, r: &Arg<'_>) -> Result<QType, ArithError> {
    use Operand::*;
    let (lk, rk) = (operand(l.ty), operand(r.ty));
    match op {
        BinOp::And | BinOp::Or => Ok(QType::boolean()),
        BinOp::Add => match (lk, rk) {
            (Bad, _) | (_, Bad) => Err(ArithError::Operand),
            (Str(_), Num) | (Num, Str(_)) if l.constant && r.constant => Err(ArithError::Mix),
            (Str(_), Num) | (Num, Str(_)) => Err(ArithError::Operand),
            (Str(a), Str(b)) => Ok(string(a.saturating_add(b))),
            (Str(a), Empty) | (Empty, Str(a)) => Ok(string(a)),
            (Empty, Empty) => Ok(QType::undefined()),
            (Num | Empty, Num | Empty) => Ok(number(0, 0, false)),
        },
        BinOp::Sub | BinOp::Mul | BinOp::Div => match (lk, rk) {
            (Num | Empty, Num | Empty) => Ok(number(0, 0, false)),
            _ => Err(ArithError::Operand),
        },
    }
}

pub fn comparable(t: &QType) -> bool {
    !t.has_unlimited_string()
}

pub fn cast(src: &QType, target: &QType) -> bool {
    src.atom_count() == 0
        || (target.boolean && src.boolean)
        || (target.number.is_some() && src.number.is_some())
        || (target.string.is_some() && src.string.is_some())
        || (target.date.is_some() && src.date.is_some())
        || target.refs.iter().any(|r| src.refs.contains(r))
        || target.others.iter().any(|o| src.others.contains(o))
}

#[derive(Clone, Copy)]
enum Operand {
    Empty,
    Num,
    Str(u32),
    Bad,
}

fn operand(t: &QType) -> Operand {
    if t.atom_count() == 0 {
        Operand::Empty
    } else if t.only_number() {
        Operand::Num
    } else {
        match t.string {
            Some(q) if t.only_string() && q.len > 0 => Operand::Str(q.len),
            _ => Operand::Bad,
        }
    }
}

fn of_kind(t: &QType, only: bool) -> bool {
    only || t.atom_count() == 0
}

fn fits(p: P, a: &Arg<'_>) -> bool {
    let t = a.ty;
    match p {
        P::Str => of_kind(t, t.only_string()),
        P::Num => of_kind(t, t.only_number()),
        P::Date => of_kind(t, t.only_date()),
        P::Any | P::Unit(_) => true,
        P::Prim => of_kind(
            t,
            t.only_number() || t.only_date() || t.only_bool() || t.only_string(),
        ),
        P::Ref => of_kind(t, !t.refs.is_empty() && t.atom_count() == t.refs.len()),
        P::NumConst => a.constant && of_kind(t, t.only_number()),
    }
}

pub fn is_period_unit(k: Kw) -> bool {
    PERIOD_UNITS.contains(&k)
}

pub const PERIOD_UNITS: [Kw; 9] = [
    Kw::Minute,
    Kw::Hour,
    Kw::Day,
    Kw::Week,
    Kw::TenDays,
    Kw::Month,
    Kw::Quarter,
    Kw::HalfYear,
    Kw::Year,
];
pub const UNITS_WITH_SECOND: [Kw; 10] = [
    Kw::Second,
    Kw::Minute,
    Kw::Hour,
    Kw::Day,
    Kw::Week,
    Kw::TenDays,
    Kw::Month,
    Kw::Quarter,
    Kw::HalfYear,
    Kw::Year,
];
const DIFF_UNITS: [Kw; 7] = [
    Kw::Second,
    Kw::Minute,
    Kw::Hour,
    Kw::Day,
    Kw::Month,
    Kw::Quarter,
    Kw::Year,
];

fn check_unit(units: Units, unit: Option<Kw>, at: usize) -> Result<(), ArgError> {
    let ok = unit.is_some_and(|k| match units {
        Units::Period => PERIOD_UNITS.contains(&k),
        Units::Add => UNITS_WITH_SECOND.contains(&k),
        Units::Diff => DIFF_UNITS.contains(&k),
    });
    match unit {
        _ if ok => Ok(()),
        Some(Kw::Week) if units == Units::Diff => Err(ArgError::Func),
        _ => Err(ArgError::At(at)),
    }
}

fn number(digits: u8, scale: u8, nonneg: bool) -> QType {
    QType::num(NumQ {
        digits,
        scale,
        nonneg,
    })
}

fn string(len: u32) -> QType {
    QType::str_(StrQ { len, fixed: false })
}

fn str_len(a: Option<&Arg<'_>>) -> u32 {
    a.and_then(|a| a.ty.string).map_or(0, |q| q.len)
}

fn num_q(a: &Arg<'_>) -> NumQ {
    a.ty.number.unwrap_or(NumQ {
        digits: 0,
        scale: 0,
        nonneg: false,
    })
}

fn result<'a>(res: R, args: &[Arg<'a>]) -> TypeResult<'a> {
    let first = args.first();
    let scalar = TypeResult::Scalar;
    match res {
        R::Num(d, s, nonneg) => scalar(number(d, s, nonneg)),
        R::Str(len) => scalar(string(len)),
        R::Date(q) => scalar(QType::date(q)),
        R::Bool => scalar(QType::boolean()),
        R::Other(atom) => TypeResult::Other(atom),
        R::Substring => scalar(match args.get(2) {
            Some(Arg {
                constant: true,
                int: Some(n),
                ..
            }) if *n > 0 => string(u32::try_from(*n).unwrap_or(u32::MAX)),
            Some(Arg {
                constant: true,
                int: Some(_),
                ..
            }) => string(str_len(first)),
            _ => string(0),
        }),
        R::String => scalar(match first {
            Some(a) if a.ty.has_number() => match num_q(a).digits {
                p if a.constant || p == 0 => string(76),
                p => string(2 * u32::from(p)),
            },
            Some(a) if a.ty.date.is_some() || a.ty.boolean => string(32),
            a => string(str_len(a)),
        }),
        R::ArgLen => scalar(string(str_len(first))),
        R::StrReplace if args.iter().all(|a| a.constant) => scalar(string(str_len(first))),
        R::StrReplace => scalar(string(0)),
        R::BeginOfPeriod => scalar(match args.get(1).and_then(|a| a.unit) {
            Some(Kw::Minute | Kw::Hour) => QType::date(DateQ::DateTime),
            _ => QType::date(DateQ::Date),
        }),
        R::Round => scalar(match first {
            Some(x) if !x.constant => {
                let q = num_q(x);
                let n = match args.get(1) {
                    None => 0,
                    Some(a) => a.int.unwrap_or(i64::from(q.scale)),
                };
                let scale = n.clamp(0, i64::from(q.scale));
                number(q.digits, u8::try_from(scale).unwrap_or(q.scale), false)
            }
            _ => number(0, 0, false),
        }),
        R::Int => scalar(match first {
            Some(x) if !x.constant => {
                let q = num_q(x);
                number(q.digits.saturating_sub(q.scale), 0, false)
            }
            _ => number(0, 0, false),
        }),
        R::Sqrt => scalar(match first {
            Some(x) if !x.constant => number(21, 9, false),
            _ => number(0, 0, false),
        }),
        R::RefPres => first.map_or_else(
            || scalar(string(0)),
            |arg| TypeResult::RefPresentation(arg.ty),
        ),
        R::IsNull => match (first, args.get(1)) {
            (Some(a), Some(b)) => TypeResult::UnionWithoutNull(a.ty, b.ty),
            _ => scalar(QType::undefined()),
        },
    }
}
