pub use yellowcode_core::service::input::*;

use crate::transport as http;
use memory_budget::{BudgetError, BudgetedVec, Category, Charged, MemoryBudget};
use std::io::{self, BufRead, Read};
#[derive(Debug)]
pub enum HttpInputError {
    Status(u16),
    Memory(BudgetError),
    Io(io::Error),
}
impl From<BudgetError> for HttpInputError {
    fn from(e: BudgetError) -> Self {
        Self::Memory(e)
    }
}
impl From<io::Error> for HttpInputError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

pub struct StackReader<R> {
    reader: R,
    bytes: [u8; 8192],
    start: usize,
    end: usize,
}
impl<R: Read> StackReader<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            bytes: [0; 8192],
            start: 0,
            end: 0,
        }
    }
}
impl<R: Read> BufRead for StackReader<R> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.start == self.end {
            self.end = self.reader.read(&mut self.bytes)?;
            self.start = 0;
        }
        Ok(self.bytes.get(self.start..self.end).unwrap_or(&[]))
    }
    fn consume(&mut self, n: usize) {
        self.start = self.start.saturating_add(n).min(self.end);
    }
}
impl<R: Read> Read for StackReader<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        let source = self.fill_buf()?;
        let n = source.len().min(output.len());
        output
            .get_mut(..n)
            .ok_or(io::ErrorKind::InvalidInput)?
            .copy_from_slice(source.get(..n).ok_or(io::ErrorKind::InvalidInput)?);
        self.consume(n);
        Ok(n)
    }
}
fn stack_line(
    reader: &mut dyn BufRead,
    line: &mut [u8],
    status: u16,
) -> std::result::Result<usize, HttpInputError> {
    let mut length = 0usize;
    loop {
        let source = reader.fill_buf()?;
        if source.is_empty() {
            return Err(HttpInputError::Status(400));
        }
        let newline = source.iter().position(|b| *b == b'\n');
        let count = newline.unwrap_or(source.len());
        let end = length
            .checked_add(count)
            .filter(|n| *n <= line.len())
            .ok_or(HttpInputError::Status(status))?;
        line.get_mut(length..end)
            .ok_or(HttpInputError::Status(status))?
            .copy_from_slice(source.get(..count).ok_or(HttpInputError::Status(400))?);
        reader.consume(count + usize::from(newline.is_some()));
        length = end;
        if newline.is_some() {
            if line.get(length.wrapping_sub(1)) == Some(&b'\r') {
                length -= 1;
            }
            return Ok(length);
        }
    }
}
fn owned_text(
    text: &str,
    budget: &MemoryBudget,
) -> std::result::Result<Charged<String>, HttpInputError> {
    let mut bytes = BudgetedVec::try_with_capacity(budget, Category::Input, text.len())?;
    bytes.try_extend_from_slice(text.as_bytes())?;
    bytes
        .into_charged_vec()
        .try_map(String::from_utf8)
        .map_err(|_| HttpInputError::Status(400))
}
fn decoded(
    text: &str,
    plus: bool,
    budget: &MemoryBudget,
) -> std::result::Result<Charged<String>, HttpInputError> {
    let mut out = BudgetedVec::try_with_capacity(budget, Category::Input, text.len())?;
    let source = text.as_bytes();
    let mut i = 0;
    while let Some(byte) = source.get(i) {
        match byte {
            b'%' => {
                let hex = |b: &u8| -> Option<u8> {
                    match b {
                        b'0'..=b'9' => Some(b - b'0'),
                        b'a'..=b'f' => Some(b - b'a' + 10),
                        b'A'..=b'F' => Some(b - b'A' + 10),
                        _ => None,
                    }
                };
                let hi = source
                    .get(i + 1)
                    .and_then(hex)
                    .ok_or(HttpInputError::Status(400))?;
                let lo = source
                    .get(i + 2)
                    .and_then(hex)
                    .ok_or(HttpInputError::Status(400))?;
                out.try_push(hi * 16 + lo)?;
                i += 3;
            }
            b'+' if plus => {
                out.try_push(b' ')?;
                i += 1;
            }
            byte => {
                out.try_push(*byte)?;
                i += 1;
            }
        }
    }
    out.into_charged_vec()
        .try_map(String::from_utf8)
        .map_err(|_| HttpInputError::Status(400))
}
pub fn read_http_request(
    reader: &mut dyn Read,
    budget: &MemoryBudget,
) -> std::result::Result<Charged<http::Request>, HttpInputError> {
    let mut reader = StackReader::new(reader);
    let mut first = [0; 8192];
    let n = stack_line(&mut reader, &mut first, 414)?;
    let first = std::str::from_utf8(first.get(..n).ok_or(HttpInputError::Status(400))?)
        .map_err(|_| HttpInputError::Status(400))?;
    let mut fields = first.split(' ');
    let (method, target, version) =
        match (fields.next(), fields.next(), fields.next(), fields.next()) {
            (Some(m), Some(t), Some(v), None) => (m, t, v),
            _ => return Err(HttpInputError::Status(400)),
        };
    if !version.starts_with("HTTP/1.") || method.is_empty() {
        return Err(HttpInputError::Status(400));
    }
    let mut line = [0; 16384];
    let (
        mut total,
        mut lengths,
        mut length,
        mut invalid_length,
        mut excessive_length,
        mut json,
        mut chunked,
    ) = (0usize, 0usize, 0usize, false, false, false, false);
    loop {
        let n = stack_line(&mut reader, &mut line, 431)?;
        if n == 0 {
            break;
        }
        total = total
            .checked_add(n + 2)
            .filter(|n| *n <= 16384)
            .ok_or(HttpInputError::Status(431))?;
        let header = line.get(..n).ok_or(HttpInputError::Status(400))?;
        if let Some(colon) = header.iter().position(|b| *b == b':') {
            let name = std::str::from_utf8(header.get(..colon).ok_or(HttpInputError::Status(400))?)
                .unwrap_or("")
                .trim();
            if !["content-length", "content-type", "transfer-encoding"]
                .iter()
                .any(|selected| name.eq_ignore_ascii_case(selected))
            {
                continue;
            }
            let raw_value = header.get(colon + 1..).ok_or(HttpInputError::Status(400))?;
            let value = std::str::from_utf8(raw_value).unwrap_or("").trim();
            if name.eq_ignore_ascii_case("content-length") {
                lengths += 1;
                if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                    invalid_length = true;
                } else {
                    match value.parse::<usize>() {
                        Ok(n) => length = n,
                        Err(_) => excessive_length = true,
                    }
                }
            } else if name.eq_ignore_ascii_case("content-type") {
                json = raw_value
                    .split(|b| *b == b';')
                    .next()
                    .and_then(|prefix| std::str::from_utf8(prefix).ok())
                    .is_some_and(|v| v.trim().eq_ignore_ascii_case("application/json"));
            } else if name.eq_ignore_ascii_case("transfer-encoding") {
                chunked = true;
            }
        }
    }
    if !matches!(method, "GET" | "HEAD" | "POST") {
        return Err(HttpInputError::Status(405));
    }
    if !target.starts_with('/') {
        return Err(HttpInputError::Status(400));
    }
    let (raw_path, raw_query) = target.split_once('?').unwrap_or((target, ""));
    let path = decoded(raw_path, false, budget)?;
    if path.contains('\0') || path.split('/').any(|p| p == "..") {
        return Err(HttpInputError::Status(400));
    }
    let capacity = raw_query.split('&').filter(|p| !p.is_empty()).count();
    let mut pairs =
        BudgetedVec::<(String, String)>::try_with_capacity(budget, Category::Input, capacity)?
            .into_charged_vec();
    for pair in raw_query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let pair = decoded(k, true, budget)?.try_zip(decoded(v, true, budget)?)?;
        pairs = pairs.try_zip(pair)?.map(|(mut pairs, pair)| {
            pairs.push(pair);
            pairs
        });
    }
    let mut body = if method == "POST" {
        if chunked {
            return Err(HttpInputError::Status(501));
        }
        if lengths == 0 {
            return Err(HttpInputError::Status(411));
        }
        if lengths != 1 || invalid_length {
            return Err(HttpInputError::Status(400));
        }
        if excessive_length || length > super::query::MAX_INPUT_BYTES {
            return Err(HttpInputError::Status(413));
        }
        if !json {
            return Err(HttpInputError::Status(415));
        }
        BudgetedVec::<u8>::try_with_capacity(budget, Category::Input, length)?
    } else {
        BudgetedVec::new(budget, Category::Input)?
    };
    if method == "POST" {
        let mut chunk = [0; 8192];
        while body.len() < length {
            let n = chunk.len().min(length - body.len());
            let slice = chunk.get_mut(..n).ok_or(HttpInputError::Status(400))?;
            reader.read_exact(slice)?;
            body.try_extend_from_slice(slice)?;
        }
    }
    Ok(owned_text(method, budget)?
        .try_zip(path)?
        .try_zip(pairs)?
        .try_zip(body.into_charged_vec())?
        .map(|(((method, path), query), body)| http::Request {
            method,
            path,
            query,
            body,
            host: None,
            sec_fetch_site: None,
        }))
}
