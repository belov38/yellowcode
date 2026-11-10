use sha2::{Digest, Sha256};
use std::{
    fmt,
    hash::{BuildHasher, Hasher, RandomState},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone)]
pub struct Salt([u8; 16]);

impl Salt {
    pub fn random() -> Self {
        let mut bytes = [0_u8; 16];
        if getrandom::fill(&mut bytes).is_err() {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            for (index, chunk) in bytes.chunks_mut(8).enumerate() {
                let mut hasher = RandomState::new().build_hasher();
                hasher.write_u128(nanos);
                hasher.write_usize(index);
                hasher.write_u32(std::process::id());
                for (slot, byte) in chunk.iter_mut().zip(hasher.finish().to_le_bytes()) {
                    *slot = byte;
                }
            }
        }
        Salt(bytes)
    }

    #[doc(hidden)]
    pub fn from_bytes(bytes: [u8; 16]) -> Self {
        Salt(bytes)
    }
}

impl fmt::Debug for Salt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Salt(..)")
    }
}

pub(crate) fn fold_char(c: char) -> char {
    if c == '\\' {
        '/'
    } else {
        c.to_lowercase().next().unwrap_or(c)
    }
}

pub(crate) fn fold(text: &str) -> String {
    text.chars().map(fold_char).collect()
}

pub(crate) fn hash12(salt: &Salt, text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(salt.0);
    hasher.update(fold(text).as_bytes());
    hasher
        .finalize()
        .iter()
        .take(6)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
