use crate::graph::read::{R, Read, ReadError};
use crate::templates::allocation::{Allocation, Owned};
use crate::xml::{
    read::{parse_arena, parse_owned_arena},
    view::XmlArena,
};

pub(in crate::templates) enum Normalized<'a> {
    Borrowed(&'a [u8]),
    Owned(Owned<Vec<u8>>),
}
#[derive(Default)]
struct AttributeScan {
    at: usize,
    tag: bool,
    quote: Option<u8>,
}
impl AttributeScan {
    fn next(&mut self, bytes: &[u8]) -> Option<usize> {
        let skip = |from: usize, end: &[u8]| {
            bytes
                .get(from..)
                .and_then(|tail| tail.windows(end.len()).position(|w| w == end))
                .map_or(bytes.len(), |at| from + at + end.len())
        };
        while let Some(&byte) = bytes.get(self.at) {
            let at = self.at;
            self.at += 1;
            if !self.tag {
                if byte != b'<' {
                    continue;
                }
                let tail = bytes.get(at..).unwrap_or_default();
                if tail.starts_with(b"<!--") {
                    self.at = skip(at + 4, b"-->");
                    continue;
                }
                if tail.starts_with(b"<![CDATA[") {
                    self.at = skip(at + 9, b"]]>");
                    continue;
                }
                if tail.starts_with(b"<?") {
                    self.at = skip(at + 2, b"?>");
                    continue;
                }
                self.tag = true;
            } else {
                match (self.quote, byte) {
                    (None, b'>') => self.tag = false,
                    (None, b'\'' | b'"') => self.quote = Some(byte),
                    (Some(end), _) if byte == end => self.quote = None,
                    (Some(_), b'\n' | b'\t') => return Some(at),
                    _ => {}
                }
            }
        }
        None
    }
}
fn normalize<'a>(bytes: &'a [u8], allocation: Allocation<'_>) -> R<Normalized<'a>> {
    let lines = if bytes.contains(&b'\r') {
        let mut output = allocation.vector(bytes.len())?;
        let mut at = 0;
        while let Some(&byte) = bytes.get(at) {
            if byte == b'\r' {
                output.push(b'\n')?;
                if bytes.get(at + 1) == Some(&b'\n') {
                    at += 1
                }
            } else {
                output.push(byte)?
            }
            at += 1;
        }
        Normalized::Owned(output.finish())
    } else {
        Normalized::Borrowed(bytes)
    };
    let source: &[u8] = match &lines {
        Normalized::Borrowed(bytes) => bytes,
        Normalized::Owned(bytes) => bytes,
    };
    if AttributeScan::default().next(source).is_none() {
        return Ok(lines);
    }
    let owned = match lines {
        Normalized::Borrowed(bytes) => allocation.bytes(bytes)?,
        Normalized::Owned(bytes) => bytes,
    };
    Ok(Normalized::Owned(owned.map(|mut bytes| {
        let mut scan = AttributeScan::default();
        while let Some(index) = scan.next(&bytes) {
            if let Some(byte) = bytes.get_mut(index) {
                *byte = b' ';
            }
        }
        bytes
    })))
}
pub(in crate::templates) fn parse<'a>(
    data: &'a [u8],
    allocation: Allocation<'_>,
) -> R<XmlArena<'a>> {
    let read = Read::new(allocation);
    let result = match normalize(data, allocation)? {
        Normalized::Borrowed(data) => parse_arena(data, allocation),
        Normalized::Owned(data) => parse_owned_arena(data, allocation),
    };
    result.map_err(|error| match error {
        crate::xml::read::ReadError::Memory(error) => ReadError::Memory(error),
        crate::xml::read::ReadError::Semantic { .. } => read.invalid("xdto XML"),
    })
}
