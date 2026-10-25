use crate::error::{Constraint, Error, Result};

const MAX_DEPTH: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Node {
    Str(String),
    Atom(String),
    List(Vec<Node>),
}

pub(crate) fn bad_descr(why: impl Into<String>) -> Error {
    Error::Constraint(Constraint::BadDescr(why.into()))
}

pub(crate) fn parse_brace(text: &str) -> Result<Node> {
    let mut p = Parser {
        text,
        bytes: text.as_bytes(),
        pos: 0,
    };
    p.skip_ws();
    let node = p.value(0)?;
    p.skip_ws();
    if p.pos < p.bytes.len() {
        return Err(p.err("unexpected text after the value"));
    }
    Ok(node)
}

struct Parser<'a> {
    text: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n')
}

fn is_delim(b: u8) -> bool {
    is_ws(b) || matches!(b, b',' | b'{' | b'}' | b'"')
}

impl Parser<'_> {
    fn err(&self, what: &str) -> Error {
        bad_descr(format!("bracket text: {what} at byte {}", self.pos))
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while self.peek().is_some_and(is_ws) {
            self.pos += 1;
        }
    }

    fn slice(&self, from: usize, to: usize) -> Result<&str> {
        self.text
            .get(from..to)
            .ok_or_else(|| self.err("split inside a character"))
    }

    fn value(&mut self, depth: usize) -> Result<Node> {
        self.skip_ws();
        match self.peek() {
            None => Err(self.err("unexpected end")),
            Some(b'{') => self.list(depth + 1),
            Some(b'"') => self.string().map(Node::Str),
            Some(b',' | b'}') => Err(self.err("unexpected delimiter")),
            Some(_) => self.atom().map(Node::Atom),
        }
    }

    fn list(&mut self, depth: usize) -> Result<Node> {
        if depth > MAX_DEPTH {
            return Err(self.err("lists are nested too deeply"));
        }
        self.pos += 1;
        self.skip_ws();
        let mut items = Vec::new();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Node::List(items));
        }
        loop {
            items.push(self.value(depth)?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Node::List(items));
                }
                Some(_) => return Err(self.err("expected `,` or `}`")),
                None => return Err(self.err("unterminated list")),
            }
        }
    }

    fn string(&mut self) -> Result<String> {
        self.pos += 1;
        let mut out = String::new();
        loop {
            let start = self.pos;
            let len = self
                .bytes
                .get(start..)
                .and_then(|rest| rest.iter().position(|&b| b == b'"'))
                .ok_or_else(|| self.err("unterminated string"))?;
            let end = start + len;
            out.push_str(self.slice(start, end)?);
            self.pos = end + 1;
            if self.peek() == Some(b'"') {
                out.push('"');
                self.pos += 1;
            } else {
                return Ok(out);
            }
        }
    }

    fn atom(&mut self) -> Result<String> {
        let start = self.pos;
        while self.peek().is_some_and(|b| !is_delim(b)) {
            self.pos += 1;
        }
        if self.pos == start {
            return Err(self.err("empty token"));
        }
        self.slice(start, self.pos).map(str::to_string)
    }
}
