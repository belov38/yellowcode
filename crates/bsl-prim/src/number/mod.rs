mod arith;
mod div;
mod float;
mod owned;
mod small;

use crate::limbs::Limbs;
pub use owned::{LimbAllocator, LimbBuffer, NumberOpError, NumberOutput, NumberParts};
use small::LIMIT;
pub(crate) use small::Mag;
use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DivByZero;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RoundMode {
    #[default]
    HalfAwayFromZero,
    HalfTowardZero,
}

#[derive(Clone)]
pub struct Number {
    pub(crate) neg: bool,
    pub(crate) scale: u32,
    pub(crate) mag: Mag,
}

#[derive(Debug, PartialEq, Eq)]
pub enum NumberAllocError<E> {
    Reserve(E),
    AllocationFailed,
    Overflow,
}

impl Number {
    pub fn try_clone_with<E>(
        &self,
        reserve: impl FnOnce(usize) -> Result<(), E>,
    ) -> Result<Number, NumberAllocError<E>> {
        let mag = match &self.mag {
            Mag::Small(value) => Mag::Small(*value),
            Mag::Big(limbs) => {
                reserve(limbs.0.len()).map_err(NumberAllocError::Reserve)?;
                let mut copy = Vec::new();
                copy.try_reserve_exact(limbs.0.len())
                    .map_err(|_| NumberAllocError::AllocationFailed)?;
                copy.extend_from_slice(&limbs.0);
                Mag::Big(Limbs(copy))
            }
        };
        Ok(Number {
            neg: self.neg,
            scale: self.scale,
            mag,
        })
    }

    pub fn try_parse_with<E>(
        text: &str,
        reserve: impl FnOnce(usize) -> Result<(), E>,
    ) -> Result<Option<Number>, NumberAllocError<E>> {
        let bytes = text.as_bytes();
        let (neg, bytes) = match bytes.first() {
            Some(b'-') => (true, &bytes[1..]),
            Some(b'+') => (false, &bytes[1..]),
            _ => (false, bytes),
        };
        let (int, frac) = match bytes.iter().position(|&b| b == b'.') {
            Some(index) => (&bytes[..index], &bytes[index + 1..]),
            None => (bytes, &bytes[..0]),
        };
        if (int.is_empty() && frac.is_empty()) || !int.iter().chain(frac).all(u8::is_ascii_digit) {
            return Ok(None);
        }
        let Ok(groups) = u32::try_from(frac.len().div_ceil(9)) else {
            return Ok(None);
        };
        let padded = (groups as usize)
            .checked_mul(9)
            .ok_or(NumberAllocError::Overflow)?;
        let total = int
            .len()
            .checked_add(padded)
            .ok_or(NumberAllocError::Overflow)?;
        let Some(start) = int.iter().chain(frac).position(|&b| b != b'0') else {
            return Ok(Some(Number::zero()));
        };
        let digit = |index: usize| -> u32 {
            u32::from(
                if index < int.len() {
                    int[index]
                } else {
                    frac.get(index - int.len()).copied().unwrap_or(b'0')
                } - b'0',
            )
        };
        if total - start <= 18 {
            let magnitude =
                (start..total).fold(0u64, |value, index| value * 10 + u64::from(digit(index)));
            return Ok(Some(small::canon(neg, groups, magnitude)));
        }
        let capacity = (total - start).div_ceil(9);
        reserve(capacity).map_err(NumberAllocError::Reserve)?;
        let mut limbs = Vec::new();
        limbs
            .try_reserve_exact(capacity)
            .map_err(|_| NumberAllocError::AllocationFailed)?;
        let mut end = total;
        while end > start {
            let begin = end.saturating_sub(9).max(start);
            let limb = (begin..end).fold(0u32, |value, index| value * 10 + digit(index));
            limbs.push(limb);
            end = begin;
        }
        Ok(Some(Number::from_limbs(neg, groups, Limbs(limbs))))
    }

    pub(crate) fn normalized(self) -> Number {
        match self.mag {
            Mag::Small(m) => small::canon(self.neg, self.scale, m),
            Mag::Big(l) => Number::from_limbs(self.neg, self.scale, l),
        }
    }

    pub(crate) fn from_limbs(neg: bool, mut scale: u32, mut l: Limbs) -> Number {
        if l.is_zero() {
            return Number::zero();
        }
        let zeros =
            l.0.iter()
                .take(scale as usize)
                .take_while(|&&x| x == 0)
                .count();
        if zeros > 0 {
            l.0.drain(..zeros);
            scale -= zeros as u32;
        }
        if l.0.len() <= 2
            && let Some(m) = l.to_u64()
        {
            return small::canon(neg, scale, m);
        }
        Number {
            neg,
            scale,
            mag: Mag::Big(l),
        }
    }

    #[doc(hidden)]
    pub fn force_big(&self) -> Number {
        Number {
            neg: self.neg,
            scale: self.scale,
            mag: Mag::Big(self.mag.limbs().into_owned()),
        }
    }

    pub fn zero() -> Number {
        Number {
            neg: false,
            scale: 0,
            mag: Mag::Small(0),
        }
    }

    pub fn one() -> Number {
        Number::from_u64(1)
    }

    pub fn from_u64(v: u64) -> Number {
        if v < LIMIT {
            small::canon(false, 0, v)
        } else {
            Number::from_limbs(false, 0, Limbs::from_u64(v))
        }
    }

    pub fn from_i64(v: i64) -> Number {
        let u = v.unsigned_abs();
        if u < LIMIT {
            small::canon(v < 0, 0, u)
        } else {
            Number::from_limbs(v < 0, 0, Limbs::from_u64(u))
        }
    }

    pub fn backing_capacity_limbs(&self) -> usize {
        match &self.mag {
            Mag::Small(_) => 0,
            Mag::Big(limbs) => limbs.0.capacity(),
        }
    }

    pub fn size_hint_limbs(&self) -> u64 {
        let len = match &self.mag {
            Mag::Small(0) => return 0,
            Mag::Small(m) => 1 + u64::from(*m >= 1_000_000_000),
            Mag::Big(l) => l.0.len() as u64,
        };
        len.max(u64::from(self.scale))
    }

    pub fn from_scaled_i64(v: i64, decimals: u32) -> Number {
        let (q, r) = (decimals / 9, decimals % 9);
        let u = v.unsigned_abs();
        let (m, scale) = if r > 0 {
            (u128::from(u) * u128::from(10u32.pow(9 - r)), q + 1)
        } else {
            (u128::from(u), q)
        };
        match u64::try_from(m) {
            Ok(m) if m < LIMIT => small::canon(v < 0, scale, m),
            _ => {
                let mut mag = Limbs::from_u64(u);
                if r > 0 {
                    mag = mag.mul_small(10u32.pow(9 - r));
                }
                Number::from_limbs(v < 0, scale, mag)
            }
        }
    }

    pub fn parse(s: &str) -> Option<Number> {
        let b = s.as_bytes();
        let (neg, b) = match b.first() {
            Some(b'-') => (true, &b[1..]),
            Some(b'+') => (false, &b[1..]),
            _ => (false, b),
        };
        let (int, frac) = match b.iter().position(|&c| c == b'.') {
            Some(i) => (&b[..i], &b[i + 1..]),
            None => (b, &b[..0]),
        };
        if int.is_empty() && frac.is_empty() {
            return None;
        }
        if !int.iter().chain(frac).all(u8::is_ascii_digit) {
            return None;
        }
        let groups = u32::try_from(frac.len().div_ceil(9)).ok()?;
        let mut digits = Vec::with_capacity(int.len() + groups as usize * 9);
        digits.extend_from_slice(int);
        digits.extend_from_slice(frac);
        digits.resize(int.len() + groups as usize * 9, b'0');
        if digits.len() <= 18 {
            let m = digits
                .iter()
                .fold(0u64, |acc, &c| acc * 10 + u64::from(c - b'0'));
            return Some(small::canon(neg, groups, m));
        }
        Some(Number::from_limbs(
            neg,
            groups,
            Limbs::from_decimal(&digits),
        ))
    }

    fn int_limbs(&self) -> Limbs {
        match &self.mag {
            Mag::Small(m) => Limbs::from_u64(small::int_part(*m, self.scale)),
            Mag::Big(l) => Limbs(l.0.get(self.scale as usize..).unwrap_or(&[]).to_vec()),
        }
    }

    fn frac_string(&self) -> String {
        let s = self.scale as usize;
        let limbs = self.mag.limbs();
        let mut out = String::with_capacity(s * 9);
        for i in (0..s).rev() {
            let limb = limbs.0.get(i).copied().unwrap_or(0);
            out.push_str(&format!("{limb:09}"));
        }
        out.truncate(out.trim_end_matches('0').len());
        out
    }

    pub fn write_xml(&self, output: &mut (impl fmt::Write + ?Sized)) -> fmt::Result {
        fn group(
            output: &mut (impl fmt::Write + ?Sized),
            mut value: u32,
            padded: bool,
            trim: bool,
        ) -> fmt::Result {
            let mut bytes = [b'0'; 9];
            for byte in bytes.iter_mut().rev() {
                *byte = b'0' + (value % 10) as u8;
                value /= 10;
            }
            let start = if padded {
                0
            } else {
                bytes.iter().position(|&b| b != b'0').unwrap_or(8)
            };
            let end = if trim {
                bytes
                    .iter()
                    .rposition(|&b| b != b'0')
                    .map_or(start, |i| i + 1)
            } else {
                9
            };
            let text = std::str::from_utf8(&bytes[start..end]).map_err(|_| fmt::Error)?;
            output.write_str(text)
        }
        let mut small = [0u32; 2];
        let limbs = match &self.mag {
            Mag::Small(magnitude) => {
                small[0] = (magnitude % 1_000_000_000) as u32;
                small[1] = (magnitude / 1_000_000_000) as u32;
                &small[..if small[1] != 0 {
                    2
                } else if small[0] != 0 {
                    1
                } else {
                    0
                }]
            }
            Mag::Big(limbs) => limbs.0.as_slice(),
        };
        if self.neg {
            output.write_char('-')?;
        }
        let scale = self.scale as usize;
        match limbs.get(scale..).unwrap_or(&[]).split_last() {
            Some((top, rest)) => {
                group(output, *top, false, false)?;
                for limb in rest.iter().rev() {
                    group(output, *limb, true, false)?;
                }
            }
            None => output.write_char('0')?,
        }
        if let Some(last) = limbs.iter().take(scale).position(|&limb| limb != 0) {
            output.write_char('.')?;
            for index in (last..scale).rev() {
                group(
                    output,
                    limbs.get(index).copied().unwrap_or(0),
                    true,
                    index == last,
                )?;
            }
        }
        Ok(())
    }

    pub fn to_xml_string(&self) -> String {
        let mut out = String::new();
        if self.neg {
            out.push('-');
        }
        out.push_str(&self.int_digits());
        let frac = self.frac_string();
        if !frac.is_empty() {
            out.push('.');
            out.push_str(&frac);
        }
        out
    }

    pub fn is_zero(&self) -> bool {
        self.mag.is_zero()
    }

    pub fn is_negative(&self) -> bool {
        self.neg
    }

    pub fn is_integer(&self) -> bool {
        self.scale == 0
    }

    pub fn add(&self, o: &Number) -> Number {
        arith::add(self, o)
    }

    pub fn sub(&self, o: &Number) -> Number {
        arith::sub(self, o)
    }

    pub fn mul(&self, o: &Number) -> Number {
        arith::mul(self, o)
    }

    pub fn neg(&self) -> Number {
        Number {
            neg: !self.neg,
            ..self.clone()
        }
        .normalized()
    }

    pub fn abs(&self) -> Number {
        Number {
            neg: false,
            ..self.clone()
        }
    }

    pub fn div(&self, o: &Number) -> Result<Number, DivByZero> {
        div::div(self, o)
    }

    pub fn rem(&self, o: &Number) -> Result<Number, DivByZero> {
        div::rem(self, o)
    }

    pub fn trunc(&self) -> Number {
        div::trunc(self)
    }

    pub fn round(&self, digits: i64, mode: RoundMode) -> Number {
        div::round(self, digits, mode)
    }

    pub fn trunc_to_i64_saturating(&self) -> i64 {
        let magnitude = self.mag.with_limbs(|limbs| {
            limbs
                .get(self.scale as usize..)
                .unwrap_or(&[])
                .iter()
                .rev()
                .try_fold(0u64, |value, &limb| {
                    value
                        .checked_mul(u64::from(crate::limbs::BASE))?
                        .checked_add(u64::from(limb))
                })
        });
        match magnitude {
            Some(value) if self.neg && value <= i64::MIN.unsigned_abs() => {
                (value as i64).wrapping_neg()
            }
            Some(value) if !self.neg && value <= i64::MAX as u64 => value as i64,
            _ if self.neg => i64::MIN,
            _ => i64::MAX,
        }
    }

    pub fn to_f64(&self) -> f64 {
        float::to_f64(self)
    }

    pub fn from_f64_15(v: f64) -> Option<Number> {
        float::from_f64_15(v)
    }

    pub fn to_i64(&self) -> Option<i64> {
        if self.scale != 0 {
            return None;
        }
        let u = self.mag.to_u64()?;
        if self.neg {
            if u <= i64::MIN.unsigned_abs() {
                Some((u as i64).wrapping_neg())
            } else {
                None
            }
        } else {
            i64::try_from(u).ok()
        }
    }

    pub fn int_digits(&self) -> String {
        self.int_limbs().to_decimal()
    }

    pub fn frac_digits(&self) -> String {
        self.frac_string()
    }

    pub fn boost_hash(&self) -> i32 {
        let h = self.mag.with_limbs(|limbs| {
            limbs
                .get(self.scale as usize..)
                .unwrap_or(&[])
                .iter()
                .rev()
                .fold(0u32, |value, &limb| {
                    value.wrapping_mul(crate::limbs::BASE).wrapping_add(limb)
                })
        });
        (if self.neg { h.wrapping_neg() } else { h }) as i32
    }
}

impl PartialEq for Number {
    fn eq(&self, o: &Number) -> bool {
        self.neg == o.neg
            && self.scale == o.scale
            && match (&self.mag, &o.mag) {
                (Mag::Small(a), Mag::Small(b)) => a == b,
                (a, b) => a.with_limbs(|a_limbs| b.with_limbs(|b_limbs| a_limbs == b_limbs)),
            }
    }
}

impl Eq for Number {}

impl Hash for Number {
    fn hash<H: Hasher>(&self, h: &mut H) {
        self.neg.hash(h);
        self.scale.hash(h);
        match self.mag.as_small() {
            Some(m) => m.hash(h),
            None => self.mag.limbs().0.hash(h),
        }
    }
}

impl Ord for Number {
    fn cmp(&self, o: &Number) -> Ordering {
        match (self.neg, o.neg) {
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
            (neg, _) => {
                let c = arith::cmp_abs(self, o);
                if neg { c.reverse() } else { c }
            }
        }
    }
}

impl PartialOrd for Number {
    fn partial_cmp(&self, o: &Number) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

impl fmt::Display for Number {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_xml_string())
    }
}

impl fmt::Debug for Number {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_xml_string())
    }
}

impl From<i32> for Number {
    fn from(v: i32) -> Number {
        Number::from_i64(i64::from(v))
    }
}

impl From<i64> for Number {
    fn from(v: i64) -> Number {
        Number::from_i64(v)
    }
}

impl From<u32> for Number {
    fn from(v: u32) -> Number {
        Number::from_u64(u64::from(v))
    }
}

impl From<u64> for Number {
    fn from(v: u64) -> Number {
        Number::from_u64(v)
    }
}
