use crate::data::{PluralCondition, PluralOperand, PluralRelation};
use crate::error::{LibError, msg};
use crate::format::{FmtValue, FormatEnv, format, parse_spec, words};
use crate::locale::{Locale, locale};
use crate::present::number_to_string;
use bsl_prim::Number;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NumberKind {
    Cardinal,
    Ordinal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Zero = 0,
    One = 1,
    Two = 2,
    Few = 3,
    Many = 4,
    Other = 5,
}

const CATEGORIES: [Category; 5] = [
    Category::Zero,
    Category::One,
    Category::Two,
    Category::Few,
    Category::Many,
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Operands {
    int: String,
    frac: String,
}

impl Operands {
    pub fn of_number(n: &Number) -> Operands {
        Operands {
            int: n.int_digits(),
            frac: n.frac_digits(),
        }
    }

    pub fn of_u64(n: u64) -> Operands {
        Operands {
            int: n.to_string(),
            frac: String::new(),
        }
    }

    pub fn parse(s: &str) -> Option<Operands> {
        let (int, frac) = s.split_once('.').unwrap_or((s, ""));
        let digits = |p: &str| p.bytes().all(|c| c.is_ascii_digit());
        if (int.is_empty() && frac.is_empty()) || !digits(int) || !digits(frac) {
            return None;
        }
        let int = int.trim_start_matches('0');
        Some(Operands {
            int: if int.is_empty() { "0" } else { int }.to_string(),
            frac: frac.to_string(),
        })
    }

    fn trimmed_frac(&self) -> &str {
        self.frac.trim_end_matches('0')
    }
}

enum Value<'a> {
    Digits(&'a str),
    Count(usize),
}

fn reduce(v: Value<'_>, modulus: Option<u64>) -> Option<u64> {
    match (v, modulus) {
        (Value::Count(c), None) => u64::try_from(c).ok(),
        (Value::Count(c), Some(m)) => u64::try_from(c).ok()?.checked_rem(m),
        (Value::Digits(d), None) => {
            let d = d.trim_start_matches('0');
            if d.len() > 19 {
                return None;
            }
            Some(d.parse().unwrap_or(0))
        }
        (Value::Digits(d), Some(m)) => {
            if m == 0 {
                return None;
            }
            let m = u128::from(m);
            let r = d
                .bytes()
                .fold(0u128, |acc, c| (acc * 10 + u128::from(c - b'0')) % m);
            u64::try_from(r).ok()
        }
    }
}

fn holds(r: &PluralRelation, ops: &Operands) -> bool {
    let value = match r.operand {
        PluralOperand::N if !ops.trimmed_frac().is_empty() => return r.negated,
        PluralOperand::N | PluralOperand::I => Value::Digits(&ops.int),
        PluralOperand::V => Value::Count(ops.frac.len()),
        PluralOperand::W => Value::Count(ops.trimmed_frac().len()),
        PluralOperand::F => Value::Digits(&ops.frac),
        PluralOperand::T => Value::Digits(ops.trimmed_frac()),
    };
    let inside =
        reduce(value, r.modulus).is_some_and(|v| r.ranges.iter().any(|&(a, b)| a <= v && v <= b));
    inside != r.negated
}

fn satisfies(c: &PluralCondition, ops: &Operands) -> bool {
    c.iter().any(|and| and.iter().all(|r| holds(r, ops)))
}

pub fn select(rules: &[Option<PluralCondition>; 5], ops: &Operands) -> Category {
    CATEGORIES
        .iter()
        .zip(rules)
        .find(|(_, c)| c.as_ref().is_some_and(|c| satisfies(c, ops)))
        .map_or(Category::Other, |(k, _)| *k)
}

pub fn category(locale: &str, kind: NumberKind, ops: &Operands) -> Category {
    let p = &words().plurals;
    let sets = match kind {
        NumberKind::Cardinal => &p.cardinal,
        NumberKind::Ordinal => &p.ordinal,
    };
    let language = locale.split('_').next().unwrap_or(locale);
    sets.get(locale)
        .or_else(|| sets.get(language))
        .or_else(|| sets.get("root"))
        .and_then(|set| p.rules.get(set))
        .map_or(Category::Other, |r| select(r, ops))
}

pub fn string_with_number(
    forms: &[u16],
    n: &Number,
    kind: NumberKind,
    fmt: Option<&[u16]>,
    session: &Locale,
) -> Result<String, LibError> {
    let parts: Vec<&[u16]> = forms.split(|&c| c == u16::from(b';')).collect();
    let [_, _, _, _, _, _] = parts.as_slice() else {
        return Err(LibError::new(msg::PARAM_VALUE).with_arg(1));
    };
    let loc = fmt
        .and_then(|f| parse_spec(f).locale)
        .and_then(|name| locale(&name))
        .unwrap_or(session);
    let cat = category(loc.name(), kind, &Operands::of_number(n));
    let form = String::from_utf16_lossy(parts.get(cat as usize).copied().unwrap_or_default());
    let number = match fmt {
        Some(f) => format(FmtValue::Num(n), f, &FormatEnv { session }),
        None => number_to_string(n, session),
    };
    Ok(form.replace("%1", &number))
}
