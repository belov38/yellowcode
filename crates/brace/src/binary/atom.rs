use std::io::{self, Read};

use super::read::Src;
use super::{
    BASE_MASK, ErrorKind, T_BSTR8, T_BSTR64, T_DIGIT0, T_DIGIT9, T_F32, T_F64, T_GUID, T_NEG_U8,
    T_NEG_U16, T_NEG_U32, T_NEG_U64, T_NUMERIC, T_U8, T_U16, T_U32, T_U64, T_WSTR8, T_WSTR64,
};
use crate::Guid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AtomValue {
    Int(i128),
    F32(u32),
    F64(u64),
    Guid(Guid),
    Numeric(Box<Atom>),
    Wide(Vec<u16>),
    Narrow(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Atom {
    tag: u8,
    value: AtomValue,
}

impl Atom {
    pub fn tag(&self) -> u8 {
        self.tag
    }

    pub fn value(&self) -> &AtomValue {
        &self.value
    }

    pub fn int(v: i128) -> Atom {
        let Ok(m) = u64::try_from(v.unsigned_abs()) else {
            return Atom::numeric(&v.to_string());
        };
        let tag = match (v < 0, m) {
            (false, 0..10) => T_DIGIT0.saturating_add(u8::try_from(m).unwrap_or(0)),
            (false, 10..0x100) => T_U8,
            (false, 0x100..0x1_0000) => T_U16,
            (false, 0x1_0000..0x1_0000_0000) => T_U32,
            (false, _) => T_U64,
            (true, 0..0x100) => T_NEG_U8,
            (true, 0x100..0x1_0000) => T_NEG_U16,
            (true, 0x1_0000..0x1_0000_0000) => T_NEG_U32,
            (true, _) => T_NEG_U64,
        };
        Atom::of_int(tag, v)
    }

    pub fn str(s: &str) -> Atom {
        match s.chars().map(|c| u8::try_from(c).ok()).collect() {
            Some(latin1) => Atom::narrow(latin1),
            None => Atom::wide(s.encode_utf16().collect()),
        }
    }

    pub fn utf16(u: &[u16]) -> Atom {
        match u.iter().map(|&c| u8::try_from(c).ok()).collect() {
            Some(latin1) => Atom::narrow(latin1),
            None => Atom::wide(u.to_vec()),
        }
    }

    pub fn bytes(b: &[u8]) -> Atom {
        Atom::narrow(b.to_vec())
    }

    pub fn guid(g: Guid) -> Atom {
        if g.is_nil() {
            Atom::of_int(T_DIGIT0, 0)
        } else {
            Atom {
                tag: T_GUID,
                value: AtomValue::Guid(g),
            }
        }
    }

    pub fn numeric_text(t: &str) -> Option<Atom> {
        let (neg, body) = match t.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, t.strip_prefix('+').unwrap_or(t)),
        };
        let (int_part, frac) = body.split_once('.').unwrap_or((body, ""));
        let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
        if !digits(int_part) || !digits(frac) || int_part.len().saturating_add(frac.len()) == 0 {
            return None;
        }
        let int_part = match int_part.trim_start_matches('0') {
            "" => "0",
            rest => rest,
        };
        let frac = frac.trim_end_matches('0');
        if frac.is_empty() && int_part.len() <= 18 {
            let m: i128 = int_part.parse().ok()?;
            return Some(Atom::int(if neg { -m } else { m }));
        }
        let mut text = String::with_capacity(t.len());
        if neg {
            text.push('-');
        }
        text.push_str(int_part);
        if !frac.is_empty() {
            text.push('.');
            text.push_str(frac);
        }
        Some(Atom::numeric(&text))
    }

    pub fn f32(v: f32) -> Atom {
        Atom {
            tag: T_F32,
            value: AtomValue::F32(v.to_bits()),
        }
    }

    pub fn f64(v: f64) -> Atom {
        Atom {
            tag: T_F64,
            value: AtomValue::F64(v.to_bits()),
        }
    }

    fn of_int(tag: u8, v: i128) -> Atom {
        Atom {
            tag,
            value: AtomValue::Int(v),
        }
    }

    fn numeric(text: &str) -> Atom {
        Atom {
            tag: T_NUMERIC,
            value: AtomValue::Numeric(Box::new(Atom::str(text))),
        }
    }

    fn narrow(b: Vec<u8>) -> Atom {
        match *b.as_slice() {
            [] => Atom::of_int(T_DIGIT0, 0),
            [x] => Atom::of_int(T_U8, i128::from(x)),
            _ => Atom {
                tag: sized_tag(T_BSTR8, b.len()),
                value: AtomValue::Narrow(b),
            },
        }
    }

    fn wide(u: Vec<u16>) -> Atom {
        Atom {
            tag: sized_tag(T_WSTR8, u.len()),
            value: AtomValue::Wide(u),
        }
    }

    pub fn as_int(&self) -> Option<i128> {
        match (self.tag, &self.value) {
            (T_DIGIT0..=T_NEG_U64, AtomValue::Int(v)) => Some(*v),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self.as_int()? {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<String> {
        match &self.value {
            AtomValue::Wide(u) => String::from_utf16(u).ok(),
            AtomValue::Narrow(b) => Some(b.iter().map(|&c| char::from(c)).collect()),
            _ => self
                .as_byte()
                .map(|b| b.map(char::from).into_iter().collect()),
        }
    }

    pub fn as_utf16(&self) -> Option<Vec<u16>> {
        match &self.value {
            AtomValue::Wide(u) => Some(u.clone()),
            AtomValue::Narrow(b) => Some(b.iter().map(|&c| u16::from(c)).collect()),
            _ => self
                .as_byte()
                .map(|b| b.map(u16::from).into_iter().collect()),
        }
    }

    pub fn as_bytes(&self) -> Option<Vec<u8>> {
        match &self.value {
            AtomValue::Narrow(b) => Some(b.clone()),
            _ => self.as_byte().map(|b| b.into_iter().collect()),
        }
    }

    pub fn as_guid(&self) -> Option<Guid> {
        match (self.tag, &self.value) {
            (T_DIGIT0, _) => Some(Guid::NIL),
            (_, AtomValue::Guid(g)) => Some(*g),
            _ => None,
        }
    }

    pub fn as_numeric_text(&self) -> Option<&str> {
        let AtomValue::Numeric(inner) = &self.value else {
            return None;
        };
        match (inner.tag, &inner.value) {
            (T_DIGIT0, _) => Some(""),
            (_, AtomValue::Narrow(b)) => std::str::from_utf8(b).ok(),
            _ => None,
        }
    }

    pub fn as_decimal(&self) -> Option<String> {
        match &self.value {
            AtomValue::Numeric(inner) => inner.as_str(),
            _ => self.as_int().map(|v| v.to_string()),
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self.value {
            AtomValue::F32(bits) => Some(f64::from(f32::from_bits(bits))),
            AtomValue::F64(bits) => Some(f64::from_bits(bits)),
            _ => None,
        }
    }

    fn as_byte(&self) -> Option<Option<u8>> {
        match (self.tag, &self.value) {
            (T_DIGIT0, _) => Some(None),
            (T_U8, AtomValue::Int(v)) => u8::try_from(*v).ok().map(Some),
            _ => None,
        }
    }

    pub(super) fn read<R: Read>(src: &mut Src<R>, base: u8, skip: bool) -> Result<Atom, ErrorKind> {
        let value = match base {
            T_DIGIT0..=T_DIGIT9 => AtomValue::Int(i128::from(base.saturating_sub(T_DIGIT0))),
            T_U8 => AtomValue::Int(i128::from(u8::from_le_bytes(fixed(src)?))),
            T_NEG_U8 => AtomValue::Int(-i128::from(u8::from_le_bytes(fixed(src)?))),
            T_U16 => AtomValue::Int(i128::from(u16::from_le_bytes(fixed(src)?))),
            T_NEG_U16 => AtomValue::Int(-i128::from(u16::from_le_bytes(fixed(src)?))),
            T_U32 => AtomValue::Int(i128::from(u32::from_le_bytes(fixed(src)?))),
            T_NEG_U32 => AtomValue::Int(-i128::from(u32::from_le_bytes(fixed(src)?))),
            T_U64 => AtomValue::Int(i128::from(u64::from_le_bytes(fixed(src)?))),
            T_NEG_U64 => AtomValue::Int(-i128::from(u64::from_le_bytes(fixed(src)?))),
            T_F32 => AtomValue::F32(u32::from_le_bytes(fixed(src)?)),
            T_F64 => AtomValue::F64(u64::from_le_bytes(fixed(src)?)),
            T_GUID => AtomValue::Guid(Guid::from_bytes_le(fixed(src)?)),
            T_NUMERIC => {
                let b = src.byte().map_err(io)?.ok_or(ErrorKind::UnexpectedEof)?;
                if b & !BASE_MASK != 0 || !matches!(b, T_DIGIT0 | T_U8 | T_WSTR8..=T_BSTR64) {
                    return Err(ErrorKind::BadNumericTag(b));
                }
                AtomValue::Numeric(Box::new(Atom::read(src, b, skip)?))
            }
            T_WSTR8..=T_WSTR64 => {
                let n = length(src, base.saturating_sub(T_WSTR8))?;
                src.charge_wide(n)?;
                AtomValue::Wide(read_wide(src, n, skip)?)
            }
            T_BSTR8..=T_BSTR64 => {
                let n = length(src, base.saturating_sub(T_BSTR8))?;
                src.charge_payload(n)?;
                AtomValue::Narrow(read_narrow(src, n, skip)?)
            }
            _ => return Err(ErrorKind::UnknownTag(base)),
        };
        Ok(Atom { tag: base, value })
    }
}

fn sized_tag(base: u8, len: usize) -> u8 {
    match len {
        0..0x100 => base,
        0x100..0x1_0000 => base.saturating_add(1),
        _ => base.saturating_add(2),
    }
}

fn io(e: io::Error) -> ErrorKind {
    ErrorKind::Io(e.kind())
}

fn fixed<R: Read, const N: usize>(src: &mut Src<R>) -> Result<[u8; N], ErrorKind> {
    src.fixed().map_err(io)?.ok_or(ErrorKind::UnexpectedEof)
}

fn length<R: Read>(src: &mut Src<R>, class: u8) -> Result<u64, ErrorKind> {
    Ok(match class {
        0 => u64::from(u8::from_le_bytes(fixed(src)?)),
        1 => u64::from(u16::from_le_bytes(fixed(src)?)),
        _ => u64::from_le_bytes(fixed(src)?),
    })
}

fn take<R: Read>(src: &mut Src<R>, n: u64, sink: impl FnMut(&[u8])) -> Result<(), ErrorKind> {
    if src.payload(n, sink).map_err(io)? {
        Ok(())
    } else {
        Err(ErrorKind::UnexpectedEof)
    }
}

fn fits(n: Option<u64>) -> Result<usize, ErrorKind> {
    n.and_then(|n| usize::try_from(n).ok())
        .filter(|&n| isize::try_from(n).is_ok())
        .ok_or(ErrorKind::UnexpectedEof)
}

fn reserve_capped<T>(v: &mut Vec<T>, add: usize, total: usize) {
    let need = v.len().saturating_add(add);
    if need > v.capacity() {
        let target = v.len().saturating_mul(2).min(total).max(need);
        v.reserve_exact(target.saturating_sub(v.len()));
    }
}

fn read_narrow<R: Read>(src: &mut Src<R>, n: u64, skip: bool) -> Result<Vec<u8>, ErrorKind> {
    let mut out = Vec::new();
    if skip {
        take(src, n, |_| {})?;
    } else {
        let total = fits(Some(n))?;
        take(src, n, |s| {
            reserve_capped(&mut out, s.len(), total);
            out.extend_from_slice(s);
        })?;
    }
    Ok(out)
}

fn read_wide<R: Read>(src: &mut Src<R>, n: u64, skip: bool) -> Result<Vec<u16>, ErrorKind> {
    let mut out = Vec::new();
    if skip {
        take(src, n, |_| {})?;
        take(src, n, |_| {})?;
        return Ok(out);
    }
    let total = fits(n.checked_mul(2))? / 2;
    let mut carry: Option<u8> = None;
    take(src, n.saturating_mul(2), |mut s| {
        let mut joined = None;
        if let (Some(lo), Some((&hi, rest))) = (carry, s.split_first()) {
            joined = Some(u16::from_le_bytes([lo, hi]));
            carry = None;
            s = rest;
        }
        let (pairs, tail) = s.as_chunks::<2>();
        reserve_capped(
            &mut out,
            pairs.len().saturating_add(usize::from(joined.is_some())),
            total,
        );
        out.extend(joined);
        out.extend(pairs.iter().map(|&p| u16::from_le_bytes(p)));
        if let Some(&b) = tail.first() {
            carry = Some(b);
        }
    })?;
    Ok(out)
}
