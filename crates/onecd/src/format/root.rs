use super::bytes::{self, OrCorrupt};
use crate::error::{Error, Location, Result};

const LOCALE_LEN: usize = 32;
const LOCALE_LEN_80: usize = 8;
const FIRST_LONG_LOCALE: [u8; 4] = [8, 1, 0, 0];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootContent {
    pub locale: String,
    pub table_refs: Vec<u32>,
}

fn corrupt(what: impl Into<String>) -> Error {
    Error::corrupt(Location::Root, what)
}

pub fn parse_root(version: [u8; 4], content: &[u8]) -> Result<RootContent> {
    let locale_len = if version >= FIRST_LONG_LOCALE {
        LOCALE_LEN
    } else {
        LOCALE_LEN_80
    };
    let trunc = "root content is truncated";
    let raw_locale = bytes::sub(content, 0, locale_len).or_corrupt(|| Location::Root, trunc)?;
    let locale = raw_locale
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| {
            if c.is_ascii() {
                char::from(c)
            } else {
                char::REPLACEMENT_CHARACTER
            }
        })
        .collect();
    let count = bytes::array(content, locale_len)
        .map(i32::from_le_bytes)
        .or_corrupt(|| Location::Root, trunc)?;
    let count =
        usize::try_from(count).map_err(|_| corrupt(format!("negative table count {count}")))?;
    let refs_at = locale_len + 4;
    let table_refs = bytes::u32s_le(content, refs_at, count).or_corrupt(
        || Location::Root,
        "table reference list does not fit the root content",
    )?;
    Ok(RootContent { locale, table_refs })
}

pub fn valid_new_locale(locale: &str) -> bool {
    (1..LOCALE_LEN).contains(&locale.len()) && locale.bytes().all(|b| b.is_ascii_graphic())
}

pub fn encode_root_838(r: &RootContent) -> Vec<u8> {
    let mut out = Vec::with_capacity(LOCALE_LEN + 4 + 4 * r.table_refs.len());
    out.extend(
        r.locale
            .chars()
            .map(|c| {
                if c.is_ascii() && c != '\0' {
                    c as u8
                } else {
                    b'?'
                }
            })
            .take(LOCALE_LEN),
    );
    out.resize(LOCALE_LEN, 0);
    let count = i32::try_from(r.table_refs.len()).unwrap_or(i32::MAX);
    out.extend_from_slice(&count.to_le_bytes());
    for t in &r.table_refs {
        out.extend_from_slice(&t.to_le_bytes());
    }
    out
}

pub fn replace_table_refs_838(content: &[u8], refs: &[u32]) -> Result<Vec<u8>> {
    let old = parse_root(FIRST_LONG_LOCALE, content)?;
    let tail = old
        .table_refs
        .len()
        .checked_mul(4)
        .and_then(|n| n.checked_add(LOCALE_LEN + 4))
        .and_then(|at| content.get(at..))
        .or_corrupt(|| Location::Root, "root content is truncated")?;
    let locale = bytes::sub(content, 0, LOCALE_LEN)
        .or_corrupt(|| Location::Root, "root content is truncated")?;
    let count =
        i32::try_from(refs.len()).map_err(|_| corrupt("too many tables for the root content"))?;
    let mut out = Vec::with_capacity(content.len() + 4 * refs.len());
    out.extend_from_slice(locale);
    out.extend_from_slice(&count.to_le_bytes());
    for r in refs {
        out.extend_from_slice(&r.to_le_bytes());
    }
    out.extend_from_slice(tail);
    Ok(out)
}
