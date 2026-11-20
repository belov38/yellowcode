use crate::graph::read::{R, Read, ReadError};
use crate::templates::allocation::{Allocation, Owned};
use crate::xml::{
    read::{CheckedReadError, parse_arena_checked, parse_owned_arena_checked},
    view::XmlArena,
};

pub(in crate::templates) enum Normalized<'a> {
    Borrowed(&'a [u8]),
    Owned(Owned<Vec<u8>>),
}
#[derive(Clone, Copy)]
struct Control<'a>(&'a dyn Fn() -> bool);
impl Control<'_> {
    fn check(self) -> Result<(), CheckedReadError> {
        if (self.0)() {
            return Err(CheckedReadError::Cancelled);
        }
        Ok(())
    }
    fn at(self, at: usize) -> Result<(), CheckedReadError> {
        if at.is_multiple_of(4096) {
            self.check()?;
        }
        Ok(())
    }
    fn find(self, bytes: &[u8], needle: &[u8]) -> Result<Option<usize>, CheckedReadError> {
        self.check()?;
        for (at, candidate) in bytes.windows(needle.len()).enumerate() {
            self.at(at)?;
            if candidate == needle {
                return Ok(Some(at));
            }
        }
        Ok(None)
    }
}
#[derive(Default)]
struct AttributeScan {
    at: usize,
    tag: bool,
    quote: Option<u8>,
}
impl AttributeScan {
    fn next(
        &mut self,
        bytes: &[u8],
        control: Control<'_>,
    ) -> Result<Option<usize>, CheckedReadError> {
        control.check()?;
        let skip = |from: usize, end: &[u8]| -> Result<usize, CheckedReadError> {
            Ok(control
                .find(bytes.get(from..).unwrap_or_default(), end)?
                .map_or(bytes.len(), |at| from + at + end.len()))
        };
        while let Some(&byte) = bytes.get(self.at) {
            control.at(self.at)?;
            let at = self.at;
            self.at += 1;
            if !self.tag {
                if byte != b'<' {
                    continue;
                }
                let tail = bytes.get(at..).unwrap_or_default();
                if tail.starts_with(b"<!--") {
                    self.at = skip(at + 4, b"-->")?;
                    continue;
                }
                if tail.starts_with(b"<![CDATA[") {
                    self.at = skip(at + 9, b"]]>")?;
                    continue;
                }
                if tail.starts_with(b"<?") {
                    self.at = skip(at + 2, b"?>")?;
                    continue;
                }
                self.tag = true;
            } else {
                match (self.quote, byte) {
                    (None, b'>') => self.tag = false,
                    (None, b'\'' | b'"') => self.quote = Some(byte),
                    (Some(end), _) if byte == end => self.quote = None,
                    (Some(_), b'\n' | b'\t') => return Ok(Some(at)),
                    _ => {}
                }
            }
        }
        Ok(None)
    }
}
fn normalize_control<'a>(
    bytes: &'a [u8],
    allocation: Allocation<'_>,
    control: Control<'_>,
) -> Result<Normalized<'a>, CheckedReadError> {
    control.check()?;
    let lines = if control.find(bytes, b"\r")?.is_some() {
        let mut output = allocation.vector(bytes.len())?;
        let mut at = 0;
        while let Some(&byte) = bytes.get(at) {
            control.at(at)?;
            if byte == b'\r' {
                output.push(b'\n')?;
                if bytes.get(at + 1) == Some(&b'\n') {
                    control.at(at + 1)?;
                    at += 1;
                }
            } else {
                output.push(byte)?;
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
    if AttributeScan::default().next(source, control)?.is_none() {
        control.check()?;
        return Ok(lines);
    }
    let owned = match lines {
        Normalized::Borrowed(bytes) => {
            let mut output = allocation.vector(bytes.len())?;
            for chunk in bytes.chunks(4096) {
                control.check()?;
                output.extend(chunk)?;
            }
            output.finish()
        }
        Normalized::Owned(bytes) => bytes,
    };
    let mut failure = None;
    let owned = owned.map(|mut bytes| {
        let mut replace = || -> Result<(), CheckedReadError> {
            let mut scan = AttributeScan::default();
            while let Some(index) = scan.next(&bytes, control)? {
                if let Some(byte) = bytes.get_mut(index) {
                    *byte = b' ';
                }
            }
            control.check()
        };
        if let Err(error) = replace() {
            failure = Some(error);
        }
        bytes
    });
    if let Some(error) = failure {
        return Err(error);
    }
    Ok(Normalized::Owned(owned))
}
#[derive(Debug)]
pub(in crate::templates) enum CheckedError {
    Read(ReadError),
    Cancelled,
}
fn failure(read: Read<'_>, error: CheckedReadError) -> CheckedError {
    match error {
        CheckedReadError::Cancelled => CheckedError::Cancelled,
        CheckedReadError::Read(crate::xml::read::ReadError::Memory(error)) => {
            CheckedError::Read(ReadError::Memory(error))
        }
        CheckedReadError::Read(crate::xml::read::ReadError::Semantic { .. }) => {
            CheckedError::Read(read.invalid("xdto XML"))
        }
    }
}
fn legacy_error(error: CheckedError) -> ReadError {
    match error {
        CheckedError::Read(error) => error,
        CheckedError::Cancelled => ReadError::Memory(memory_budget::BudgetError::Overflow),
    }
}
pub(in crate::templates) fn parse<'a>(
    data: &'a [u8],
    allocation: Allocation<'_>,
) -> R<XmlArena<'a>> {
    parse_checked(data, allocation, &|| false).map_err(legacy_error)
}
pub(in crate::templates) fn parse_checked<'a>(
    data: &'a [u8],
    allocation: Allocation<'_>,
    cancelled: &dyn Fn() -> bool,
) -> Result<XmlArena<'a>, CheckedError> {
    let read = Read::new(allocation);
    let normalized = normalize_control(data, allocation, Control(cancelled))
        .map_err(|error| failure(read, error))?;
    let result = match normalized {
        Normalized::Borrowed(data) => parse_arena_checked(data, allocation, cancelled),
        Normalized::Owned(data) => parse_owned_arena_checked(data, allocation, cancelled),
    };
    result.map_err(|error| failure(read, error))
}
