use super::{Mag, Number, small};
use crate::limbs::Limbs;
use std::cmp::Ordering;

fn aligned(n: &Number, scale: u32) -> Limbs {
    n.mag.limbs().shl_limbs((scale - n.scale) as usize)
}

pub(super) fn cmp_limbs(a: &[u32], sa: u32, b: &[u32], sb: u32) -> Ordering {
    match (a.is_empty(), b.is_empty()) {
        (true, true) => return Ordering::Equal,
        (true, false) => return Ordering::Less,
        (false, true) => return Ordering::Greater,
        _ => {}
    }
    let top = |l: &[u32], s: u32| l.len() as i64 - 1 - i64::from(s);
    let (ta, tb) = (top(a, sa), top(b, sb));
    ta.cmp(&tb).then_with(|| {
        let get = |l: &[u32], s: u32, e: i64| {
            usize::try_from(e + i64::from(s))
                .ok()
                .and_then(|i| l.get(i).copied())
                .unwrap_or(0)
        };
        let lo = -i64::from(sa.max(sb));
        (lo..=ta)
            .rev()
            .map(|e| get(a, sa, e).cmp(&get(b, sb, e)))
            .find(|c| c.is_ne())
            .unwrap_or(Ordering::Equal)
    })
}

pub(super) fn cmp_abs(a: &Number, b: &Number) -> Ordering {
    if let (Mag::Small(x), Mag::Small(y)) = (&a.mag, &b.mag) {
        return small::cmp_small(*x, a.scale, *y, b.scale);
    }
    a.mag.with_limbs(|a_limbs| {
        b.mag
            .with_limbs(|b_limbs| cmp_limbs(a_limbs, a.scale, b_limbs, b.scale))
    })
}

fn add_signed(a: &Number, b: &Number, b_neg: bool) -> Number {
    if let Some(r) = small::try_add(a, b, b_neg) {
        return r;
    }
    let scale = a.scale.max(b.scale);
    let (x, y) = (aligned(a, scale), aligned(b, scale));
    let (neg, mag) = if a.neg == b_neg {
        (a.neg, x.add(&y))
    } else {
        match x.cmp_mag(&y) {
            Ordering::Less => (b_neg, y.sub(&x)),
            _ => (a.neg, x.sub(&y)),
        }
    };
    Number::from_limbs(neg, scale, mag)
}

pub(super) fn add(a: &Number, b: &Number) -> Number {
    add_signed(a, b, b.neg)
}

pub(super) fn sub(a: &Number, b: &Number) -> Number {
    add_signed(a, b, !b.neg)
}

pub(super) fn mul(a: &Number, b: &Number) -> Number {
    if let Some(r) = small::try_mul(a, b) {
        return r;
    }
    Number::from_limbs(
        a.neg != b.neg,
        a.scale.saturating_add(b.scale),
        a.mag.limbs().mul(&b.mag.limbs()),
    )
}
