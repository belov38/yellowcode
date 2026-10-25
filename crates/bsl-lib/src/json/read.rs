use crate::error::{LibError, msg};
use bsl_prim::Number;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq)]
pub enum Token {
    StartObject,
    EndObject,
    StartArray,
    EndArray,
    PropertyName(Vec<u16>),
    Null,
    Bool(bool),
    Number(Number),
    String(Vec<u16>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Number(Number),
    String(Vec<u16>),
    Array(Vec<Value>),
    Object(Vec<(Vec<u16>, Value)>),
}

const MAX_DEPTH: usize = 1_000_000;
const MAX_VALUE_DEPTH: usize = 1000;
const MAX_PADDING: u64 = 100_000;
const PADDING_PER_UNIT: u64 = 64;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Frame {
    Array,
    Object,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum St {
    Value,
    ArrItem,
    ArrAfter,
    ObjKey,
    ObjColon,
    ObjAfter,
    Done,
}

pub struct OwnedReader {
    text: Arc<[u16]>,
    pos: usize,
    stack: Vec<Frame>,
    st: St,
    failed: Option<LibError>,
    padding_left: u64,
}

fn bad_state() -> LibError {
    LibError::new(msg::BAD_STATE)
}

fn unexpected() -> LibError {
    LibError::new(msg::UNEXPECTED)
}

fn is_digit(c: u16) -> bool {
    (0x30..=0x39).contains(&c)
}

fn hex_val(c: u16) -> Option<u16> {
    match c {
        0x30..=0x39 => Some(c - 0x30),
        0x41..=0x46 => Some(c - 0x41 + 10),
        0x61..=0x66 => Some(c - 0x61 + 10),
        _ => None,
    }
}

impl OwnedReader {
    pub fn new(text: Arc<[u16]>) -> OwnedReader {
        let units = u64::try_from(text.len()).unwrap_or(u64::MAX);
        OwnedReader {
            text,
            pos: 0,
            stack: Vec::new(),
            st: St::Value,
            failed: None,
            padding_left: units
                .saturating_mul(PADDING_PER_UNIT)
                .saturating_add(MAX_PADDING),
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<Option<Token>, LibError> {
        if let Some(e) = &self.failed {
            return Err(e.clone());
        }
        match self.step() {
            Ok(t) => Ok(t),
            Err(e) => {
                self.failed = Some(e.clone());
                Err(e)
            }
        }
    }

    pub fn next_value_token(&mut self) -> Result<Option<Token>, LibError> {
        if self.failed.is_none() && self.st == St::Done {
            self.skip_ws();
            if self.peek().is_some() {
                self.st = St::Value;
            }
        }
        self.next()
    }

    fn peek(&self) -> Option<u16> {
        self.text.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(0x20 | 0x09 | 0x0A | 0x0D)) {
            self.pos += 1;
        }
    }

    fn misplaced(&self, c: Option<u16>) -> LibError {
        match c {
            None => bad_state(),
            Some(c) => match char::from_u32(u32::from(c)) {
                Some('{' | '}' | '[' | ']' | ',' | ':' | '"' | '\'' | '/' | '-' | '0'..='9') => {
                    bad_state()
                }
                _ => unexpected(),
            },
        }
    }

    fn after_value(&mut self) {
        self.st = match self.stack.last() {
            None => St::Done,
            Some(Frame::Array) => St::ArrAfter,
            Some(Frame::Object) => St::ObjAfter,
        };
    }

    fn open(&mut self, f: Frame) -> Result<(), LibError> {
        if self.stack.len() >= MAX_DEPTH {
            return Err(bad_state());
        }
        self.stack.push(f);
        self.pos += 1;
        self.st = if f == Frame::Array {
            St::ArrItem
        } else {
            St::ObjKey
        };
        Ok(())
    }

    fn close(&mut self, t: Token) -> Option<Token> {
        self.stack.pop();
        self.pos += 1;
        self.after_value();
        Some(t)
    }

    fn step(&mut self) -> Result<Option<Token>, LibError> {
        loop {
            if self.st == St::Done {
                return Ok(None);
            }
            self.skip_ws();
            let c = self.peek();
            match self.st {
                St::Done => return Ok(None),
                St::Value | St::ArrItem => {
                    if self.st == St::ArrItem && c == Some(u16::from(b']')) {
                        return Ok(self.close(Token::EndArray));
                    }
                    return self.value(c).map(Some);
                }
                St::ArrAfter => match c {
                    Some(0x2C) => {
                        self.pos += 1;
                        self.st = St::ArrItem;
                    }
                    Some(0x5D) => return Ok(self.close(Token::EndArray)),
                    _ => return Err(self.misplaced(c)),
                },
                St::ObjKey => match c {
                    Some(0x7D) => return Ok(self.close(Token::EndObject)),
                    Some(q @ (0x22 | 0x27)) => {
                        let s = self.string(q)?;
                        self.st = St::ObjColon;
                        return Ok(Some(Token::PropertyName(s)));
                    }
                    Some(c) if self.misplaced(Some(c)).key == msg::BAD_STATE => {
                        return Err(bad_state());
                    }
                    Some(_) => return Err(unexpected()),
                    None => return Err(bad_state()),
                },
                St::ObjColon => match c {
                    Some(0x3A) => {
                        self.pos += 1;
                        self.st = St::Value;
                    }
                    _ => return Err(self.misplaced(c)),
                },
                St::ObjAfter => match c {
                    Some(0x2C) => {
                        self.pos += 1;
                        self.st = St::ObjKey;
                    }
                    Some(0x7D) => return Ok(self.close(Token::EndObject)),
                    _ => return Err(self.misplaced(c)),
                },
            }
        }
    }

    fn value(&mut self, c: Option<u16>) -> Result<Token, LibError> {
        let Some(c) = c else {
            return Err(bad_state());
        };
        match char::from_u32(u32::from(c)) {
            Some('{') => {
                self.open(Frame::Object)?;
                Ok(Token::StartObject)
            }
            Some('[') => {
                self.open(Frame::Array)?;
                Ok(Token::StartArray)
            }
            Some('"' | '\'') => {
                let s = self.string(c)?;
                self.after_value();
                Ok(Token::String(s))
            }
            Some('-' | '0'..='9') => {
                let n = self.number()?;
                self.after_value();
                Ok(Token::Number(n))
            }
            Some('t') => self.literal("true", Token::Bool(true)),
            Some('f') => self.literal("false", Token::Bool(false)),
            Some('n') => self.literal("null", Token::Null),
            _ => Err(self.misplaced(Some(c))),
        }
    }

    fn literal(&mut self, word: &str, t: Token) -> Result<Token, LibError> {
        let end = self.pos + word.len();
        let ok = self
            .text
            .get(self.pos..end)
            .is_some_and(|w| w.iter().copied().eq(word.bytes().map(u16::from)));
        if !ok {
            return Err(unexpected());
        }
        if self.text.get(end).is_some_and(|&c| {
            is_digit(c) || char::from_u32(u32::from(c)).is_some_and(char::is_alphabetic)
        }) {
            return Err(unexpected());
        }
        self.pos = end;
        self.after_value();
        Ok(t)
    }

    fn string(&mut self, quote: u16) -> Result<Vec<u16>, LibError> {
        self.pos += 1;
        let mut out = Vec::new();
        loop {
            let Some(c) = self.peek() else {
                return Err(bad_state());
            };
            self.pos += 1;
            if c == quote {
                return Ok(out);
            }
            if c != 0x5C {
                out.push(c);
                continue;
            }
            let Some(e) = self.peek() else {
                return Err(bad_state());
            };
            self.pos += 1;
            let unit = match char::from_u32(u32::from(e)) {
                Some('"') => 0x22,
                Some('\'') => 0x27,
                Some('\\') => 0x5C,
                Some('/') => 0x2F,
                Some('b') => 0x08,
                Some('f') => 0x0C,
                Some('n') => 0x0A,
                Some('r') => 0x0D,
                Some('t') => 0x09,
                Some('u') => {
                    let mut v = 0u16;
                    for _ in 0..4 {
                        let Some(h) = self.peek() else {
                            return Err(bad_state());
                        };
                        let Some(d) = hex_val(h) else {
                            return Err(unexpected());
                        };
                        v = v << 4 | d;
                        self.pos += 1;
                    }
                    v
                }
                _ => return Err(unexpected()),
            };
            out.push(unit);
        }
    }

    fn number(&mut self) -> Result<Number, LibError> {
        let mut neg = false;
        if self.peek() == Some(0x2D) {
            neg = true;
            self.pos += 1;
        }
        let int = self.digits();
        if int.is_empty() {
            return Err(unexpected());
        }
        let mut frac = Vec::new();
        if self.peek() == Some(0x2E) {
            self.pos += 1;
            frac = self.digits();
            if frac.is_empty() {
                return Err(unexpected());
            }
        }
        let mut exp: i64 = 0;
        if matches!(self.peek(), Some(0x45 | 0x65)) {
            self.pos += 1;
            let mut eneg = false;
            match self.peek() {
                Some(0x2B) => self.pos += 1,
                Some(0x2D) => {
                    eneg = true;
                    self.pos += 1;
                }
                _ => {}
            }
            let ed = self.digits();
            if ed.is_empty() {
                return Err(unexpected());
            }
            for d in ed {
                exp = (exp * 10 + i64::from(d - b'0')).min(i64::from(u32::MAX));
            }
            if eneg {
                exp = -exp;
            }
        }
        let mut digits = int;
        let frac_len = i64::try_from(frac.len()).map_err(|_| unexpected())?;
        digits.extend(frac);
        let lit_len = digits.len() as u64;
        let first = digits.iter().position(|&d| d != b'0');
        let Some(first) = first else {
            return Ok(Number::from_u64(0));
        };
        let digits = &digits[first..];
        let sig = digits.len() as u64;
        let shift = exp - frac_len;
        let out_len = match u64::try_from(shift) {
            Ok(z) => sig.saturating_add(z),
            Err(_) if shift.unsigned_abs() < sig => sig,
            Err(_) => shift.unsigned_abs().saturating_add(1),
        };
        let padding = out_len.saturating_sub(lit_len);
        if padding > MAX_PADDING || padding > self.padding_left {
            return Err(unexpected());
        }
        self.padding_left -= padding;
        let cap = usize::try_from(out_len).map_err(|_| unexpected())?;
        let mut text = String::with_capacity(cap.saturating_add(3));
        if neg {
            text.push('-');
        }
        let ascii = |d: &[u8]| d.iter().map(|&b| b as char).collect::<String>();
        if shift >= 0 {
            let zeros = usize::try_from(shift).map_err(|_| unexpected())?;
            text.push_str(&ascii(digits));
            text.extend(std::iter::repeat_n('0', zeros));
        } else {
            let k = usize::try_from(shift.unsigned_abs()).map_err(|_| unexpected())?;
            if k < digits.len() {
                let (a, b) = digits.split_at(digits.len() - k);
                text.push_str(&ascii(a));
                text.push('.');
                text.push_str(&ascii(b));
            } else {
                text.push_str("0.");
                text.extend(std::iter::repeat_n('0', k - digits.len()));
                text.push_str(&ascii(digits));
            }
        }
        Number::parse(&text).ok_or_else(unexpected)
    }

    fn digits(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        while let Some(c) = self.peek().filter(|&c| is_digit(c)) {
            out.push(c as u8);
            self.pos += 1;
        }
        out
    }
}

pub struct Reader(OwnedReader);

impl Reader {
    pub fn new(text: &[u16]) -> Reader {
        Reader(OwnedReader::new(Arc::from(text)))
    }

    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<Option<Token>, LibError> {
        self.0.next()
    }
}

enum Build {
    Array(Vec<Value>),
    Object {
        items: Vec<(Vec<u16>, Value)>,
        index: HashMap<Vec<u16>, usize>,
        key: Option<Vec<u16>>,
    },
}

fn place(stack: &mut [Build], v: Value) -> Option<Value> {
    match stack.last_mut() {
        None => Some(v),
        Some(Build::Array(a)) => {
            a.push(v);
            None
        }
        Some(Build::Object { items, index, key }) => {
            let k = key.take().unwrap_or_default();
            match index.get(&k) {
                Some(&i) => {
                    if let Some(slot) = items.get_mut(i) {
                        slot.1 = v;
                    }
                }
                None => {
                    index.insert(k.clone(), items.len());
                    items.push((k, v));
                }
            }
            None
        }
    }
}

pub fn read_value(text: &[u16]) -> Result<Value, LibError> {
    let mut r = Reader::new(text);
    let mut stack: Vec<Build> = Vec::new();
    loop {
        let Some(t) = r.next()? else {
            return Err(bad_state());
        };
        let done = match t {
            Token::StartArray | Token::StartObject => {
                if stack.len() >= MAX_VALUE_DEPTH {
                    return Err(bad_state());
                }
                stack.push(if t == Token::StartArray {
                    Build::Array(Vec::new())
                } else {
                    Build::Object {
                        items: Vec::new(),
                        index: HashMap::new(),
                        key: None,
                    }
                });
                None
            }
            Token::EndArray | Token::EndObject => {
                let v = match stack.pop() {
                    Some(Build::Array(a)) => Value::Array(a),
                    Some(Build::Object { items, .. }) => Value::Object(items),
                    None => return Err(bad_state()),
                };
                place(&mut stack, v)
            }
            Token::PropertyName(k) => {
                if let Some(Build::Object { key, .. }) = stack.last_mut() {
                    *key = Some(k);
                }
                None
            }
            Token::Null => place(&mut stack, Value::Null),
            Token::Bool(b) => place(&mut stack, Value::Bool(b)),
            Token::Number(n) => place(&mut stack, Value::Number(n)),
            Token::String(s) => place(&mut stack, Value::String(s)),
        };
        if let Some(v) = done {
            return Ok(v);
        }
    }
}
