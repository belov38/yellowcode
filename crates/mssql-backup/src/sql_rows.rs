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
    fn nonspace(next: &mut impl FnMut() -> Result<Option<u8>, Error>) -> Result<Option<u8>, Error> {
        loop {
            match next()? {
                Some(b' ' | b'\t') => continue,
                byte => return Ok(byte),
            }
        }
    }
    if nonspace(&mut next)? != Some(b'[') {
        return Err(Error::Invalid("expected array"));
    }
    let mut first = true;
    loop {
        let token = nonspace(&mut next)?;
        if first && token == Some(b']') {
            break;
        }
        if token != Some(b'{') {
            return Err(Error::Invalid("expected row object"));
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
        first = false;
        match nonspace(&mut next)? {
            Some(b',') => {}
            Some(b']') => break,
            _ => return Err(Error::Invalid("expected comma or end of array")),
        }
    }
    if nonspace(&mut next)?.is_some() {
        return Err(Error::Invalid("trailing output"));
    }
    Ok(())
}
