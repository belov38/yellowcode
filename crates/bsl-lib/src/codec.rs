use crate::error::{LibError, msg};
use encoding_rs::{EncoderResult, UTF_8, UTF_16BE, UTF_16LE};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf16Le,
    Utf16Be,
    Utf32Le,
    Utf32Be,
    Single(&'static encoding_rs::Encoding),
}

#[derive(Clone, Copy, Debug)]
pub struct CodePages {
    pub ansi: &'static str,
    pub oem: &'static str,
    pub system: &'static str,
}

impl CodePages {
    pub const STAND: CodePages = CodePages {
        ansi: "windows-1251",
        oem: "IBM866",
        system: "windows-1252",
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncodingName<'a> {
    Ansi,
    Oem,
    Utf8,
    Utf8N,
    Utf16,
    System,
    Named(&'a [u16]),
}

fn bad(name: &[u16]) -> LibError {
    LibError::new(msg::BAD_ENCODING).with_suffix(format!(" '{}'", String::from_utf16_lossy(name)))
}

fn by_label(label: &str) -> Option<Encoding> {
    let e = encoding_rs::Encoding::for_label(label.as_bytes())?;
    if e == UTF_8 {
        Some(Encoding::Utf8)
    } else if e == UTF_16LE {
        Some(Encoding::Utf16Le)
    } else if e == UTF_16BE {
        Some(Encoding::Utf16Be)
    } else if e == encoding_rs::REPLACEMENT {
        None
    } else {
        Some(Encoding::Single(e))
    }
}

pub fn resolve(name: EncodingName<'_>, pages: &CodePages) -> Result<(Encoding, bool), LibError> {
    let page = |l: &str| by_label(l).map(|e| (e, false));
    let r = match name {
        EncodingName::Ansi => page(pages.ansi),
        EncodingName::Oem => page(pages.oem),
        EncodingName::System => page(pages.system),
        EncodingName::Utf8 => Some((Encoding::Utf8, true)),
        EncodingName::Utf16 => Some((Encoding::Utf16Le, true)),
        EncodingName::Utf8N => None,
        EncodingName::Named(n) => {
            let s = String::from_utf16(n).ok().filter(|s| s.is_ascii());
            match s.map(|s| s.to_ascii_lowercase()) {
                None => None,
                Some(s) => match s.as_str() {
                    "utf-8n" | "utf8n" => None,
                    "utf-8" | "utf8" => Some((Encoding::Utf8, true)),
                    "utf-16" | "utf16" => Some((Encoding::Utf16Le, true)),
                    "utf-32" | "utf32" => Some((Encoding::Utf32Le, true)),
                    "utf-32le" => Some((Encoding::Utf32Le, false)),
                    "utf-32be" => Some((Encoding::Utf32Be, false)),
                    l => by_label(l).map(|e| (e, false)),
                },
            }
        }
    };
    r.ok_or_else(|| match name {
        EncodingName::Named(n) => bad(n),
        EncodingName::Utf8N => bad(&"UTF-8N".encode_utf16().collect::<Vec<_>>()),
        _ => LibError::new(msg::BAD_ENCODING),
    })
}

fn units_to_chars(text: &[u16]) -> impl Iterator<Item = Result<char, u16>> + '_ {
    char::decode_utf16(text.iter().copied()).map(|r| r.map_err(|e| e.unpaired_surrogate()))
}

pub fn encode(text: &[u16], enc: Encoding) -> Vec<u8> {
    match enc {
        Encoding::Utf8 => {
            let mut out = Vec::with_capacity(text.len());
            let mut buf = [0u8; 4];
            for c in units_to_chars(text) {
                match c {
                    Ok(c) => out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes()),
                    Err(_) => out.push(b'?'),
                }
            }
            out
        }
        Encoding::Utf16Le | Encoding::Utf16Be => {
            let mut out = Vec::with_capacity(text.len() * 2);
            let mut push = |u: u16| {
                out.extend_from_slice(&if enc == Encoding::Utf16Le {
                    u.to_le_bytes()
                } else {
                    u.to_be_bytes()
                });
            };
            for c in units_to_chars(text) {
                match c {
                    Ok(c) => {
                        let mut b = [0u16; 2];
                        for &u in c.encode_utf16(&mut b).iter() {
                            push(u);
                        }
                    }
                    Err(_) => push(0xFFFD),
                }
            }
            out
        }
        Encoding::Utf32Le | Encoding::Utf32Be => {
            let mut out = Vec::with_capacity(text.len() * 4);
            for c in units_to_chars(text) {
                let v = c.map_or(u32::from(b'?'), u32::from);
                let b = if enc == Encoding::Utf32Le {
                    v.to_le_bytes()
                } else {
                    v.to_be_bytes()
                };
                out.extend_from_slice(&b);
            }
            out
        }
        Encoding::Single(e) => encode_single(text, e),
    }
}

fn encode_single(text: &[u16], e: &'static encoding_rs::Encoding) -> Vec<u8> {
    let mut encoder = e.new_encoder();
    let mut out = Vec::with_capacity(text.len());
    let mut run = String::new();
    let mut flush = |run: &mut String, last: bool, out: &mut Vec<u8>| {
        let mut src: &str = run.as_str();
        let mut buf = [0u8; 256];
        loop {
            let (res, read, written) =
                encoder.encode_from_utf8_without_replacement(src, &mut buf, last);
            out.extend_from_slice(buf.get(..written).unwrap_or(&[]));
            src = src.get(read..).unwrap_or("");
            match res {
                EncoderResult::InputEmpty => break,
                EncoderResult::OutputFull => {}
                EncoderResult::Unmappable(_) => {
                    out.push(b'?');
                }
            }
        }
        run.clear();
    };
    for c in units_to_chars(text) {
        match c {
            Ok(c) => run.push(c),
            Err(_) => {
                flush(&mut run, false, &mut out);
                out.push(b'?');
            }
        }
    }
    flush(&mut run, true, &mut out);
    out
}

pub fn decode(bytes: &[u8], enc: Encoding) -> Vec<u16> {
    match enc {
        Encoding::Utf8 => UTF_8
            .decode_without_bom_handling(bytes)
            .0
            .encode_utf16()
            .collect(),
        Encoding::Utf16Le => UTF_16LE
            .decode_without_bom_handling(bytes)
            .0
            .encode_utf16()
            .collect(),
        Encoding::Utf16Be => UTF_16BE
            .decode_without_bom_handling(bytes)
            .0
            .encode_utf16()
            .collect(),
        Encoding::Single(e) => e
            .decode_without_bom_handling(bytes)
            .0
            .encode_utf16()
            .collect(),
        Encoding::Utf32Le | Encoding::Utf32Be => {
            let mut out = Vec::with_capacity(bytes.len() / 4 + 1);
            let (chunks, remainder) = bytes.as_chunks::<4>();
            for c in chunks {
                let a = [c[0], c[1], c[2], c[3]];
                let v = if enc == Encoding::Utf32Le {
                    u32::from_le_bytes(a)
                } else {
                    u32::from_be_bytes(a)
                };
                let ch = char::from_u32(v).unwrap_or('\u{FFFD}');
                let mut b = [0u16; 2];
                out.extend_from_slice(ch.encode_utf16(&mut b));
            }
            if !remainder.is_empty() {
                out.push(0xFFFD);
            }
            out
        }
    }
}

pub fn bom(enc: Encoding) -> &'static [u8] {
    match enc {
        Encoding::Utf8 => &[0xEF, 0xBB, 0xBF],
        Encoding::Utf16Le => &[0xFF, 0xFE],
        Encoding::Utf16Be => &[0xFE, 0xFF],
        Encoding::Utf32Le => &[0xFF, 0xFE, 0, 0],
        Encoding::Utf32Be => &[0, 0, 0xFE, 0xFF],
        Encoding::Single(_) => &[],
    }
}

pub fn detect_bom(bytes: &[u8]) -> Option<(Encoding, usize)> {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        Some((Encoding::Utf8, 3))
    } else if bytes.starts_with(&[0xFF, 0xFE]) {
        Some((Encoding::Utf16Le, 2))
    } else if bytes.starts_with(&[0xFE, 0xFF]) {
        Some((Encoding::Utf16Be, 2))
    } else {
        None
    }
}

pub fn string_to_bytes(s: &[u16], enc: Encoding, with_bom: bool) -> Vec<u8> {
    let mut out = if with_bom {
        bom(enc).to_vec()
    } else {
        Vec::new()
    };
    out.extend(encode(s, enc));
    out
}

pub fn bytes_to_string(b: &[u8], enc: Encoding) -> Vec<u16> {
    if matches!(enc, Encoding::Utf32Le | Encoding::Utf32Be) {
        let own = bom(enc);
        if let Some(rest) = b.strip_prefix(own) {
            return decode(rest, enc);
        }
    }
    match detect_bom(b) {
        Some((e, n)) => decode(b.get(n..).unwrap_or(&[]), e),
        None => decode(b, enc),
    }
}
