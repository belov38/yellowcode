use crate::Node;
use std::fmt;

pub const MAX_DEPTH: usize = 1024;

const BOM_BYTES: &[u8] = b"\xef\xbb\xbf";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub offset: usize,
    pub kind: ErrorKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    InvalidUtf8,
    UnexpectedClose,
    UnexpectedComma,
    UnterminatedString,
    UnterminatedList,
    MissingComma,
    TooDeep,
    Empty,
    TrailingData,
    StrayBom,
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ErrorKind::InvalidUtf8 => "invalid UTF-8",
            ErrorKind::UnexpectedClose => "unexpected '}'",
            ErrorKind::UnexpectedComma => "unexpected ','",
            ErrorKind::UnterminatedString => "unterminated string",
            ErrorKind::UnterminatedList => "unterminated list",
            ErrorKind::MissingComma => "missing comma",
            ErrorKind::TooDeep => "nesting too deep",
            ErrorKind::Empty => "no value",
            ErrorKind::TrailingData => "trailing data after the value",
            ErrorKind::StrayBom => "unexpected byte order mark",
        })
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at byte {}", self.kind, self.offset)
    }
}

impl std::error::Error for Error {}

fn err<T>(offset: usize, kind: ErrorKind) -> Result<T, Error> {
    Err(Error { offset, kind })
}

fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n' | 0x0b | 0x0c)
}

fn is_delim(b: u8) -> bool {
    matches!(b, b',' | b'{' | b'}' | b'"')
}

struct Frame {
    items: Vec<Node>,
    have_val: bool,
    open: usize,
}

struct Parser {
    stack: Vec<Frame>,
    tops: Vec<(usize, Node)>,
}

impl Parser {
    fn check_slot(&self, start: usize) -> Result<(), Error> {
        match self.stack.last() {
            Some(f) if f.have_val => err(start, ErrorKind::MissingComma),
            _ => Ok(()),
        }
    }

    fn place(&mut self, start: usize, node: Node) -> Result<(), Error> {
        self.check_slot(start)?;
        match self.stack.last_mut() {
            Some(f) => {
                f.items.push(node);
                f.have_val = true;
            }
            None => self.tops.push((start, node)),
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum ParseMode {
    Single,
    Multi,
}

fn run(text: &str, base: usize, mode: ParseMode) -> Result<Vec<(usize, Node)>, Error> {
    run_inner(text, mode).map_err(|e| Error {
        offset: e.offset.saturating_add(base),
        kind: e.kind,
    })
}

fn run_inner(text: &str, mode: ParseMode) -> Result<Vec<(usize, Node)>, Error> {
    let bytes = text.as_bytes();
    let mut p = Parser {
        stack: Vec::new(),
        tops: Vec::new(),
    };
    let mut i = 0usize;
    while let Some(&c) = bytes.get(i) {
        let start = i;
        match c {
            b'{' => {
                p.check_slot(start)?;
                if p.stack.len() >= MAX_DEPTH {
                    return err(start, ErrorKind::TooDeep);
                }
                p.stack.push(Frame {
                    items: Vec::new(),
                    have_val: false,
                    open: start,
                });
                i += 1;
            }
            b'}' => {
                let Some(mut f) = p.stack.pop() else {
                    return err(start, ErrorKind::UnexpectedClose);
                };
                if !f.have_val && !f.items.is_empty() {
                    f.items.push(Node::empty());
                }
                p.place(f.open, Node::List(f.items))?;
                i += 1;
            }
            b',' => {
                if let Some(f) = p.stack.last_mut() {
                    if !f.have_val {
                        f.items.push(Node::empty());
                    }
                    f.have_val = false;
                } else if matches!(mode, ParseMode::Single) {
                    return err(start, ErrorKind::UnexpectedComma);
                }
                i += 1;
            }
            b'"' => {
                let (s, end) = lex_string(text, start)?;
                p.place(start, Node::Str(s))?;
                i = end;
            }
            _ if is_ws(c) => i += 1,
            _ => {
                let rest = bytes.get(start..).unwrap_or_default();
                let stop: fn(u8) -> bool = if rest.starts_with(b"#base64:") {
                    is_delim
                } else {
                    |b| is_delim(b) || is_ws(b)
                };
                let len = rest.iter().position(|&b| stop(b)).unwrap_or(rest.len());
                let end = start + len;
                let atom = text.get(start..end).unwrap_or_default();
                p.place(start, Node::Atom(atom.to_owned()))?;
                i = end;
            }
        }
    }
    if let Some(f) = p.stack.last() {
        return err(f.open, ErrorKind::UnterminatedList);
    }
    Ok(p.tops)
}

fn lex_string(text: &str, start: usize) -> Result<(String, usize), Error> {
    let bytes = text.as_bytes();
    let body = start + 1;
    let mut escaped = 0usize;
    let mut i = body;
    let close = loop {
        let rest = bytes.get(i..).unwrap_or_default();
        let Some(q) = rest.iter().position(|&b| b == b'"') else {
            return err(start, ErrorKind::UnterminatedString);
        };
        let quote = i + q;
        if bytes.get(quote + 1) == Some(&b'"') {
            escaped += 1;
            i = quote + 2;
        } else {
            break quote;
        }
    };
    let raw = text.get(body..close).unwrap_or_default();
    if escaped == 0 {
        return Ok((raw.to_owned(), close + 1));
    }
    let mut out = String::with_capacity(raw.len() - escaped);
    let mut seg = 0;
    for (pair, _) in raw.match_indices("\"\"") {
        out.push_str(raw.get(seg..=pair).unwrap_or_default());
        seg = pair + 2;
    }
    out.push_str(raw.get(seg..).unwrap_or_default());
    Ok((out, close + 1))
}

fn single(mut tops: Vec<(usize, Node)>, base: usize) -> Result<Node, Error> {
    if tops.len() > 1 {
        let off = tops.get(1).map_or(0, |t| t.0);
        return err(off.saturating_add(base), ErrorKind::TrailingData);
    }
    match tops.pop() {
        Some((_, n)) => Ok(n),
        None => err(base, ErrorKind::Empty),
    }
}

pub fn parse(text: &str) -> Result<Node, Error> {
    single(run(text, 0, ParseMode::Single)?, 0)
}

pub fn parse_multi(text: &str) -> Result<Vec<Node>, Error> {
    Ok(run(text, 0, ParseMode::Multi)?
        .into_iter()
        .map(|(_, n)| n)
        .collect())
}

fn utf8(bytes: &[u8], base: usize) -> Result<&str, Error> {
    std::str::from_utf8(bytes).map_err(|e| Error {
        offset: base.saturating_add(e.valid_up_to()),
        kind: ErrorKind::InvalidUtf8,
    })
}

pub fn parse_bytes(bytes: &[u8]) -> Result<Node, Error> {
    let (base, body) = match bytes.strip_prefix(BOM_BYTES) {
        Some(b) => (BOM_BYTES.len(), b),
        None => (0, bytes),
    };
    single(run(utf8(body, base)?, base, ParseMode::Single)?, base)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Doc {
    pub root: Node,
    pub bom: bool,
    pub lead: String,
    pub trailer: String,
}

pub fn parse_doc(bytes: &[u8]) -> Result<Doc, Error> {
    let (base, body, bom) = match bytes.strip_prefix(BOM_BYTES) {
        Some(b) => (BOM_BYTES.len(), b, true),
        None => (0, bytes, false),
    };
    let text = utf8(body, base)?;
    let ws = [' ', '\t', '\r', '\n'];
    let no_lead = text.trim_start_matches(ws);
    let lead = text.get(..text.len() - no_lead.len()).unwrap_or_default();
    let mid = no_lead.trim_end_matches(ws);
    let trailer = no_lead.get(mid.len()..).unwrap_or_default();
    if mid.is_empty() {
        return err(base + lead.len(), ErrorKind::Empty);
    }
    let shift = base + lead.len();
    let root = single(run(mid, shift, ParseMode::Single)?, shift)?;
    if !bom && lead.is_empty() && matches!(&root, Node::Atom(a) if a.starts_with('\u{FEFF}')) {
        return err(shift, ErrorKind::StrayBom);
    }
    Ok(Doc {
        root,
        bom,
        lead: lead.to_owned(),
        trailer: trailer.to_owned(),
    })
}

fn trim_ws_start(b: &[u8]) -> &[u8] {
    let n = b.iter().take_while(|c| is_ws(**c)).count();
    b.get(n..).unwrap_or_default()
}

pub fn is_json_entry(bytes: &[u8]) -> bool {
    let t = bytes.strip_prefix(BOM_BYTES).unwrap_or(bytes);
    let t = trim_ws_start(t);
    let Some(rest) = t.strip_prefix(b"{") else {
        return false;
    };
    if trim_ws_start(rest).first() != Some(&b'"') {
        return false;
    }
    t.get(..200.min(t.len()))
        .is_some_and(|h| h.windows(2).any(|w| w == b"\":"))
}
