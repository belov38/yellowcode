use super::Number;
use crate::limbs::Limbs;
use std::borrow::Cow;
use std::cmp::Ordering;

pub(super) const LIMIT: u64 = 1_000_000_000_000_000_000;
const GROUP: u64 = 1_000_000_000;

#[derive(Clone, Debug)]
pub(crate) enum Mag {
    Small(u64),
    Big(Limbs),
}

impl Mag {
    pub(crate) fn is_zero(&self) -> bool {
        match self {
            Mag::Small(m) => *m == 0,
            Mag::Big(l) => l.is_zero(),
        }
    }

    pub(crate) fn with_limbs<R>(&self, read: impl FnOnce(&[u32]) -> R) -> R {
        match self {
            Mag::Small(m) => {
                let limbs = [(*m % GROUP) as u32, (*m / GROUP) as u32];
                let len = if *m == 0 {
                    0
                } else if *m < GROUP {
                    1
                } else {
                    2
                };
                read(limbs.get(..len).unwrap_or(&[]))
            }
            Mag::Big(l) => read(&l.0),
        }
    }

    pub(crate) fn limbs(&self) -> Cow<'_, Limbs> {
        match self {
            Mag::Small(m) => Cow::Owned(Limbs::from_u64(*m)),
            Mag::Big(l) => Cow::Borrowed(l),
        }
    }

    pub(crate) fn to_u64(&self) -> Option<u64> {
        match self {
            Mag::Small(m) => Some(*m),
            Mag::Big(l) => l.to_u64(),
        }
    }

    pub(crate) fn as_small(&self) -> Option<u64> {
        match self {
            Mag::Small(m) => Some(*m),
            Mag::Big(l) if l.0.len() <= 2 => l.to_u64(),
            Mag::Big(_) => None,
        }
    }
}

pub(super) fn canon(neg: bool, mut scale: u32, mut m: u64) -> Number {
    debug_assert!(m < LIMIT);
    if m == 0 {
        return Number::zero();
    }
    while scale > 0 && m.is_multiple_of(GROUP) {
        m /= GROUP;
        scale -= 1;
    }
    Number {
        neg,
        scale,
        mag: Mag::Small(m),
    }
}

fn align(m: u64, d: u32) -> Option<u128> {
    if m == 0 {
        return Some(0);
    }
    match d {
        0 => Some(u128::from(m)),
        1 => Some(u128::from(m) * u128::from(GROUP)),
        2 => Some(u128::from(m) * u128::from(GROUP) * u128::from(GROUP)),
        _ => None,
    }
}

pub(super) fn try_add(a: &Number, b: &Number, b_neg: bool) -> Option<Number> {
    let (Mag::Small(am), Mag::Small(bm)) = (&a.mag, &b.mag) else {
        return None;
    };
    let scale = a.scale.max(b.scale);
    let x = align(*am, scale - a.scale)?;
    let y = align(*bm, scale - b.scale)?;
    let (neg, r) = if a.neg == b_neg {
        (a.neg, x + y)
    } else if x >= y {
        (a.neg, x - y)
    } else {
        (b_neg, y - x)
    };
    let r = u64::try_from(r).ok().filter(|&r| r < LIMIT)?;
    Some(canon(neg, scale, r))
}

pub(super) fn try_mul(a: &Number, b: &Number) -> Option<Number> {
    let (Mag::Small(am), Mag::Small(bm)) = (&a.mag, &b.mag) else {
        return None;
    };
    let r = u128::from(*am) * u128::from(*bm);
    let r = u64::try_from(r).ok().filter(|&r| r < LIMIT)?;
    Some(canon(a.neg != b.neg, a.scale.saturating_add(b.scale), r))
}

pub(super) fn cmp_small(am: u64, a_scale: u32, bm: u64, b_scale: u32) -> Ordering {
    match a_scale.cmp(&b_scale) {
        Ordering::Equal => am.cmp(&bm),
        Ordering::Less => match align(am, b_scale - a_scale) {
            Some(x) => x.cmp(&u128::from(bm)),
            None => Ordering::Greater,
        },
        Ordering::Greater => match align(bm, a_scale - b_scale) {
            Some(y) => u128::from(am).cmp(&y),
            None => Ordering::Less,
        },
    }
}

pub(super) fn int_part(m: u64, scale: u32) -> u64 {
    match scale {
        0 => m,
        1 => m / GROUP,
        _ => 0,
    }
}
