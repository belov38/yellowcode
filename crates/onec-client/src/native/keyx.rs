use base64::Engine as _;
use des::TdesEde3;
use des::cipher::{BlockDecrypt, KeyInit};

use super::rsa::{Fill, RsaKey};
use super::stream::{Next, hex};
use super::{BOM, NativeError};

pub const MAX_KEY_BLOCK: usize = 8192;
const DES_BLOCK: usize = 8;

pub const OFFER: char = 'n';

pub fn request(modulus: &[u8], first_call: &str) -> Vec<u8> {
    let b64 = base64::engine::general_purpose::STANDARD;
    let key = format!(
        "\"{OFFER}\",\r\n{{#base64:{}}},\r\n{{#base64:{}}},",
        b64.encode(modulus),
        b64.encode([1u8, 0, 1])
    );
    let head = [BOM, key.as_bytes()].concat();
    let len = u16::try_from(head.len()).unwrap_or(u16::MAX);
    let mut out = len.to_le_bytes().to_vec();
    out.extend_from_slice(&head);
    out.extend_from_slice(BOM);
    out.extend_from_slice(first_call.as_bytes());
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelKey {
    pub level: u8,
    pub key: Vec<u8>,
    pub iv: Vec<u8>,
    pub iv_step: Vec<u8>,
}

pub fn split_answer(answer: &[u8]) -> Result<(Vec<u8>, &[u8]), NativeError> {
    let bad = |m: &str| NativeError::Malformed(format!("key answer: {m}"));
    let len = answer
        .get(..2)
        .map(|b| usize::from(u16::from_le_bytes([b[0], b[1]])))
        .ok_or_else(|| bad("shorter than its length"))?;
    if len > MAX_KEY_BLOCK {
        return Err(bad("key block longer than 8192 bytes"));
    }
    let head = answer
        .get(2..2 + len)
        .ok_or_else(|| bad("shorter than its key block"))?;
    let mut tail = &answer[2 + len..];
    if len == MAX_KEY_BLOCK {
        tail = tail
            .strip_prefix(&[0u8, 0][..])
            .ok_or_else(|| bad("a key block of 8192 bytes without the 00 00 after it"))?;
    }
    let head = head
        .strip_prefix(BOM)
        .ok_or_else(|| bad("key block without BOM"))?;
    let text = std::str::from_utf8(head).map_err(|_| bad("key block is not UTF-8"))?;
    let b64 = base64_items(text);
    let [block] = b64.as_slice() else {
        return Err(bad("expected one {#base64:…} in the key block"));
    };
    Ok((decode_b64(block)?, tail))
}

pub fn is_empty_answer(answer: &[u8]) -> bool {
    answer.len() < 2
}

pub struct EmptyAnswer<'a> {
    pub answer: &'a [u8],
    pub pings: u64,
    pub signals: u64,
    pub last_signal: Option<[u8; 16]>,
    pub after: &'a [Next],
}

pub fn empty_answer_error(e: &EmptyAnswer<'_>) -> NativeError {
    let mut text = format!(
        "key answer: the server answered the key message with {} ({} bytes{}; with it {})",
        if e.answer.is_empty() {
            "an empty message"
        } else {
            "a message shorter than its length"
        },
        e.answer.len(),
        if e.answer.is_empty() {
            String::new()
        } else {
            format!(" {}", hex(e.answer))
        },
        controls(e.pings, e.signals)
    );
    if let Some(id) = e.last_signal.filter(|_| e.signals > 0) {
        text.push_str(&format!(", last signal {}", hex(&id)));
    }
    let after = e.after.iter().map(next_text).collect::<Vec<_>>();
    text.push_str("; after it: ");
    text.push_str(&if after.is_empty() {
        "not read".to_string()
    } else {
        after.join(", then ")
    });
    NativeError::Malformed(text)
}

fn controls(pings: u64, signals: u64) -> String {
    format!(
        "{pings} ping{}, {signals} signal{}",
        if pings == 1 { "" } else { "s" },
        if signals == 1 { "" } else { "s" }
    )
}

const HEAD: usize = 16;
const TEXT_HEAD: usize = 160;

fn next_text(n: &Next) -> String {
    match n {
        Next::Message {
            payload,
            pings,
            signals,
        } => {
            let mut t = format!("a message of {} bytes", payload.len());
            if !payload.is_empty() {
                t.push_str(&format!(
                    " starting {}",
                    hex(&payload[..payload.len().min(HEAD)])
                ));
            }
            if let Some(body) = payload.strip_prefix(BOM) {
                let s = String::from_utf8_lossy(body);
                let s: String = s.chars().take(TEXT_HEAD).collect();
                t.push_str(&format!(" (text {:?})", s.replace("\r\n", " ")));
            }
            if pings + signals > 0 {
                t.push_str(&format!(" with {}", controls(*pings, *signals)));
            }
            t
        }
        Next::Closed => "the connection closed".into(),
        Next::Silent => "nothing within the wait".into(),
        Next::Failed(e) => format!("a read error ({e})"),
    }
}

fn base64_items(text: &str) -> Vec<String> {
    text.split("{#base64:")
        .skip(1)
        .filter_map(|s| s.split_once('}'))
        .map(|(body, _)| body.chars().filter(|c| !c.is_whitespace()).collect())
        .collect()
}

fn decode_b64(s: &str) -> Result<Vec<u8>, NativeError> {
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .map_err(|e| NativeError::Malformed(format!("key answer: bad base64: {e}")))
}

pub struct ClientKey(RsaKey);

impl ClientKey {
    pub fn generate(fill: &mut Fill) -> Result<Self, NativeError> {
        RsaKey::generate(fill).map(Self)
    }

    pub fn modulus(&self) -> Vec<u8> {
        self.0.modulus()
    }

    pub fn open_answer(&self, answer: &[u8]) -> Result<(ChannelKey, Vec<u8>), NativeError> {
        let (block, tail) = split_answer(answer)?;
        let plain = self.0.decrypt_pkcs1(&block)?;
        let channel = parse_key_block(&plain)?;
        let mut reply = des3_cbc_decrypt(&channel.key, &channel.iv, tail)?;
        if let Some(at) = reply.windows(BOM.len()).take(8).position(|w| w == BOM) {
            reply.drain(..at);
        }
        Ok((channel, reply))
    }
}

pub fn parse_key_block(plain: &[u8]) -> Result<ChannelKey, NativeError> {
    let bad = |m: &str| NativeError::Crypto(format!("key block: {m}"));
    let len = plain
        .get(..2)
        .map(|b| usize::from(u16::from_le_bytes([b[0], b[1]])))
        .ok_or_else(|| bad("too short"))?;
    let body = plain
        .get(2..2 + len)
        .ok_or_else(|| bad("shorter than its length"))?;
    let body = body.strip_prefix(BOM).unwrap_or(body);
    let text = std::str::from_utf8(body).map_err(|_| bad("not UTF-8"))?;
    let mut quoted = text.strip_prefix('"').unwrap_or_default().chars();
    let letter = match (quoted.next(), quoted.next()) {
        (Some(l), Some('"')) => l,
        _ => return Err(bad("no quoted level letter")),
    };
    let level = match letter {
        'n' => 0,
        'y' => return Err(NativeError::SecurityLevel(1)),
        'c' => return Err(NativeError::SecurityLevel(2)),
        'u' => return Err(NativeError::KeyRefused),
        other => return Err(bad(&format!("unknown level letter {other:?}"))),
    };
    let items = base64_items(text);
    let [k, iv, step] = items.as_slice() else {
        return Err(bad("expected key, IV and IV step"));
    };
    let (key, iv, iv_step) = (decode_b64(k)?, decode_b64(iv)?, decode_b64(step)?);
    if key.len() != 24 || iv.len() != DES_BLOCK || iv_step.len() != DES_BLOCK {
        return Err(bad(&format!(
            "level 0 wants a 24/8/8-byte key, IV and step, got {}/{}/{}",
            key.len(),
            iv.len(),
            iv_step.len()
        )));
    }
    Ok(ChannelKey {
        level,
        key,
        iv,
        iv_step,
    })
}

pub fn des3_cbc_decrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Result<Vec<u8>, NativeError> {
    let bad = |m: String| NativeError::Crypto(format!("channel cipher: {m}"));
    if data.is_empty() || !data.len().is_multiple_of(DES_BLOCK) {
        return Err(bad(format!(
            "{} bytes is not a whole number of blocks",
            data.len()
        )));
    }
    let cipher = TdesEde3::new_from_slice(key).map_err(|_| bad("bad key length".into()))?;
    let mut prev: [u8; DES_BLOCK] = iv.try_into().map_err(|_| bad("bad IV length".into()))?;
    let mut out = Vec::with_capacity(data.len());
    for chunk in data.as_chunks::<DES_BLOCK>().0 {
        let mut block = des::cipher::generic_array::GenericArray::from(*chunk);
        cipher.decrypt_block(&mut block);
        out.extend(block.iter().zip(prev.iter()).map(|(a, b)| a ^ b));
        prev = *chunk;
    }
    let pad = usize::from(out[out.len() - 1]);
    if pad == 0 || pad > DES_BLOCK {
        return Err(bad(format!("bad padding count {pad}")));
    }
    out.truncate(out.len() - pad);
    Ok(out)
}
