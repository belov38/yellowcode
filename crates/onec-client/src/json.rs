#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Json {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Json::Null => "null",
            Json::Bool(_) => "a boolean",
            Json::Number(_) => "a number",
            Json::String(_) => "a string",
            Json::Array(_) => "an array",
            Json::Object(_) => "an object",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JsonError {
    pub(crate) offset: usize,
    pub(crate) message: &'static str,
}

const MAX_DEPTH: usize = 64;

pub(crate) fn parse(body: &[u8]) -> Result<Json, JsonError> {
    let skip = if body.starts_with(&[0xEF, 0xBB, 0xBF]) {
        3
    } else {
        0
    };
    let text = std::str::from_utf8(&body[skip..]).map_err(|e| JsonError {
        offset: skip + e.valid_up_to(),
        message: "not UTF-8",
    })?;
    let mut p = Parser {
        s: text.as_bytes(),
        at: 0,
        base: skip,
    };
    p.ws();
    let v = p.value(0)?;
    p.ws();
    if p.at < p.s.len() {
        return Err(p.err("text after the value"));
    }
    Ok(v)
}

struct Parser<'a> {
    s: &'a [u8],
    at: usize,
    base: usize,
}

impl Parser<'_> {
    fn err(&self, message: &'static str) -> JsonError {
        JsonError {
            offset: self.base + self.at,
            message,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.at).copied()
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    fn word(&mut self, word: &[u8]) -> Result<(), JsonError> {
        if self.s[self.at..].starts_with(word) {
            self.at += word.len();
            Ok(())
        } else {
            Err(self.err("bad literal"))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, JsonError> {
        match self.peek() {
            None => Err(self.err("unexpected end")),
            Some(b'{') => self.object(depth + 1),
            Some(b'[') => self.array(depth + 1),
            Some(b'"') => Ok(Json::String(self.string()?)),
            Some(b't') => self.word(b"true").map(|()| Json::Bool(true)),
            Some(b'f') => self.word(b"false").map(|()| Json::Bool(false)),
            Some(b'n') => self.word(b"null").map(|()| Json::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => Err(self.err("unexpected character")),
        }
    }

    fn object(&mut self, depth: usize) -> Result<Json, JsonError> {
        if depth > MAX_DEPTH {
            return Err(self.err("nested too deep"));
        }
        self.at += 1;
        let mut members = Vec::new();
        self.ws();
        if self.peek() == Some(b'}') {
            self.at += 1;
            return Ok(Json::Object(members));
        }
        loop {
            self.ws();
            if self.peek() != Some(b'"') {
                return Err(self.err("expected a key"));
            }
            let key = self.string()?;
            self.ws();
            if self.peek() != Some(b':') {
                return Err(self.err("expected ':'"));
            }
            self.at += 1;
            self.ws();
            let v = self.value(depth)?;
            members.push((key, v));
            self.ws();
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b'}') => {
                    self.at += 1;
                    return Ok(Json::Object(members));
                }
                _ => return Err(self.err("expected ',' or '}'")),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<Json, JsonError> {
        if depth > MAX_DEPTH {
            return Err(self.err("nested too deep"));
        }
        self.at += 1;
        let mut items = Vec::new();
        self.ws();
        if self.peek() == Some(b']') {
            self.at += 1;
            return Ok(Json::Array(items));
        }
        loop {
            self.ws();
            items.push(self.value(depth)?);
            self.ws();
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b']') => {
                    self.at += 1;
                    return Ok(Json::Array(items));
                }
                _ => return Err(self.err("expected ',' or ']'")),
            }
        }
    }

    fn digits(&mut self) -> Result<(), JsonError> {
        let start = self.at;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.at += 1;
        }
        if self.at == start {
            return Err(self.err("expected a digit"));
        }
        Ok(())
    }

    fn number(&mut self) -> Result<Json, JsonError> {
        let start = self.at;
        if self.peek() == Some(b'-') {
            self.at += 1;
        }
        if self.peek() == Some(b'0') {
            self.at += 1;
            if matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.err("leading zero"));
            }
        } else {
            self.digits()?;
        }
        if self.peek() == Some(b'.') {
            self.at += 1;
            self.digits()?;
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.at += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.at += 1;
            }
            self.digits()?;
        }
        let text = String::from_utf8_lossy(&self.s[start..self.at]).into_owned();
        Ok(Json::Number(text))
    }

    fn hex4(&mut self) -> Result<u32, JsonError> {
        let mut v = 0u32;
        for _ in 0..4 {
            let d = match self.peek() {
                Some(c @ b'0'..=b'9') => c - b'0',
                Some(c @ b'a'..=b'f') => c - b'a' + 10,
                Some(c @ b'A'..=b'F') => c - b'A' + 10,
                _ => return Err(self.err("bad \\u escape")),
            };
            v = v * 16 + u32::from(d);
            self.at += 1;
        }
        Ok(v)
    }

    fn string(&mut self) -> Result<String, JsonError> {
        self.at += 1;
        let mut out = String::new();
        loop {
            let run = self.at;
            while matches!(self.peek(), Some(c) if c != b'"' && c != b'\\' && c >= 0x20) {
                self.at += 1;
            }
            out.push_str(&String::from_utf8_lossy(&self.s[run..self.at]));
            match self.peek() {
                None => return Err(self.err("unterminated string")),
                Some(b'"') => {
                    self.at += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.at += 1;
                    let c = match self.peek() {
                        Some(b'"') => '"',
                        Some(b'\\') => '\\',
                        Some(b'/') => '/',
                        Some(b'b') => '\u{8}',
                        Some(b'f') => '\u{c}',
                        Some(b'n') => '\n',
                        Some(b'r') => '\r',
                        Some(b't') => '\t',
                        Some(b'u') => {
                            self.at += 1;
                            out.push(self.unicode()?);
                            continue;
                        }
                        _ => return Err(self.err("bad escape")),
                    };
                    self.at += 1;
                    out.push(c);
                }
                Some(_) => return Err(self.err("control character in a string")),
            }
        }
    }

    fn unicode(&mut self) -> Result<char, JsonError> {
        let at = self.at;
        let hi = self.hex4()?;
        let code = match hi {
            0xD800..=0xDBFF => {
                if !self.s[self.at..].starts_with(b"\\u") {
                    return Err(self.err("lone surrogate"));
                }
                self.at += 2;
                let lo = self.hex4()?;
                if !(0xDC00..=0xDFFF).contains(&lo) {
                    return Err(self.err("lone surrogate"));
                }
                0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
            }
            0xDC00..=0xDFFF => {
                self.at = at;
                return Err(self.err("lone surrogate"));
            }
            c => c,
        };
        char::from_u32(code).ok_or_else(|| self.err("bad \\u escape"))
    }
}
