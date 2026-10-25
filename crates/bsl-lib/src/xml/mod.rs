mod decode;
mod reader;
mod writer;

pub use decode::{decode_xml_bytes, decode_xml_bytes_partial};
pub use reader::{XmlAttr, XmlError, XmlEvent, XmlReader};
pub(crate) use reader::{is_name_char, is_name_start};
pub use writer::{XmlWriteError, XmlWriteOptions, XmlWriter};

use bsl_prim::{BslDate, Number};

fn is_high(u: u16) -> bool {
    (0xD800..0xDC00).contains(&u)
}
fn is_low(u: u16) -> bool {
    (0xDC00..0xE000).contains(&u)
}

fn bmp_valid(u: u16, xml11: bool) -> bool {
    match u {
        0x09 | 0x0A | 0x0D => true,
        0x01..=0x08 | 0x0B | 0x0C | 0x0E..=0x1F => xml11,
        0x20..=0xD7FF | 0xE000..=0xFFFD => true,
        _ => false,
    }
}

fn valid_len(s: &[u16], i: usize, xml11: bool) -> usize {
    let u = s[i];
    if is_high(u) {
        return if s.get(i + 1).is_some_and(|&n| is_low(n)) {
            2
        } else {
            0
        };
    }
    usize::from(bmp_valid(u, xml11))
}

pub fn xml_invalid_pos(s: &[u16], xml11: bool) -> i64 {
    let mut i = 0;
    while i < s.len() {
        match valid_len(s, i, xml11) {
            0 => return i as i64 + 1,
            n => i += n,
        }
    }
    0
}

pub fn xml_remove_invalid(s: &[u16], xml11: bool) -> Vec<u16> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        match valid_len(s, i, xml11) {
            0 => i += 1,
            n => {
                out.extend_from_slice(&s[i..i + n]);
                i += n;
            }
        }
    }
    out
}

fn xml_trim(s: &[u16]) -> &[u16] {
    let ws = |u: &u16| matches!(u, 0x20 | 0x09 | 0x0D | 0x0A);
    let start = s.iter().position(|u| !ws(u)).unwrap_or(s.len());
    let end = s.iter().rposition(|u| !ws(u)).map_or(start, |i| i + 1);
    s.get(start..end).unwrap_or(&[])
}

fn is_digit(u: u16) -> bool {
    (0x30..=0x39).contains(&u)
}

pub fn xml_number(s: &[u16]) -> Option<Number> {
    let s = xml_trim(s);
    let mut i = 0;
    if s.get(i) == Some(&0x2D) {
        i += 1;
    }
    let d0 = i;
    while s.get(i).is_some_and(|&u| is_digit(u)) {
        i += 1;
    }
    if i == d0 {
        return None;
    }
    if s.get(i) == Some(&0x2E) && s.get(i + 1).is_some_and(|&u| is_digit(u)) {
        i += 1;
        while s.get(i).is_some_and(|&u| is_digit(u)) {
            i += 1;
        }
    }
    let text: String = s[..i].iter().map(|&u| u as u8 as char).collect();
    Number::parse(&text)
}

pub fn xml_bool(s: &[u16]) -> Option<bool> {
    let t = String::from_utf16(xml_trim(s)).ok()?;
    match t.as_str() {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct XmlDate {
    pub date: BslDate,
    pub offset: Option<i32>,
}

fn num(s: &[u16], from: usize, n: usize) -> Option<u32> {
    let part = s.get(from..from.checked_add(n)?)?;
    let mut v = 0u32;
    for &u in part {
        if !is_digit(u) {
            return None;
        }
        v = v * 10 + u32::from(u - 0x30);
    }
    Some(v)
}

fn ch(s: &[u16], i: usize, c: char) -> bool {
    s.get(i) == Some(&(c as u16))
}

pub fn xml_date(s: &[u16]) -> Option<XmlDate> {
    let s = xml_trim(s);
    let y = num(s, 0, 4)?;
    if !ch(s, 4, '-') || !ch(s, 7, '-') {
        return None;
    }
    let mo = num(s, 5, 2)?;
    let d = num(s, 8, 2)?;
    let mut i = 10;
    let (mut h, mut mi, mut sec, mut round) = (0, 0, 0, false);
    if ch(s, i, 'T') {
        if !ch(s, i + 3, ':') || !ch(s, i + 6, ':') {
            return None;
        }
        h = num(s, i + 1, 2)?;
        mi = num(s, i + 4, 2)?;
        sec = num(s, i + 7, 2)?;
        i += 9;
        if ch(s, i, '.') {
            let n = s[i + 1..].iter().take_while(|&&u| is_digit(u)).count();
            if n == 0 {
                return None;
            }
            round = s[i + 1] >= 0x35;
            i += 1 + n;
        }
    }
    if h > 23 || mi > 59 || sec > 59 {
        return None;
    }
    let offset = if i == s.len() {
        None
    } else if ch(s, i, 'Z') && i + 1 == s.len() {
        Some(0)
    } else if (ch(s, i, '+') || ch(s, i, '-')) && i + 6 == s.len() && ch(s, i + 3, ':') {
        let oh = num(s, i + 1, 2)?;
        let om = num(s, i + 4, 2)?;
        if oh > 14 || om > 59 {
            return None;
        }
        let secs = (oh * 3600 + om * 60) as i32;
        Some(if ch(s, i, '-') { -secs } else { secs })
    } else {
        return None;
    };
    let mut date = BslDate::from_parts(y, mo, d, h, mi, sec)?;
    if round {
        date = date.add_seconds(&Number::parse("1")?);
    }
    Some(XmlDate { date, offset })
}
