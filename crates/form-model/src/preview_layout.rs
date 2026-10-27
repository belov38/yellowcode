pub use crate::preview_wire::{DecodeError, ProtocolError, WriteError};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    Model,
    Autoform,
    Layout,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Picture<'a> {
    Std(&'a str),
    Config(&'a str),
    Embedded(&'a str),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Input<'a> {
    pub source: &'a [u8],
    pub requests: &'a [u8],
    pub answers: &'a [u8],
}
#[derive(Clone, Copy, Debug)]
pub struct Pictures<'a> {
    bytes: &'a [u8],
    count: usize,
}
impl<'a> Pictures<'a> {
    pub fn len(&self) -> usize {
        self.count
    }
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
    pub fn iter(&self) -> impl Iterator<Item = Result<Picture<'a>, ProtocolError>> {
        let mut input = Reader(self.bytes);
        (0..self.count).map(move |_| input.picture())
    }
}
#[derive(Debug)]
pub enum Outcome<'a> {
    Pictures(Pictures<'a>),
    ModelError(&'a str),
    AutoformError(&'a str),
    LayoutError(&'a str),
}
struct Writer<F>(F);
impl<E, F: FnMut(&[u8]) -> Result<(), E>> Writer<F> {
    fn put(&mut self, bytes: &[u8]) -> Result<(), WriteError<E>> {
        (self.0)(bytes).map_err(WriteError::Sink)
    }
    fn count(&mut self, count: usize) -> Result<(), WriteError<E>> {
        self.put(
            &u64::try_from(count)
                .map_err(|_| WriteError::Overflow)?
                .to_le_bytes(),
        )
    }
    fn bytes(&mut self, bytes: &[u8]) -> Result<(), WriteError<E>> {
        self.count(bytes.len())?;
        self.put(bytes)
    }
}
pub fn encode_input<E>(
    input: Input<'_>,
    sink: impl FnMut(&[u8]) -> Result<(), E>,
) -> Result<(), WriteError<E>> {
    let mut out = Writer(sink);
    out.put(b"PIN1")?;
    for bytes in [input.source, input.requests, input.answers] {
        out.bytes(bytes)?;
    }
    Ok(())
}
pub fn encode_pictures<E>(
    pictures: &[Picture<'_>],
    sink: impl FnMut(&[u8]) -> Result<(), E>,
) -> Result<(), WriteError<E>> {
    let mut out = Writer(sink);
    out.put(b"LYT1\0")?;
    out.count(pictures.len())?;
    for picture in pictures {
        let (tag, name) = match picture {
            Picture::Std(name) => (0, name),
            Picture::Config(name) => (1, name),
            Picture::Embedded(name) => (2, name),
        };
        out.put(&[tag])?;
        out.bytes(name.as_bytes())?;
    }
    Ok(())
}
pub fn encode_error<E>(
    kind: ErrorKind,
    message: &str,
    sink: impl FnMut(&[u8]) -> Result<(), E>,
) -> Result<(), WriteError<E>> {
    let mut out = Writer(sink);
    out.put(b"LYT1")?;
    out.put(&[match kind {
        ErrorKind::Model => 1,
        ErrorKind::Autoform => 2,
        ErrorKind::Layout => 3,
    }])?;
    out.bytes(message.as_bytes())
}
struct Reader<'a>(&'a [u8]);
impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], ProtocolError> {
        let out = self.0.get(..count).ok_or(ProtocolError)?;
        self.0 = self.0.get(count..).ok_or(ProtocolError)?;
        Ok(out)
    }
    fn byte(&mut self) -> Result<u8, ProtocolError> {
        self.take(1)?.first().copied().ok_or(ProtocolError)
    }
    fn count(&mut self) -> Result<usize, ProtocolError> {
        let bytes = self.take(8)?.try_into().map_err(|_| ProtocolError)?;
        usize::try_from(u64::from_le_bytes(bytes)).map_err(|_| ProtocolError)
    }
    fn bytes(&mut self) -> Result<&'a [u8], ProtocolError> {
        let count = self.count()?;
        self.take(count)
    }
    fn text(&mut self) -> Result<&'a str, ProtocolError> {
        std::str::from_utf8(self.bytes()?).map_err(|_| ProtocolError)
    }
    fn picture(&mut self) -> Result<Picture<'a>, ProtocolError> {
        let tag = self.byte()?;
        let name = self.text()?;
        match tag {
            0 => Ok(Picture::Std(name)),
            1 => Ok(Picture::Config(name)),
            2 => Ok(Picture::Embedded(name)),
            _ => Err(ProtocolError),
        }
    }
    fn finish(&self) -> Result<(), ProtocolError> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(ProtocolError)
        }
    }
}
pub fn decode_input_checked<E>(
    bytes: &[u8],
    mut check: impl FnMut() -> Result<(), E>,
) -> Result<Input<'_>, DecodeError<E>> {
    check().map_err(DecodeError::Check)?;
    let mut input = Reader(bytes);
    if input.take(4)? != b"PIN1" {
        return Err(DecodeError::Protocol);
    }
    let source = input.bytes()?;
    check().map_err(DecodeError::Check)?;
    let requests = input.bytes()?;
    check().map_err(DecodeError::Check)?;
    let answers = input.bytes()?;
    input.finish()?;
    check().map_err(DecodeError::Check)?;
    Ok(Input {
        source,
        requests,
        answers,
    })
}
pub fn decode_result_checked<E>(
    bytes: &[u8],
    mut check: impl FnMut() -> Result<(), E>,
) -> Result<Outcome<'_>, DecodeError<E>> {
    check().map_err(DecodeError::Check)?;
    let mut input = Reader(bytes);
    if input.take(4)? != b"LYT1" {
        return Err(DecodeError::Protocol);
    }
    let result = match input.byte()? {
        0 => {
            let count = input.count()?;
            if count > input.0.len() / 9 {
                return Err(DecodeError::Protocol);
            }
            let bytes = input.0;
            for _ in 0..count {
                check().map_err(DecodeError::Check)?;
                input.picture()?;
                check().map_err(DecodeError::Check)?;
            }
            Outcome::Pictures(Pictures { bytes, count })
        }
        1 => Outcome::ModelError(input.text()?),
        2 => Outcome::AutoformError(input.text()?),
        3 => Outcome::LayoutError(input.text()?),
        _ => return Err(DecodeError::Protocol),
    };
    input.finish()?;
    check().map_err(DecodeError::Check)?;
    Ok(result)
}
