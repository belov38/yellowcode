pub use crate::preview_wire::{DecodeError, ProtocolError, WriteError};
use crate::{
    preview_layout::ErrorKind,
    preview_picture::{self, Image},
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Asset<'a> {
    pub key: &'a str,
    pub image: Image<'a>,
}
#[derive(Clone, Copy, Debug)]
pub struct Assets<'a> {
    bytes: &'a [u8],
    count: usize,
}
impl<'a> Assets<'a> {
    pub fn len(&self) -> usize {
        self.count
    }
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
    pub fn iter(&self) -> impl Iterator<Item = Result<Asset<'a>, ProtocolError>> {
        let mut cursor = Cursor(self.bytes);
        (0..self.count).map(move |_| cursor.asset(usize::MAX))
    }
}
struct Writer<F>(F);
impl<E, F: FnMut(&[u8]) -> Result<(), E>> Writer<F> {
    fn put(&mut self, bytes: &[u8]) -> Result<(), WriteError<E>> {
        (self.0)(bytes).map_err(WriteError::Sink)
    }
    fn number(&mut self, n: usize) -> Result<(), WriteError<E>> {
        self.put(
            &u64::try_from(n)
                .map_err(|_| WriteError::Overflow)?
                .to_le_bytes(),
        )
    }
    fn blob(&mut self, bytes: &[u8]) -> Result<(), WriteError<E>> {
        self.number(bytes.len())?;
        self.put(bytes)
    }
}
pub fn encode_assets<'a, E>(
    count: usize,
    rows: impl Iterator<Item = Result<Asset<'a>, E>>,
    sink: impl FnMut(&[u8]) -> Result<(), E>,
) -> Result<(), WriteError<E>> {
    let mut out = Writer(sink);
    out.put(b"AST1")?;
    out.number(count)?;
    let mut actual = 0usize;
    for row in rows {
        let row = row.map_err(WriteError::Sink)?;
        actual = actual
            .checked_add(1)
            .filter(|n| *n <= count)
            .ok_or(WriteError::Overflow)?;
        out.blob(row.key.as_bytes())?;
        preview_picture::encode(Some(row.image), &mut out.0)?;
    }
    if actual != count {
        return Err(WriteError::Overflow);
    }
    Ok(())
}
struct Cursor<'a>(&'a [u8]);
impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], ProtocolError> {
        let bytes = self.0.get(..n).ok_or(ProtocolError)?;
        self.0 = self.0.get(n..).ok_or(ProtocolError)?;
        Ok(bytes)
    }
    fn number(&mut self) -> Result<usize, ProtocolError> {
        let bytes = self.take(8)?.try_into().map_err(|_| ProtocolError)?;
        usize::try_from(u64::from_le_bytes(bytes)).map_err(|_| ProtocolError)
    }
    fn blob(&mut self) -> Result<&'a [u8], ProtocolError> {
        let n = self.number()?;
        self.take(n)
    }
    fn text(&mut self) -> Result<&'a str, ProtocolError> {
        std::str::from_utf8(self.blob()?).map_err(|_| ProtocolError)
    }
    fn asset(&mut self, max_file: usize) -> Result<Asset<'a>, ProtocolError> {
        let key = self.text()?;
        if !key.starts_with("config:") && !key.starts_with("form:") {
            return Err(ProtocolError);
        }
        let before = self.0;
        self.take(6)?;
        let length = self.number()?;
        if length > max_file {
            return Err(ProtocolError);
        }
        self.take(length)?;
        let image_length = length.checked_add(14).ok_or(ProtocolError)?;
        let image = preview_picture::decode_checked(
            before.get(..image_length).ok_or(ProtocolError)?,
            max_file,
            || Ok::<_, ()>(()),
        )
        .map_err(|_| ProtocolError)?
        .ok_or(ProtocolError)?;
        Ok(Asset { key, image })
    }
    fn finish(&self) -> Result<(), ProtocolError> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(ProtocolError)
        }
    }
}
pub fn decode_assets_checked<E>(
    bytes: &[u8],
    max_file: usize,
    max_total: usize,
    mut check: impl FnMut() -> Result<(), E>,
) -> Result<Assets<'_>, DecodeError<E>> {
    let protocol = |_| DecodeError::Protocol;
    check().map_err(DecodeError::Check)?;
    let mut cursor = Cursor(bytes);
    if cursor.take(4).map_err(protocol)? != b"AST1" {
        return Err(DecodeError::Protocol);
    }
    let count = cursor.number().map_err(protocol)?;
    if count > cursor.0.len() / 22 {
        return Err(DecodeError::Protocol);
    }
    let rows = cursor.0;
    let mut total = 0usize;
    for _ in 0..count {
        check().map_err(DecodeError::Check)?;
        let row = cursor.asset(max_file).map_err(protocol)?;
        total = total
            .checked_add(row.image.bytes.len())
            .filter(|n| *n <= max_total)
            .ok_or(DecodeError::Protocol)?;
    }
    cursor.finish().map_err(protocol)?;
    check().map_err(DecodeError::Check)?;
    Ok(Assets { bytes: rows, count })
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome<'a> {
    Body(&'a str),
    ModelError(&'a str),
    AutoformError(&'a str),
    LayoutError(&'a str),
}
pub fn encode_result<E>(
    kind: Option<ErrorKind>,
    body: &str,
    sink: impl FnMut(&[u8]) -> Result<(), E>,
) -> Result<(), WriteError<E>> {
    let mut out = Writer(sink);
    out.put(b"RND1")?;
    out.put(&[match kind {
        None => 0,
        Some(ErrorKind::Model) => 1,
        Some(ErrorKind::Autoform) => 2,
        Some(ErrorKind::Layout) => 3,
    }])?;
    out.blob(body.as_bytes())
}
pub fn decode_result_checked<E>(
    bytes: &[u8],
    maximum: usize,
    mut check: impl FnMut() -> Result<(), E>,
) -> Result<Outcome<'_>, DecodeError<E>> {
    let protocol = |_| DecodeError::Protocol;
    check().map_err(DecodeError::Check)?;
    let mut cursor = Cursor(bytes);
    if cursor.take(4).map_err(protocol)? != b"RND1" {
        return Err(DecodeError::Protocol);
    }
    let tag = cursor
        .take(1)
        .map_err(protocol)?
        .first()
        .copied()
        .ok_or(DecodeError::Protocol)?;
    let size = cursor.number().map_err(protocol)?;
    if size > maximum {
        return Err(DecodeError::Protocol);
    }
    let text = std::str::from_utf8(cursor.take(size).map_err(protocol)?)
        .map_err(|_| DecodeError::Protocol)?;
    cursor.finish().map_err(protocol)?;
    let outcome = match tag {
        0 => Outcome::Body(text),
        1 => Outcome::ModelError(text),
        2 => Outcome::AutoformError(text),
        3 => Outcome::LayoutError(text),
        _ => return Err(DecodeError::Protocol),
    };
    check().map_err(DecodeError::Check)?;
    Ok(outcome)
}
