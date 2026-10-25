use std::fmt;

use crate::Error;

const BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];

#[derive(Clone, PartialEq, Eq)]
pub struct Obfuscated {
    pub key: Vec<u8>,
    pub text: String,
}

impl fmt::Debug for Obfuscated {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Obfuscated")
            .field("key_len", &self.key.len())
            .field("text_len", &self.text.len())
            .finish()
    }
}

fn xor(payload: &[u8], key: &[u8]) -> Vec<u8> {
    payload
        .iter()
        .zip(key.iter().cycle())
        .map(|(p, k)| p ^ k)
        .collect()
}

pub fn decode(blob: &[u8]) -> Result<Obfuscated, Error> {
    if blob.len() > crate::MAX_DATA_BYTES {
        return Err(Error::Limit);
    }
    let (&n, rest) = blob.split_first().ok_or(Error::Empty)?;
    let n = usize::from(n);
    if n == 0 {
        return Err(Error::EmptyKey);
    }
    let (key, payload) = rest.split_at_checked(n).ok_or(Error::KeyTooLong {
        key: n,
        have: rest.len(),
    })?;
    let plain = xor(payload, key);
    let body = plain.strip_prefix(&BOM[..]).ok_or(Error::NoBom)?;
    let text = std::str::from_utf8(body).map_err(|_| Error::NotUtf8)?;
    Ok(Obfuscated {
        key: key.to_vec(),
        text: text.to_owned(),
    })
}

pub fn encode(text: &str, key: &[u8]) -> Result<Vec<u8>, Error> {
    let n = u8::try_from(key.len())
        .ok()
        .filter(|&n| n != 0)
        .ok_or(Error::BadKeyLength(key.len()))?;
    let mut plain = Vec::with_capacity(BOM.len() + text.len());
    plain.extend_from_slice(&BOM);
    plain.extend_from_slice(text.as_bytes());
    let mut out = Vec::with_capacity(1 + key.len() + plain.len());
    out.push(n);
    out.extend_from_slice(key);
    out.extend(xor(&plain, key));
    Ok(out)
}

pub fn new_key() -> Vec<u8> {
    let mut k = vec![0u8; 16];
    rand::fill(&mut k[..]);
    k
}
