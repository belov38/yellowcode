use crate::ast::DateLit;
use crate::diag::{MsgId, msg};
use crate::keywords;
pub use crate::keywords::Kw;
use crate::source::{SourceText, Span};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TokenKind {
    Ident,
    Kw(Kw),
    Number,
    Str(u32),
    Date(DateLit),
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    LParen,
    RParen,
    LBracket,
    RBracket,
    Comma,
    Semicolon,
    Dot,
    Colon,
    Question,
    Tilde,
    Amp,
    Directive,
    Unknown,
    Bad(MsgId),
    Eof,
}

#[derive(Clone, Debug)]
pub struct Lexed {
    pub tokens: Vec<Token>,
    pub strings: Vec<Box<[u16]>>,
}

fn off(i: usize) -> u32 {
    u32::try_from(i).unwrap_or(u32::MAX)
}

fn is_break(c: u16) -> bool {
    c == 0x0A || c == 0x0D
}

pub(crate) fn is_space(c: u16) -> bool {
    matches!(
        c,
        0x0009..=0x000D | 0x001C..=0x0020 | 0x0085 | 0x00A0 | 0x1680 | 0x180E
            | 0x2000..=0x200A | 0x2028..=0x2029 | 0x202F | 0x205F | 0x3000
    )
}

const fn ascii_set(s: &[u8]) -> u128 {
    let mut m = 0u128;
    let mut i = 0;
    while i < s.len() {
        m |= 1 << s[i];
        i += 1;
    }
    m
}

const WORD_ENDS: u128 = ascii_set(b" \t\r\n+-*/%<>=()[],;.?~:&\"'#|");

pub(crate) fn ends_word(c: u16) -> bool {
    c < 0x80 && (WORD_ENDS >> c) & 1 != 0
}

fn is_digit(c: u16) -> bool {
    (0x30..=0x39).contains(&c)
}

fn is_cyrillic_letter(c: u16) -> bool {
    (0x0400..=0x0481).contains(&c)
}

pub(crate) fn is_ident_start(c: u16) -> bool {
    if c < 0x80 {
        return c == 0x5F || (c as u8).is_ascii_alphabetic();
    }
    is_cyrillic_letter(c) || char::from_u32(u32::from(c)).is_some_and(char::is_alphabetic)
}

pub(crate) fn is_ident_part(c: u16) -> bool {
    if c < 0x80 {
        return c == 0x5F || (c as u8).is_ascii_alphanumeric();
    }
    is_cyrillic_letter(c) || char::from_u32(u32::from(c)).is_some_and(char::is_alphanumeric)
}

struct Lexer<'a> {
    u: &'a [u16],
    i: usize,
    strings: Vec<Box<[u16]>>,
}

impl Lexer<'_> {
    fn at(&self, j: usize) -> Option<u16> {
        self.u.get(j).copied()
    }

    fn line_end(&self, mut j: usize) -> usize {
        while self.at(j).is_some_and(|c| !is_break(c)) {
            j += 1;
        }
        j
    }

    fn break_len(&self, j: usize) -> usize {
        match self.at(j) {
            Some(0x0D) if self.at(j + 1) == Some(0x0A) => 2,
            Some(c) if is_break(c) => 1,
            _ => 0,
        }
    }

    fn skip_trivia_from(&self, mut j: usize) -> usize {
        while let Some(c) = self.at(j) {
            if is_space(c) {
                j += 1;
            } else if c == 0x2F && self.at(j + 1) == Some(0x2F) {
                j = self.line_end(j);
            } else {
                break;
            }
        }
        j
    }

    fn ident_end(&self, mut j: usize) -> usize {
        while self.at(j).is_some_and(is_ident_part) {
            j += 1;
        }
        j
    }

    fn glued_end(&self, mut j: usize) -> usize {
        while self.at(j).is_some_and(|c| !ends_word(c)) {
            j += 1;
        }
        j
    }

    fn digits_end(&self, mut j: usize) -> usize {
        while self.at(j).is_some_and(is_digit) {
            j += 1;
        }
        j
    }

    fn bad_number_end(&self, mut j: usize) -> usize {
        loop {
            j = self.glued_end(j);
            if self.at(j) == Some(0x2E) && self.at(j + 1).is_some_and(is_digit) {
                j += 1;
            } else {
                return j;
            }
        }
    }

    fn number(&mut self, start: usize) -> TokenKind {
        let mut j = self.digits_end(start);
        if self.at(j) == Some(0x2E) {
            j = self.digits_end(j + 1);
        }
        let bad = match self.at(j) {
            None => false,
            Some(0x2E) => self.at(j + 1).is_some_and(is_digit),
            Some(c) => !ends_word(c),
        };
        if bad {
            self.i = self.bad_number_end(j);
            TokenKind::Bad(msg::BAD_NUMCONST)
        } else {
            self.i = j;
            TokenKind::Number
        }
    }

    fn segment(&self, i: &mut usize, val: &mut Vec<u16>) -> Result<(), usize> {
        loop {
            let Some(c) = self.at(*i) else {
                return Err(self.u.len());
            };
            match c {
                0x22 => {
                    if self.at(*i + 1) == Some(0x22) {
                        val.push(0x22);
                        *i += 2;
                    } else {
                        *i += 1;
                        return Ok(());
                    }
                }
                0x0A | 0x0D => {
                    let end = *i;
                    let mut j = *i + self.break_len(*i);
                    loop {
                        let mut k = j;
                        while self.at(k).is_some_and(|c| is_space(c) && !is_break(c)) {
                            k += 1;
                        }
                        match self.at(k) {
                            Some(0x7C) => {
                                val.push(0x0A);
                                *i = k + 1;
                                break;
                            }
                            Some(c) if is_break(c) => j = k + self.break_len(k),
                            Some(0x2F) if self.at(k + 1) == Some(0x2F) => {
                                let e = self.line_end(k);
                                j = e + self.break_len(e);
                                if j == e {
                                    return Err(end);
                                }
                            }
                            _ => return Err(end),
                        }
                    }
                }
                _ => {
                    val.push(c);
                    *i += 1;
                }
            }
        }
    }

    fn string(&mut self, start: usize) -> TokenKind {
        let mut val: Vec<u16> = Vec::new();
        let mut i = start + 1;
        loop {
            if let Err(end) = self.segment(&mut i, &mut val) {
                self.i = end;
                return TokenKind::Bad(msg::MIS_DOUBLEQUOT);
            }
            let j = self.skip_trivia_from(i);
            if self.at(j) == Some(0x22) {
                val.push(0x0A);
                i = j + 1;
            } else {
                break;
            }
        }
        self.i = i;
        let idx = off(self.strings.len());
        self.strings.push(val.into_boxed_slice());
        TokenKind::Str(idx)
    }

    fn date(&mut self, start: usize) -> TokenKind {
        let mut j = start + 1;
        let mut digits = [0u8; 14];
        let mut n = 0usize;
        while let Some(c) = self.at(j) {
            if c == 0x27 || is_break(c) {
                break;
            }
            if is_digit(c) {
                if let Some(d) = digits.get_mut(n) {
                    *d = (c - 0x30) as u8;
                }
                n += 1;
            }
            j += 1;
        }
        if self.at(j) != Some(0x27) {
            self.i = j;
            return TokenKind::Bad(msg::MIS_SINGLEQUOT);
        }
        self.i = j + 1;
        match digits.get(..n).and_then(date_lit) {
            Some(d) => TokenKind::Date(d),
            None => TokenKind::Bad(msg::BAD_DATECONST),
        }
    }

    fn word(&mut self, start: usize) -> TokenKind {
        let end = self.ident_end(start + 1);
        if self.at(end).is_some_and(|c| !ends_word(c)) {
            self.i = self.glued_end(end);
            return TokenKind::Unknown;
        }
        self.i = end;
        match self.u.get(start..end).and_then(keywords::lookup) {
            Some(kw) => TokenKind::Kw(kw),
            None => TokenKind::Ident,
        }
    }

    fn unknown(&mut self, c: u16, start: usize) -> TokenKind {
        let pair = (0xD800..0xDC00).contains(&c)
            && self
                .at(start + 1)
                .is_some_and(|d| (0xDC00..0xE000).contains(&d));
        self.i = start + if pair { 2 } else { 1 };
        TokenKind::Unknown
    }

    fn next_kind(&mut self, c: u16, start: usize) -> TokenKind {
        let next = self.at(start + 1);
        let (kind, len) = match c {
            0x2B => (TokenKind::Plus, 1),
            0x2D => (TokenKind::Minus, 1),
            0x2A => (TokenKind::Star, 1),
            0x2F => (TokenKind::Slash, 1),
            0x25 => (TokenKind::Percent, 1),
            0x3D => (TokenKind::Eq, 1),
            0x3C => match next {
                Some(0x3D) => (TokenKind::Le, 2),
                Some(0x3E) => (TokenKind::Ne, 2),
                _ => (TokenKind::Lt, 1),
            },
            0x3E => match next {
                Some(0x3D) => (TokenKind::Ge, 2),
                _ => (TokenKind::Gt, 1),
            },
            0x28 => (TokenKind::LParen, 1),
            0x29 => (TokenKind::RParen, 1),
            0x5B => (TokenKind::LBracket, 1),
            0x5D => (TokenKind::RBracket, 1),
            0x2C => (TokenKind::Comma, 1),
            0x3B => (TokenKind::Semicolon, 1),
            0x2E => {
                if next.is_some_and(is_digit) {
                    self.i = self.bad_number_end(start + 1);
                    return TokenKind::Bad(msg::BAD_NUMCONST);
                }
                (TokenKind::Dot, 1)
            }
            0x3F => (TokenKind::Question, 1),
            0x7E => (TokenKind::Tilde, 1),
            0x3A => (TokenKind::Colon, 1),
            0x26 => (TokenKind::Amp, 1),
            0x23 => {
                self.i = self.line_end(start);
                return TokenKind::Directive;
            }
            0x22 => return self.string(start),
            0x27 => return self.date(start),
            _ if is_digit(c) => return self.number(start),
            _ if is_ident_start(c) => return self.word(start),
            _ => return self.unknown(c, start),
        };
        self.i = start + len;
        kind
    }
}

fn days_in_month(year: u16, month: u8) -> u8 {
    match month {
        2 if year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)) => {
            29
        }
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

fn date_lit(d: &[u8]) -> Option<DateLit> {
    if !matches!(d.len(), 8 | 12 | 14) {
        return None;
    }
    if d.iter().all(|&x| x == 0) {
        return Some(DateLit {
            year: 0,
            month: 0,
            day: 0,
            hour: 0,
            minute: 0,
            second: 0,
        });
    }
    let two = |k: usize| -> u8 {
        let a = d.get(k).copied().unwrap_or(0);
        let b = d.get(k + 1).copied().unwrap_or(0);
        a * 10 + b
    };
    let year = u16::from(two(0)) * 100 + u16::from(two(2));
    let (month, day, hour, minute, second) = (two(4), two(6), two(8), two(10), two(12));
    let ok = (1..=9999).contains(&year)
        && (1..=12).contains(&month)
        && (1..=days_in_month(year, month)).contains(&day)
        && hour < 24
        && minute < 60
        && second < 60;
    ok.then_some(DateLit {
        year,
        month,
        day,
        hour,
        minute,
        second,
    })
}

pub fn tokenize(src: &SourceText) -> Lexed {
    let u = src.units();
    let mut lx = Lexer {
        u,
        i: 0,
        strings: Vec::new(),
    };
    let mut tokens = Vec::with_capacity(u.len() / 4 + 1);
    loop {
        let start = lx.skip_trivia_from(lx.i);
        let Some(c) = lx.at(start) else {
            break;
        };
        let kind = lx.next_kind(c, start);
        tokens.push(Token {
            kind,
            span: Span {
                start: off(start),
                end: off(lx.i),
            },
        });
    }
    let end = off(u.len());
    tokens.push(Token {
        kind: TokenKind::Eof,
        span: Span { start: end, end },
    });
    Lexed {
        tokens,
        strings: lx.strings,
    }
}
