use super::reader::XmlError;
use crate::codec::{self, CodePages, Encoding, EncodingName};

fn fatal(message: String) -> XmlError {
    XmlError {
        message,
        line: 1,
        col: 1,
        fatal: true,
    }
}

fn position_after(prefix: &str) -> (u32, u32) {
    let (mut line, mut col) = (1u32, 1u32);
    let mut it = prefix.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\n' || (c == '\r' && it.peek() != Some(&'\n')) {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

fn declared_encoding(bytes: &[u8]) -> Option<String> {
    if !bytes.starts_with(b"<?xml") {
        return None;
    }
    let end = bytes.windows(2).position(|w| w == b"?>")?;
    let decl = &bytes[..end];
    let at = decl.windows(8).position(|w| w == b"encoding")?;
    let mut i = at + 8;
    let skip_ws = |i: &mut usize| {
        while decl.get(*i).is_some_and(|b| b.is_ascii_whitespace()) {
            *i += 1;
        }
    };
    skip_ws(&mut i);
    if decl.get(i) != Some(&b'=') {
        return None;
    }
    i += 1;
    skip_ws(&mut i);
    let q = *decl.get(i).filter(|b| **b == b'"' || **b == b'\'')?;
    let rest = decl.get(i + 1..)?;
    let len = rest.iter().position(|b| *b == q)?;
    Some(String::from_utf8_lossy(&rest[..len]).into_owned())
}

pub fn decode_xml_bytes(bytes: &[u8]) -> Result<String, XmlError> {
    match decode_xml_bytes_partial(bytes) {
        (text, None) => Ok(text),
        (_, Some(e)) => Err(e),
    }
}

pub fn decode_xml_bytes_partial(bytes: &[u8]) -> (String, Option<XmlError>) {
    let (enc, body) = if let Some((enc, n)) = codec::detect_bom(bytes) {
        (enc, &bytes[n..])
    } else if bytes.starts_with(&[0x3C, 0, 0x3F, 0]) {
        (Encoding::Utf16Le, bytes)
    } else if bytes.starts_with(&[0, 0x3C, 0, 0x3F]) {
        (Encoding::Utf16Be, bytes)
    } else if let Some(name) = declared_encoding(bytes) {
        let u: Vec<u16> = name.encode_utf16().collect();
        match codec::resolve(EncodingName::Named(&u), &CodePages::STAND) {
            Ok((
                Encoding::Utf16Le | Encoding::Utf16Be | Encoding::Utf32Le | Encoding::Utf32Be,
                _,
            )) => (Encoding::Utf8, bytes),
            Ok((enc, _)) => (enc, bytes),
            Err(_) => {
                return (
                    String::new(),
                    Some(fatal(format!("Unsupported encoding {name}"))),
                );
            }
        }
    } else {
        (Encoding::Utf8, bytes)
    };
    decode_strict(body, enc)
}

fn malformed(name: &str, text: &str, body: &[u8], at: usize) -> XmlError {
    let (line, col) = position_after(text);
    let bytes: Vec<String> = body
        .iter()
        .skip(at)
        .take(4)
        .map(|b| format!("0x{b:02X}"))
        .collect();
    XmlError {
        message: format!(
            "Input is not proper {name}, indicate encoding !\nBytes: {}",
            bytes.join(" ")
        ),
        line,
        col,
        fatal: true,
    }
}

fn decode_strict(body: &[u8], enc: Encoding) -> (String, Option<XmlError>) {
    match enc {
        Encoding::Utf8 => match std::str::from_utf8(body) {
            Ok(t) => (t.to_string(), None),
            Err(e) => {
                let ok = e.valid_up_to();
                let text = String::from_utf8_lossy(&body[..ok]).into_owned();
                let err = malformed("UTF-8", &text, body, ok);
                (text, Some(err))
            }
        },
        Encoding::Utf16Le | Encoding::Utf16Be => {
            let be = enc == Encoding::Utf16Be;
            let units: Vec<u16> = body
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&a| {
                    if be {
                        u16::from_be_bytes(a)
                    } else {
                        u16::from_le_bytes(a)
                    }
                })
                .collect();
            let mut text = String::new();
            let mut at = 0usize;
            for r in char::decode_utf16(units.iter().copied()) {
                match r {
                    Ok(c) => {
                        text.push(c);
                        at += c.len_utf16() * 2;
                    }
                    Err(_) => return (text.clone(), Some(malformed("UTF-16", &text, body, at))),
                }
            }
            if !body.len().is_multiple_of(2) {
                return (text.clone(), Some(malformed("UTF-16", &text, body, at)));
            }
            (text, None)
        }
        Encoding::Utf32Le | Encoding::Utf32Be => {
            let mut text = String::new();
            let mut at = 0usize;
            for &a in body.as_chunks::<4>().0 {
                let v = if enc == Encoding::Utf32Be {
                    u32::from_be_bytes(a)
                } else {
                    u32::from_le_bytes(a)
                };
                match char::from_u32(v) {
                    Some(ch) => {
                        text.push(ch);
                        at += 4;
                    }
                    None => return (text.clone(), Some(malformed("UTF-32", &text, body, at))),
                }
            }
            if !body.len().is_multiple_of(4) {
                return (text.clone(), Some(malformed("UTF-32", &text, body, at)));
            }
            (text, None)
        }
        Encoding::Single(_) => (String::from_utf16_lossy(&codec::decode(body, enc)), None),
    }
}
