use crate::ExternalTypeRequest;
use std::convert::Infallible;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    Model,
    Autoform,
}
#[derive(Debug, PartialEq, Eq)]
pub enum WriteError<E> {
    Sink(E),
    Overflow,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProtocolError;
#[derive(Debug, PartialEq, Eq)]
pub enum DecodeError<E> {
    Protocol,
    Check(E),
}
impl<E> From<ProtocolError> for DecodeError<E> {
    fn from(_: ProtocolError) -> Self {
        Self::Protocol
    }
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
    fn text(&mut self, text: &str) -> Result<(), WriteError<E>> {
        self.count(text.len())?;
        self.put(text.as_bytes())
    }
}
pub fn encode_requests<E>(
    requests: &[ExternalTypeRequest],
    sink: impl FnMut(&[u8]) -> Result<(), E>,
) -> Result<(), WriteError<E>> {
    let mut out = Writer(sink);
    out.put(b"PRV1")?;
    out.put(&[0])?;
    out.count(requests.len())?;
    for request in requests {
        out.text(&request.owner)?;
        out.count(request.path.len())?;
        for segment in &request.path {
            out.text(segment)?;
        }
    }
    Ok(())
}
pub fn encode_error<E>(
    kind: ErrorKind,
    message: &str,
    sink: impl FnMut(&[u8]) -> Result<(), E>,
) -> Result<(), WriteError<E>> {
    let mut out = Writer(sink);
    out.put(b"PRV1")?;
    out.put(&[match kind {
        ErrorKind::Model => 1,
        ErrorKind::Autoform => 2,
    }])?;
    out.text(message)
}

#[derive(Debug)]
pub enum Prepared<'a> {
    Requests(Requests<'a>),
    ModelError(&'a str),
    AutoformError(&'a str),
}
#[derive(Clone, Copy, Debug)]
pub struct Requests<'a> {
    bytes: &'a [u8],
    count: usize,
}
impl<'a> Requests<'a> {
    pub fn len(&self) -> usize {
        self.count
    }
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
    pub fn iter(&self) -> RequestIter<'a> {
        RequestIter {
            input: Reader(self.bytes),
            remaining: self.count,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Request<'a> {
    pub owner: &'a str,
    segments: &'a [u8],
    count: usize,
}
impl<'a> Request<'a> {
    pub fn segments(&self) -> Segments<'a> {
        Segments {
            input: Reader(self.segments),
            remaining: self.count,
        }
    }
}
pub struct RequestIter<'a> {
    input: Reader<'a>,
    remaining: usize,
}
impl<'a> Iterator for RequestIter<'a> {
    type Item = Result<Request<'a>, ProtocolError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        let result = without_checks(self.input.request(&mut || Ok::<_, Infallible>(())));
        if result.is_err() {
            self.remaining = 0;
        }
        Some(result)
    }
}
pub struct Segments<'a> {
    input: Reader<'a>,
    remaining: usize,
}
impl<'a> Iterator for Segments<'a> {
    type Item = Result<&'a str, ProtocolError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        let result = self.input.text();
        if result.is_err() {
            self.remaining = 0;
        }
        Some(result)
    }
}
#[derive(Clone, Copy)]
struct Reader<'a>(&'a [u8]);
impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], ProtocolError> {
        let bytes = self.0.get(..count).ok_or(ProtocolError)?;
        self.0 = self.0.get(count..).ok_or(ProtocolError)?;
        Ok(bytes)
    }
    fn byte(&mut self) -> Result<u8, ProtocolError> {
        self.take(1)?.first().copied().ok_or(ProtocolError)
    }
    fn count(&mut self) -> Result<usize, ProtocolError> {
        let bytes = self.take(8)?.try_into().map_err(|_| ProtocolError)?;
        usize::try_from(u64::from_le_bytes(bytes)).map_err(|_| ProtocolError)
    }
    fn text(&mut self) -> Result<&'a str, ProtocolError> {
        let count = self.count()?;
        std::str::from_utf8(self.take(count)?).map_err(|_| ProtocolError)
    }
    fn request<E>(
        &mut self,
        check: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<Request<'a>, DecodeError<E>> {
        check().map_err(DecodeError::Check)?;
        let owner = self.text()?;
        let count = self.count()?;
        if count > self.0.len() / 8 {
            return Err(DecodeError::Protocol);
        }
        let start = self.0;
        for _ in 0..count {
            check().map_err(DecodeError::Check)?;
            self.text()?;
            check().map_err(DecodeError::Check)?;
        }
        let length = start.len().checked_sub(self.0.len()).ok_or(ProtocolError)?;
        let segments = start.get(..length).ok_or(ProtocolError)?;
        check().map_err(DecodeError::Check)?;
        Ok(Request {
            owner,
            segments,
            count,
        })
    }
}
fn without_checks<T>(result: Result<T, DecodeError<Infallible>>) -> Result<T, ProtocolError> {
    match result {
        Ok(value) => Ok(value),
        Err(DecodeError::Protocol) => Err(ProtocolError),
        Err(DecodeError::Check(never)) => match never {},
    }
}
pub fn decode_result(bytes: &[u8]) -> Result<Prepared<'_>, ProtocolError> {
    without_checks(decode_result_checked(bytes, || Ok::<_, Infallible>(())))
}
pub fn decode_result_checked<E>(
    bytes: &[u8],
    mut check: impl FnMut() -> Result<(), E>,
) -> Result<Prepared<'_>, DecodeError<E>> {
    check().map_err(DecodeError::Check)?;
    let mut input = Reader(bytes);
    if input.take(4)? != b"PRV1" {
        return Err(DecodeError::Protocol);
    }
    let tag = input.byte()?;
    let result = match tag {
        0 => {
            let count = input.count()?;
            if count > input.0.len() / 16 {
                return Err(DecodeError::Protocol);
            }
            let rows = input.0;
            for _ in 0..count {
                input.request(&mut check)?;
            }
            Prepared::Requests(Requests { bytes: rows, count })
        }
        1 => Prepared::ModelError(input.text()?),
        2 => Prepared::AutoformError(input.text()?),
        _ => return Err(DecodeError::Protocol),
    };
    if !input.0.is_empty() {
        return Err(DecodeError::Protocol);
    }
    check().map_err(DecodeError::Check)?;
    Ok(result)
}
