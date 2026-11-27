use num_bigint::BigUint;

use super::NativeError;

pub(crate) const MODULUS_BYTES: usize = 128;
const PRIME_BYTES: usize = MODULUS_BYTES / 2;
const MR_ROUNDS: usize = 20;
const MAX_CANDIDATES: usize = 100_000;
const MAX_PAIRS: usize = 16;
pub(crate) const E: u32 = 65537;

pub(crate) type Fill<'a> = dyn FnMut(&mut [u8]) -> Result<(), NativeError> + 'a;

pub(crate) struct RsaKey {
    n: BigUint,
    d: BigUint,
}

impl RsaKey {
    pub(crate) fn generate(fill: &mut Fill) -> Result<Self, NativeError> {
        let e = BigUint::from(E);
        let one = BigUint::from(1u32);
        let small = small_primes();
        for _ in 0..MAX_PAIRS {
            let p = random_prime(fill, &small)?;
            let q = random_prime(fill, &small)?;
            if p == q {
                continue;
            }
            let n = &p * &q;
            let phi = (&p - &one) * (&q - &one);
            if let Some(d) = e.modinv(&phi) {
                return Ok(Self { n, d });
            }
        }
        Err(NativeError::Crypto(
            "no usable prime pair for the RSA key (random source?)".into(),
        ))
    }

    pub(crate) fn modulus(&self) -> Vec<u8> {
        left_pad(&self.n.to_bytes_be(), MODULUS_BYTES)
    }

    pub(crate) fn decrypt_pkcs1(&self, block: &[u8]) -> Result<Vec<u8>, NativeError> {
        if block.len() != MODULUS_BYTES {
            return Err(NativeError::Crypto(format!(
                "RSA block of {} bytes, expected {MODULUS_BYTES}",
                block.len()
            )));
        }
        let c = BigUint::from_bytes_be(block);
        if c >= self.n {
            return Err(NativeError::Crypto(
                "RSA block not below the modulus".into(),
            ));
        }
        let em = left_pad(&c.modpow(&self.d, &self.n).to_bytes_be(), MODULUS_BYTES);
        let bad = || NativeError::Crypto("bad PKCS#1 v1.5 padding".into());
        if em[0] != 0 || em[1] != 2 {
            return Err(bad());
        }
        let sep = em[2..].iter().position(|&b| b == 0).ok_or_else(bad)? + 2;
        if sep < 10 {
            return Err(bad());
        }
        Ok(em[sep + 1..].to_vec())
    }
}

fn left_pad(b: &[u8], len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len.saturating_sub(b.len())];
    out.extend_from_slice(b);
    out
}

fn small_primes() -> Vec<u32> {
    (3u32..2000)
        .step_by(2)
        .filter(|n| {
            (3..)
                .step_by(2)
                .take_while(|d| d * d <= *n)
                .all(|d| n % d != 0)
        })
        .collect()
}

fn random_prime(fill: &mut Fill, small: &[u32]) -> Result<BigUint, NativeError> {
    let mut buf = [0u8; PRIME_BYTES];
    for _ in 0..MAX_CANDIDATES {
        fill(&mut buf)?;
        buf[0] |= 0xC0;
        buf[PRIME_BYTES - 1] |= 1;
        let cand = BigUint::from_bytes_be(&buf);
        if small.iter().any(|&p| (&cand % p) == BigUint::ZERO) {
            continue;
        }
        if is_probable_prime(&cand, fill)? {
            return Ok(cand);
        }
    }
    Err(NativeError::Crypto(
        "no prime found for the RSA key (random source?)".into(),
    ))
}

fn is_probable_prime(n: &BigUint, fill: &mut Fill) -> Result<bool, NativeError> {
    let one = BigUint::from(1u32);
    let two = BigUint::from(2u32);
    let n1 = n - &one;
    let s = n1.trailing_zeros().unwrap_or(0);
    let d = &n1 >> s;
    let mut buf = vec![0u8; PRIME_BYTES];
    for _ in 0..MR_ROUNDS {
        fill(&mut buf)?;
        let a = BigUint::from_bytes_be(&buf) % (n - 3u32) + &two;
        let mut x = a.modpow(&d, n);
        if x == one || x == n1 {
            continue;
        }
        let mut witness = true;
        for _ in 1..s {
            x = x.modpow(&two, n);
            if x == n1 {
                witness = false;
                break;
            }
        }
        if witness {
            return Ok(false);
        }
    }
    Ok(true)
}
