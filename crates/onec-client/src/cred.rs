use aes::Aes256;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use base64::Engine;
use sha1::Sha1;
use sha2::{Digest, Sha256};

const CRED_IV: [u8; 16] = hex16("8e716cb23d487a0628bb6569209a7b9d");
const R1_KEY: [u8; 32] = hex32("a3aeb390acbc1580aebf118dd5629be42e0e07adb134356a831053c5718109cd");
const R1_IV: [u8; 16] = hex16("143a95ee632fd5c0c525dc1f36cc5a8e");
const R1_INC: [u8; 16] = hex16("fe301afcbc4759814ba64577c29298a1");
const EPH_MASK: [u8; 16] = hex16("130af692542dc12e2ec12d5492f60a13");

#[derive(Debug, thiserror::Error)]
pub enum CredError {
    #[error("credAns4 too short: {0} bytes, need at least 49")]
    Ans4TooShort(usize),
}

const fn hex16(s: &str) -> [u8; 16] {
    let b = s.as_bytes();
    let mut out = [0u8; 16];
    let mut i = 0;
    while i < 16 {
        out[i] = (hex_nibble(b[i * 2]) << 4) | hex_nibble(b[i * 2 + 1]);
        i += 1;
    }
    out
}

const fn hex32(s: &str) -> [u8; 32] {
    let b = s.as_bytes();
    let mut out = [0u8; 32];
    let mut i = 0;
    while i < 32 {
        out[i] = (hex_nibble(b[i * 2]) << 4) | hex_nibble(b[i * 2 + 1]);
        i += 1;
    }
    out
}

const fn hex_nibble(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        b'A'..=b'F' => c - b'A' + 10,
        _ => 0,
    }
}

fn aes_cbc_encrypt(key: &[u8; 32], iv: &[u8; 16], data: &[u8]) -> Vec<u8> {
    debug_assert_eq!(data.len() % 16, 0, "CBC input must be block-aligned");
    let cipher = Aes256::new(key.into());
    let mut prev = *iv;
    let mut out = Vec::with_capacity(data.len());
    let (blocks, _) = data.as_chunks::<16>();
    for chunk in blocks {
        let mut block = *chunk;
        for i in 0..16 {
            block[i] ^= prev[i];
        }
        cipher.encrypt_block((&mut block).into());
        prev = block;
        out.extend_from_slice(&block);
    }
    out
}

fn aes_cbc_decrypt(key: &[u8; 32], iv: &[u8; 16], data: &[u8]) -> Vec<u8> {
    debug_assert_eq!(data.len() % 16, 0, "CBC input must be block-aligned");
    let cipher = Aes256::new(key.into());
    let mut prev = *iv;
    let mut out = Vec::with_capacity(data.len());
    let (blocks, _) = data.as_chunks::<16>();
    for chunk in blocks {
        let mut block = *chunk;
        let cipher_block = block;
        cipher.decrypt_block((&mut block).into());
        for i in 0..16 {
            block[i] ^= prev[i];
        }
        prev = cipher_block;
        out.extend_from_slice(&block);
    }
    out
}

fn step_iv(iv: &[u8; 16], inc: &[u8; 16]) -> [u8; 16] {
    let mut out = [0u8; 16];
    let mut carry = 0u16;
    for i in 0..16 {
        let s = iv[i] as u16 + inc[i] as u16 + carry;
        out[i] = (s & 0xFF) as u8;
        carry = s >> 8;
    }
    out
}

pub fn unmask(x: &[u8]) -> Vec<u8> {
    let mut out = x.to_vec();
    for i in 0..16.min(out.len()) {
        out[i] ^= EPH_MASK[i];
    }
    out
}

pub fn pwkey(password: &str, strong: bool) -> [u8; 32] {
    let material = if strong {
        password.to_string()
    } else {
        password.to_uppercase()
    };
    let sha1 = Sha1::digest(material.as_bytes());
    let chsm = base64::engine::general_purpose::STANDARD.encode(sha1);
    let out = Sha256::digest(chsm.as_bytes());
    out.into()
}

fn tail_pad(data: &[u8], tail: &[u8; 16]) -> Vec<u8> {
    let mut n = 16 - (data.len() % 16);
    if n == 0 {
        n = 16;
    }
    let mut out = data.to_vec();
    out.extend_from_slice(&tail[..n - 1]);
    out.push(n as u8);
    out
}

fn crc32_le(data: &[u8]) -> [u8; 4] {
    crc32fast::hash(data).to_le_bytes()
}

pub fn build_cred_req1(username: &str, noise: &[u8; 16], pad_tail: &[u8; 16]) -> Vec<u8> {
    let name = username.as_bytes();
    let mut body = Vec::with_capacity(6 + 16 + name.len() + 4);
    body.push(0x01);
    body.push(0x10);
    body.extend_from_slice(noise);
    body.extend_from_slice(&(name.len() as u32).to_le_bytes());
    body.extend_from_slice(name);
    let crc = crc32_le(&body);
    body.extend_from_slice(&crc);
    let iv = step_iv(&R1_IV, &R1_INC);
    aes_cbc_encrypt(&R1_KEY, &iv, &tail_pad(&body, pad_tail))
}

pub fn build_cred_req3(rc: &[u8; 32], key: &[u8; 32], pad_tail: &[u8; 16]) -> Vec<u8> {
    let erc = aes_cbc_encrypt(key, &CRED_IV, &tail_pad(&unmask(rc), pad_tail));
    let mut head = Vec::with_capacity(2 + erc.len() + 4);
    head.push(0x03);
    head.push(erc.len() as u8);
    head.extend_from_slice(&erc);
    let crc = crc32_le(&head);
    head.extend_from_slice(&crc);
    head
}

pub fn recover_rs(ans4: &[u8], rc: &[u8; 32]) -> Result<[u8; 32], CredError> {
    if ans4.len() < 1 + 48 {
        return Err(CredError::Ans4TooShort(ans4.len()));
    }
    let ers: [u8; 48] = ans4[1..49]
        .try_into()
        .map_err(|_| CredError::Ans4TooShort(ans4.len()))?;
    let dec = aes_cbc_decrypt(rc, &CRED_IV, &ers);
    let unmasked = unmask(&dec[..32]);
    let mut rs = [0u8; 32];
    rs.copy_from_slice(&unmasked[..32]);
    Ok(rs)
}

pub fn build_cred_req5(
    rs: &[u8; 32],
    session_key: &[u8; 32],
    password: &str,
    pad_tail: &[u8; 16],
) -> Vec<u8> {
    let pw = password.as_bytes();
    let mut body = Vec::with_capacity(1 + 32 + 4 + pw.len() + 4);
    body.push(0x20);
    body.extend_from_slice(session_key);
    body.extend_from_slice(&(pw.len() as u32).to_le_bytes());
    body.extend_from_slice(pw);
    let crc = crc32_le(&body);
    body.extend_from_slice(&crc);
    let mut iv_rev = CRED_IV;
    iv_rev.reverse();
    aes_cbc_encrypt(rs, &iv_rev, &tail_pad(&body, pad_tail))
}

pub fn cred_blob(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}
