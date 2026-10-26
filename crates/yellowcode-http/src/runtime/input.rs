use crate::service::input::StackReader;
use std::io::{self, BufRead, Read};
use yellowcode_server::service::RequestData;

const REQUEST_LINE: usize = 8192;
const HEADERS: usize = 16384;
const BODY: usize = 8 * 1024 * 1024;

#[derive(Debug)]
pub enum InputError {
    Status(u16),
    Io(io::Error),
}
impl InputError {
    pub fn status(&self) -> u16 {
        match self {
            Self::Status(status) => *status,
            Self::Io(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) =>
            {
                408
            }
            Self::Io(_) => 400,
        }
    }
}
impl From<io::Error> for InputError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
fn bad() -> InputError {
    InputError::Status(400)
}
fn token(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}

fn line(reader: &mut dyn BufRead, output: &mut [u8], status: u16) -> Result<usize, InputError> {
    let mut used = 0usize;
    loop {
        let source = reader.fill_buf()?;
        if source.is_empty() {
            return Err(bad());
        }
        let newline = source.iter().position(|b| *b == b'\n');
        let count = newline.map_or(source.len(), |n| n + 1);
        let end = used
            .checked_add(count)
            .filter(|n| *n <= output.len())
            .ok_or(InputError::Status(status))?;
        output
            .get_mut(used..end)
            .ok_or_else(bad)?
            .copy_from_slice(source.get(..count).ok_or_else(bad)?);
        reader.consume(count);
        used = end;
        if newline.is_some() {
            if used < 2 || output.get(used - 2) != Some(&b'\r') {
                return Err(bad());
            }
            return Ok(used - 2);
        }
    }
}
fn decode(raw: &str, plus: bool) -> Result<String, InputError> {
    let text = if plus {
        raw.replace('+', " ")
    } else {
        raw.to_owned()
    };
    let text = crate::percent_decode(&text).ok_or_else(bad)?;
    if text.chars().any(char::is_control) {
        return Err(bad());
    }
    Ok(text)
}
fn base_url(origin: &str, path: &str) -> String {
    let Some(root) = path.strip_prefix("/hs/").and_then(|s| s.split('/').next()) else {
        return origin.to_owned();
    };
    let mut url = format!("{origin}/hs/");
    for byte in root.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            url.push(char::from(byte));
        } else {
            use std::fmt::Write;
            let _ = write!(url, "%{byte:02X}");
        }
    }
    url
}

pub fn read_request(reader: &mut dyn Read, origin: &str) -> Result<RequestData, InputError> {
    let mut reader = StackReader::new(reader);
    let mut first = [0; REQUEST_LINE];
    let count = line(&mut reader, &mut first, 414)?;
    let first = std::str::from_utf8(first.get(..count).ok_or_else(bad)?).map_err(|_| bad())?;
    let mut parts = first.split(' ');
    let (method, target) = match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(method), Some(target), Some("HTTP/1.1"), None)
            if !method.is_empty() && method.bytes().all(token) =>
        {
            (method, target)
        }
        _ => return Err(bad()),
    };
    if !target.starts_with('/') || target.contains('#') || target.chars().any(char::is_control) {
        return Err(bad());
    }
    let (raw_path, query) = target.split_once('?').unwrap_or((target, ""));
    let path = decode(raw_path, false)?;
    if path.contains('\\') || path.split('/').any(|s| s == "..") {
        return Err(bad());
    }
    let mut request = RequestData {
        method: method.to_owned(),
        base_url: base_url(origin, &path),
        path,
        ..Default::default()
    };
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        request
            .query
            .push((decode(key, true)?, decode(value, true)?));
    }
    let mut buffer = [0; HEADERS];
    let (mut total, mut hosts, mut length, mut transfer, mut expect) =
        (0usize, 0usize, None, false, false);
    loop {
        let count = line(&mut reader, &mut buffer, 431)?;
        if count == 0 {
            break;
        }
        total += count + 2;
        if total > HEADERS {
            return Err(InputError::Status(431));
        }
        let header =
            std::str::from_utf8(buffer.get(..count).ok_or_else(bad)?).map_err(|_| bad())?;
        let (name, value) = header.split_once(':').ok_or_else(bad)?;
        if name.is_empty()
            || !name.bytes().all(token)
            || !value.bytes().all(|b| b == b'\t' || (b >= 32 && b != 127))
        {
            return Err(bad());
        }
        let value = value.trim_matches([' ', '\t']);
        if name.eq_ignore_ascii_case("Host") {
            hosts += 1;
            if value.is_empty()
                || !value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-.:[]".contains(&b))
            {
                return Err(bad());
            }
        } else if name.eq_ignore_ascii_case("Content-Length") {
            if length.is_some() || value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err(bad());
            }
            length = Some(
                value
                    .parse::<usize>()
                    .map_err(|_| InputError::Status(413))?,
            );
        } else if name.eq_ignore_ascii_case("Transfer-Encoding") {
            transfer = true;
        } else if name.eq_ignore_ascii_case("Expect") {
            expect = true;
        }
        request.headers.push((name.to_owned(), value.to_owned()));
    }
    if hosts != 1 {
        return Err(bad());
    }
    if transfer {
        return Err(InputError::Status(if length.is_some() { 400 } else { 501 }));
    }
    if expect {
        return Err(InputError::Status(417));
    }
    let length = length.unwrap_or(0);
    if length > BODY {
        return Err(InputError::Status(413));
    }
    request
        .body
        .try_reserve_exact(length)
        .map_err(|_| InputError::Status(503))?;
    request.body.resize(length, 0);
    if let Err(error) = reader.read_exact(&mut request.body) {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            return Err(bad());
        }
        return Err(error.into());
    }
    Ok(request)
}
