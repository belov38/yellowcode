use std::cmp::Ordering;

pub(crate) const BASE: u32 = 1_000_000_000;
const BASE64: u64 = BASE as u64;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub(crate) struct Limbs(pub(crate) Vec<u32>);

impl Limbs {
    fn normalized(mut v: Vec<u32>) -> Limbs {
        while v.last() == Some(&0) {
            v.pop();
        }
        Limbs(v)
    }

    pub fn zero() -> Limbs {
        Limbs(Vec::new())
    }

    pub fn from_u64(mut v: u64) -> Limbs {
        let mut out = Vec::new();
        while v > 0 {
            out.push((v % BASE64) as u32);
            v /= BASE64;
        }
        Limbs(out)
    }

    pub fn is_zero(&self) -> bool {
        self.0.is_empty()
    }

    pub fn from_decimal(digits: &[u8]) -> Limbs {
        let mut out = Vec::with_capacity(digits.len() / 9 + 1);
        for chunk in digits.rchunks(9) {
            let mut limb = 0u32;
            for &c in chunk {
                limb = limb * 10 + u32::from(c.wrapping_sub(b'0') % 10);
            }
            out.push(limb);
        }
        Limbs::normalized(out)
    }

    pub fn to_decimal(&self) -> String {
        let Some((top, rest)) = self.0.split_last() else {
            return "0".to_string();
        };
        let mut s = top.to_string();
        for limb in rest.iter().rev() {
            s.push_str(&format!("{limb:09}"));
        }
        s
    }

    pub fn cmp_mag(&self, o: &Limbs) -> Ordering {
        self.0
            .len()
            .cmp(&o.0.len())
            .then_with(|| self.0.iter().rev().cmp(o.0.iter().rev()))
    }

    pub fn add(&self, o: &Limbs) -> Limbs {
        let (long, short) = if self.0.len() >= o.0.len() {
            (&self.0, &o.0)
        } else {
            (&o.0, &self.0)
        };
        let mut out = Vec::with_capacity(long.len() + 1);
        let mut carry = 0u32;
        for (i, &a) in long.iter().enumerate() {
            let mut s = a + short.get(i).copied().unwrap_or(0) + carry;
            carry = u32::from(s >= BASE);
            if carry == 1 {
                s -= BASE;
            }
            out.push(s);
        }
        if carry == 1 {
            out.push(1);
        }
        Limbs(out)
    }

    pub fn sub(&self, o: &Limbs) -> Limbs {
        debug_assert!(self.cmp_mag(o) != Ordering::Less);
        let mut out = Vec::with_capacity(self.0.len());
        let mut borrow = 0i64;
        for (i, &a) in self.0.iter().enumerate() {
            let mut t = i64::from(a) - i64::from(o.0.get(i).copied().unwrap_or(0)) - borrow;
            borrow = i64::from(t < 0);
            if t < 0 {
                t += i64::from(BASE);
            }
            out.push(t as u32);
        }
        Limbs::normalized(out)
    }

    pub fn mul(&self, o: &Limbs) -> Limbs {
        if self.is_zero() || o.is_zero() {
            return Limbs::zero();
        }
        let mut out = vec![0u32; self.0.len() + o.0.len()];
        for (i, &a) in self.0.iter().enumerate() {
            let mut carry = 0u64;
            for (j, &b) in o.0.iter().enumerate() {
                let t = u64::from(a) * u64::from(b) + u64::from(out[i + j]) + carry;
                out[i + j] = (t % BASE64) as u32;
                carry = t / BASE64;
            }
            out[i + o.0.len()] = carry as u32;
        }
        Limbs::normalized(out)
    }

    pub fn mul_small(&self, m: u32) -> Limbs {
        let mut out = Vec::with_capacity(self.0.len() + 1);
        let mut carry = 0u64;
        for &a in &self.0 {
            let t = u64::from(a) * u64::from(m) + carry;
            out.push((t % BASE64) as u32);
            carry = t / BASE64;
        }
        while carry > 0 {
            out.push((carry % BASE64) as u32);
            carry /= BASE64;
        }
        Limbs::normalized(out)
    }

    pub fn divmod_small(&self, d: u32) -> (Limbs, u32) {
        if d == 0 {
            return (Limbs::zero(), 0);
        }
        let mut q = vec![0u32; self.0.len()];
        let mut rem = 0u64;
        for (i, &a) in self.0.iter().enumerate().rev() {
            let cur = rem * BASE64 + u64::from(a);
            q[i] = (cur / u64::from(d)) as u32;
            rem = cur % u64::from(d);
        }
        (Limbs::normalized(q), rem as u32)
    }

    pub fn divmod(&self, d: &Limbs) -> (Limbs, Limbs) {
        if d.is_zero() {
            return (Limbs::zero(), Limbs::zero());
        }
        if self.cmp_mag(d) == Ordering::Less {
            return (Limbs::zero(), self.clone());
        }
        if d.0.len() == 1 {
            let (q, r) = self.divmod_small(d.0[0]);
            return (q, Limbs::from_u64(u64::from(r)));
        }
        let n = d.0.len();
        let top = d.0[n - 1];
        let f = BASE / (top + 1);
        let v = d.mul_small(f).0;
        let mut u = self.mul_small(f).0;
        u.resize(self.0.len() + 1, 0);
        let m = u.len() - n - 1;
        let (vt, vs) = (u64::from(v[n - 1]), u64::from(v[n - 2]));
        let mut q = vec![0u32; m + 1];
        for j in (0..=m).rev() {
            let num = u64::from(u[j + n]) * BASE64 + u64::from(u[j + n - 1]);
            let mut qhat = num / vt;
            let mut rhat = num % vt;
            while qhat >= BASE64 || qhat * vs > rhat * BASE64 + u64::from(u[j + n - 2]) {
                qhat -= 1;
                rhat += vt;
                if rhat >= BASE64 {
                    break;
                }
            }
            let mut borrow = 0i64;
            let mut carry = 0u64;
            for i in 0..n {
                let p = qhat * u64::from(v[i]) + carry;
                carry = p / BASE64;
                let mut t = i64::from(u[i + j]) - (p % BASE64) as i64 - borrow;
                borrow = i64::from(t < 0);
                if t < 0 {
                    t += i64::from(BASE);
                }
                u[i + j] = t as u32;
            }
            let mut t = i64::from(u[j + n]) - carry as i64 - borrow;
            if t < 0 {
                t += i64::from(BASE);
                u[j + n] = t as u32;
                qhat -= 1;
                let mut c = 0u32;
                for i in 0..n {
                    let mut s = u[i + j] + v[i] + c;
                    c = u32::from(s >= BASE);
                    if c == 1 {
                        s -= BASE;
                    }
                    u[i + j] = s;
                }
                u[j + n] = (u[j + n] + c) % BASE;
            } else {
                u[j + n] = t as u32;
            }
            q[j] = qhat as u32;
        }
        u.truncate(n);
        let (r, _) = Limbs::normalized(u).divmod_small(f);
        (Limbs::normalized(q), r)
    }

    pub fn shl_limbs(&self, n: usize) -> Limbs {
        if self.is_zero() {
            return Limbs::zero();
        }
        let mut out = vec![0u32; n];
        out.extend_from_slice(&self.0);
        Limbs(out)
    }

    pub fn pow10(n: u32) -> Limbs {
        let mut out = vec![0u32; (n / 9) as usize];
        out.push(10u32.pow(n % 9));
        Limbs(out)
    }

    pub fn decimal_len(&self) -> usize {
        match self.0.last() {
            None => 0,
            Some(top) => (self.0.len() - 1) * 9 + top.to_string().len(),
        }
    }

    pub fn to_u64(&self) -> Option<u64> {
        let mut acc = 0u64;
        for &l in self.0.iter().rev() {
            acc = acc.checked_mul(BASE64)?.checked_add(u64::from(l))?;
        }
        Some(acc)
    }
}
