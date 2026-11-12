use std::io::{BufReader, Read};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("SQL output I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid SQL row stream: {0}")]
    Invalid(&'static str),
    #[error("SQL row exceeds the configured limit")]
    Limit,
    #[error("invalid SQL row JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("SQL row consumer: {0}")]
    Consumer(String),
}

pub fn read_framed_rows(
    input: impl Read,
    max_row_bytes: usize,
    mut consume: impl FnMut(&[u8]) -> Result<(), String>,
) -> Result<(), Error> {
    let mut bytes = BufReader::new(input).bytes();
    let mut next = || -> Result<Option<u8>, Error> {
        loop {
            match bytes.next().transpose()? {
                Some(b'\r' | b'\n') => continue,
                byte => return Ok(byte),
            }
        }
    };
    fn expect(
        next: &mut impl FnMut() -> Result<Option<u8>, Error>,
        value: &[u8],
    ) -> Result<(), Error> {
        for &byte in value {
            if next()? != Some(byte) {
                return Err(Error::Invalid("invalid frame marker"));
            }
        }
        Ok(())
    }
    fn nonspace(next: &mut impl FnMut() -> Result<Option<u8>, Error>) -> Result<Option<u8>, Error> {
        loop {
            match next()? {
                Some(b' ' | b'\t') => continue,
                byte => return Ok(byte),
            }
        }
    }
    expect(&mut next, b"YC_ROWS_BEGIN")?;
    let mut count = 0_u64;
    loop {
        match nonspace(&mut next)? {
            Some(b'R') => {
                if next()? != Some(b'{') {
                    return Err(Error::Invalid("expected row object"));
                }
                if max_row_bytes == 0 {
                    return Err(Error::Limit);
                }
                let mut row = vec![b'{'];
                let mut depth = 1_usize;
                let mut quoted = false;
                let mut escaped = false;
                while depth != 0 {
                    let byte = next()?.ok_or(Error::Invalid("truncated row"))?;
                    if row.len() >= max_row_bytes {
                        return Err(Error::Limit);
                    }
                    row.push(byte);
                    if quoted {
                        if escaped {
                            escaped = false;
                        } else if byte == b'\\' {
                            escaped = true;
                        } else if byte == b'"' {
                            quoted = false;
                        }
                    } else {
                        match byte {
                            b'"' => quoted = true,
                            b'{' | b'[' => depth += 1,
                            b'}' | b']' => depth -= 1,
                            _ => {}
                        }
                    }
                }
                let _: serde::de::IgnoredAny = serde_json::from_slice(&row)?;
                consume(&row).map_err(Error::Consumer)?;
                count = count
                    .checked_add(1)
                    .ok_or(Error::Invalid("row count overflow"))?;
            }
            Some(b'Y') => {
                expect(&mut next, b"C_ROWS_END:")?;
                let mut expected = 0_u64;
                let mut digits = 0_usize;
                loop {
                    match next()? {
                        Some(byte @ b'0'..=b'9') => {
                            digits += 1;
                            if digits > 20 {
                                return Err(Error::Invalid("invalid row count"));
                            }
                            expected = expected
                                .checked_mul(10)
                                .and_then(|n| n.checked_add(u64::from(byte - b'0')))
                                .ok_or(Error::Invalid("row count overflow"))?;
                        }
                        None => break,
                        Some(b' ' | b'\t') => {
                            if nonspace(&mut next)?.is_some() {
                                return Err(Error::Invalid("trailing output"));
                            }
                            break;
                        }
                        _ => return Err(Error::Invalid("invalid row count")),
                    }
                }
                if digits == 0 || expected != count {
                    return Err(Error::Invalid("row count mismatch"));
                }
                return Ok(());
            }
            _ => return Err(Error::Invalid("expected row or end marker")),
        }
    }
}

pub fn read_rows(
    input: impl Read,
    max_row_bytes: usize,
    consume: impl FnMut(&[u8]) -> Result<(), String>,
) -> Result<(), Error> {
    let mut frames = Frames::new(input);
    frames.rows(max_row_bytes, consume)?;
    frames.end()
}

pub struct Frames<R: Read> {
    bytes: std::io::Bytes<BufReader<R>>,
    peeked: Option<u8>,
}

impl<R: Read> Frames<R> {
    pub fn new(input: R) -> Self {
        Self {
            bytes: BufReader::new(input).bytes(),
            peeked: None,
        }
    }

    fn next(&mut self) -> Result<Option<u8>, Error> {
        if let Some(byte) = self.peeked.take() {
            return Ok(Some(byte));
        }
        loop {
            match self.bytes.next().transpose()? {
                Some(b'\r' | b'\n') => continue,
                byte => return Ok(byte),
            }
        }
    }

    fn nonspace(&mut self) -> Result<Option<u8>, Error> {
        loop {
            match self.next()? {
                Some(b' ' | b'\t') => continue,
                byte => return Ok(byte),
            }
        }
    }

    pub fn expect(&mut self, literal: &[u8]) -> Result<(), Error> {
        let mut first = true;
        for &byte in literal {
            let actual = if first {
                self.nonspace()?
            } else {
                self.next()?
            };
            first = false;
            if actual != Some(byte) {
                return Err(Error::Invalid("invalid frame marker"));
            }
        }
        Ok(())
    }

    pub fn number(&mut self) -> Result<u64, Error> {
        let mut value = 0_u64;
        let mut digits = 0_usize;
        loop {
            match self.next()? {
                Some(byte @ b'0'..=b'9') => {
                    digits += 1;
                    if digits > 20 {
                        return Err(Error::Invalid("invalid number"));
                    }
                    value = value
                        .checked_mul(10)
                        .and_then(|n| n.checked_add(u64::from(byte - b'0')))
                        .ok_or(Error::Invalid("number out of range"))?;
                }
                other => {
                    self.peeked = other;
                    break;
                }
            }
        }
        if digits == 0 {
            return Err(Error::Invalid("missing number"));
        }
        Ok(value)
    }

    fn balanced(&mut self, open: u8, max: usize) -> Result<Vec<u8>, Error> {
        let mut value = vec![open];
        let mut depth = 1_usize;
        let mut quoted = false;
        let mut escaped = false;
        while depth != 0 {
            let byte = self.next()?.ok_or(Error::Invalid("truncated row"))?;
            if value.len() >= max {
                return Err(Error::Limit);
            }
            value.push(byte);
            if quoted {
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    quoted = false;
                }
            } else {
                match byte {
                    b'"' => quoted = true,
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => depth -= 1,
                    _ => {}
                }
            }
        }
        Ok(value)
    }

    pub fn json(&mut self, max: usize) -> Result<Vec<u8>, Error> {
        match self.nonspace()? {
            Some(open @ (b'[' | b'{')) => self.balanced(open, max),
            _ => Err(Error::Invalid("expected JSON value")),
        }
    }

    pub fn rows(
        &mut self,
        max_row_bytes: usize,
        mut consume: impl FnMut(&[u8]) -> Result<(), String>,
    ) -> Result<u64, Error> {
        if self.nonspace()? != Some(b'[') {
            return Err(Error::Invalid("expected array"));
        }
        let mut count = 0_u64;
        loop {
            let token = self.nonspace()?;
            if count == 0 && token == Some(b']') {
                break;
            }
            if token != Some(b'{') {
                return Err(Error::Invalid("expected row object"));
            }
            let row = self.balanced(b'{', max_row_bytes)?;
            let _: serde::de::IgnoredAny = serde_json::from_slice(&row)?;
            consume(&row).map_err(Error::Consumer)?;
            count += 1;
            match self.nonspace()? {
                Some(b',') => {}
                Some(b']') => break,
                _ => return Err(Error::Invalid("expected comma or end of array")),
            }
        }
        Ok(count)
    }

    pub fn end(mut self) -> Result<(), Error> {
        if self.nonspace()?.is_some() {
            return Err(Error::Invalid("trailing output"));
        }
        Ok(())
    }
}
