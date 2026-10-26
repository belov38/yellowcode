use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use sha1::{Digest as _, Sha1};
use subtle::ConstantTimeEq as _;

pub fn sha1_b64(s: &str) -> String {
    STANDARD.encode(Sha1::digest(s.as_bytes()))
}

pub fn upper(s: &str) -> String {
    s.to_uppercase()
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct Hashes {
    pub plain: String,
    pub upper: String,
}

impl fmt::Debug for Hashes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hide = |s: &str| if s.is_empty() { "" } else { "<redacted>" };
        f.debug_struct("Hashes")
            .field("plain", &hide(&self.plain))
            .field("upper", &hide(&self.upper))
            .finish()
    }
}

pub fn hashes(password: &str) -> Hashes {
    Hashes {
        plain: sha1_b64(password),
        upper: sha1_b64(&upper(password)),
    }
}

pub fn stored_value(h: &Hashes) -> String {
    format!("{},{}", h.plain, h.upper)
}

pub fn login_matches(stored_upper: &str, password: &str) -> bool {
    let mut stored = [0u8; 20];
    if stored_upper.len() != 28
        || STANDARD.decode_slice(stored_upper, &mut stored) != Ok(stored.len())
    {
        return false;
    }
    let candidate = Sha1::digest(upper(password).as_bytes());
    bool::from(candidate.as_slice().ct_eq(&stored))
}
