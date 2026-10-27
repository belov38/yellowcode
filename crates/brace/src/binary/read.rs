use std::fmt;
use std::io::{self, Read, Write};

use super::{Atom, BinNode, F_CLOSE, F_COMMA, F_OPEN};
use crate::MAX_DEPTH;

const CHUNK: usize = 64 * 1024;

#[derive(Debug, Clone, Copy)]
pub struct ReadLimits {
    pub atom_bytes: u64,
    pub retained_bytes: u64,
    pub nodes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Open,
    Close,
    Atom(Atom),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub offset: u64,
    pub kind: ErrorKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    UnknownTag(u8),
    UnexpectedEof,
    Unbalanced,
    BadNumericTag(u8),
    TooDeep,
    Limit(&'static str),
    InvalidBinary,
    Io(io::ErrorKind),
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ErrorKind::UnknownTag(t) => write!(f, "unknown tag {t:#04x}"),
            ErrorKind::UnexpectedEof => f.write_str("unexpected end of stream"),
            ErrorKind::Unbalanced => f.write_str("unbalanced '}'"),
            ErrorKind::BadNumericTag(t) => write!(f, "bad numeric text tag {t:#04x}"),
            ErrorKind::TooDeep => f.write_str("nesting too deep"),
            ErrorKind::Limit(what) => write!(f, "read limit: {what}"),
            ErrorKind::InvalidBinary => f.write_str("invalid binary chunk list"),
            ErrorKind::Io(k) => write!(f, "I/O error: {k}"),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at byte {}", self.kind, self.offset)
    }
}

impl std::error::Error for Error {}

pub(super) struct Src<R> {
    inner: R,
    buf: Vec<u8>,
    pos: usize,
    end: usize,
    offset: u64,
    limits: Option<ReadLimits>,
    remaining_bytes: u64,
    remaining_nodes: u64,
}

impl<R: Read> Src<R> {
    pub(super) fn charge_payload(&mut self, n: u64) -> Result<(), ErrorKind> {
        if let Some(limits) = self.limits {
            if n > limits.atom_bytes {
                return Err(ErrorKind::Limit("atom bytes"));
            }
            if n > self.remaining_bytes {
                return Err(ErrorKind::Limit("retained bytes"));
            }
            self.remaining_bytes -= n;
        }
        Ok(())
    }
    pub(super) fn charge_wide(&mut self, units: u64) -> Result<(), ErrorKind> {
        if self.limits.is_some() {
            self.charge_payload(
                units
                    .checked_mul(2)
                    .ok_or(ErrorKind::Limit("wide byte overflow"))?,
            )?;
        }
        Ok(())
    }
    fn charge_node(&mut self) -> Result<(), ErrorKind> {
        if self.limits.is_some() {
            if self.remaining_nodes == 0 {
                return Err(ErrorKind::Limit("nodes"));
            }
            self.remaining_nodes -= 1;
        }
        Ok(())
    }

    fn fill(&mut self) -> io::Result<bool> {
        while self.pos >= self.end {
            match self.inner.read(&mut self.buf) {
                Ok(0) => return Ok(false),
                Ok(n) => {
                    self.pos = 0;
                    self.end = n.min(self.buf.len());
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(true)
    }

    pub(super) fn byte(&mut self) -> io::Result<Option<u8>> {
        let mut b = None;
        self.payload(1, |s| b = s.first().copied())?;
        Ok(b)
    }

    pub(super) fn fixed<const N: usize>(&mut self) -> io::Result<Option<[u8; N]>> {
        let mut out = [0u8; N];
        let mut at = 0usize;
        let done = self.payload(N as u64, |s| {
            let end = at.saturating_add(s.len());
            if let Some(d) = out.get_mut(at..end) {
                d.copy_from_slice(s);
            }
            at = end;
        })?;
        Ok(done.then_some(out))
    }

    pub(super) fn payload(&mut self, n: u64, mut sink: impl FnMut(&[u8])) -> io::Result<bool> {
        self.try_payload(n, |piece| {
            sink(piece);
            Ok(())
        })
    }

    pub(super) fn try_payload(
        &mut self,
        mut n: u64,
        mut sink: impl FnMut(&[u8]) -> io::Result<()>,
    ) -> io::Result<bool> {
        while n > 0 {
            if !self.fill()? {
                return Ok(false);
            }
            let avail = self.buf.get(self.pos..self.end).unwrap_or_default();
            let k = usize::try_from(n).map_or(avail.len(), |n| n.min(avail.len()));
            let Some(piece) = avail.get(..k).filter(|p| !p.is_empty()) else {
                return Ok(false);
            };
            sink(piece)?;
            self.pos = self.pos.saturating_add(k);
            self.offset = self.offset.saturating_add(k as u64);
            n = n.saturating_sub(k as u64);
        }
        Ok(true)
    }
}

pub struct Reader<R: Read> {
    src: Src<R>,
    pend: u8,
    tag_off: u64,
    comma: bool,
    prev_elem: bool,
    depth: usize,
    missing_commas: u64,
    peeked: Option<Option<Event>>,
    failed: Option<Error>,
}

impl<R: Read> Reader<R> {
    pub fn new(r: R) -> Self {
        Reader {
            src: Src {
                inner: r,
                buf: vec![0; CHUNK],
                pos: 0,
                end: 0,
                offset: 0,
                limits: None,
                remaining_bytes: 0,
                remaining_nodes: 0,
            },
            pend: 0,
            tag_off: 0,
            comma: false,
            prev_elem: false,
            depth: 0,
            missing_commas: 0,
            peeked: None,
            failed: None,
        }
    }

    pub fn begin_budget(&mut self, limits: ReadLimits) {
        self.src.limits = Some(limits);
        self.src.remaining_bytes = limits.retained_bytes;
        self.src.remaining_nodes = limits.nodes;
    }

    pub fn next_event(&mut self) -> Result<Option<Event>, Error> {
        self.guard(|r| r.pull(false))
    }

    pub fn peek_event(&mut self) -> Result<Option<&Event>, Error> {
        if self.peeked.is_none() {
            let e = self.guard(|r| r.lex(false))?;
            self.peeked = Some(e);
        } else if let Some(e) = &self.failed {
            return Err(e.clone());
        }
        Ok(self.peeked.as_ref().and_then(Option::as_ref))
    }

    pub fn read_value(&mut self) -> Result<Option<BinNode>, Error> {
        self.guard(Self::read_value_inner)
    }

    fn read_value_inner(&mut self) -> Result<Option<BinNode>, Error> {
        let mut stack: Vec<Vec<BinNode>> = Vec::new();
        let mut next = self.start_value(false)?;
        loop {
            let done = match next {
                None if stack.is_empty() => return Ok(None),
                None => return Err(self.eof()),
                Some(Event::Open) => {
                    stack.push(Vec::new());
                    None
                }
                Some(Event::Close) => stack.pop().map(BinNode::List),
                Some(Event::Atom(a)) => Some(BinNode::Atom(a)),
            };
            if let Some(node) = done {
                match stack.last_mut() {
                    Some(parent) => parent.push(node),
                    None => return Ok(Some(node)),
                }
            }
            next = self.pull(false)?;
        }
    }

    pub fn copy_binary(&mut self, sink: &mut impl Write, limit: u64) -> Result<u64, Error> {
        self.guard(|r| {
            if r.pull(false)? != Some(Event::Open)
                || !matches!(r.pull(false)?, Some(Event::Atom(a)) if a.as_int() == Some(0))
            {
                return Err(r.fail(ErrorKind::InvalidBinary));
            }
            let mut copied = 0u64;
            loop {
                let mut n = 0;
                match r.lex_to(false, Some((sink, limit - copied, &mut n)))? {
                    Some(Event::Close) => return Ok(copied),
                    Some(Event::Atom(_)) => copied += n,
                    None => return Err(r.eof()),
                    _ => return Err(r.fail(ErrorKind::InvalidBinary)),
                }
            }
        })
    }

    pub fn skip_value(&mut self) -> Result<bool, Error> {
        self.guard(Self::skip_value_inner)
    }

    fn skip_value_inner(&mut self) -> Result<bool, Error> {
        let mut level = 0usize;
        let mut next = self.start_value(true)?;
        loop {
            match next {
                None if level == 0 => return Ok(false),
                None => return Err(self.eof()),
                Some(Event::Open) => level = level.saturating_add(1),
                Some(Event::Close) => level = level.saturating_sub(1),
                Some(Event::Atom(_)) => {}
            }
            if level == 0 {
                return Ok(true);
            }
            next = self.pull(true)?;
        }
    }

    pub fn offset(&self) -> u64 {
        self.src.offset
    }

    pub fn depth(&self) -> usize {
        match &self.peeked {
            Some(Some(Event::Open)) => self.depth.saturating_sub(1),
            Some(Some(Event::Close)) => self.depth.saturating_add(1),
            _ => self.depth,
        }
    }

    pub fn missing_commas(&self) -> u64 {
        self.missing_commas
    }

    pub fn into_inner(self) -> R {
        self.src.inner
    }

    fn guard<T>(&mut self, f: impl FnOnce(&mut Self) -> Result<T, Error>) -> Result<T, Error> {
        if let Some(e) = &self.failed {
            return Err(e.clone());
        }
        let r = f(self);
        if let Err(e) = &r {
            self.failed = Some(e.clone());
        }
        r
    }

    fn pull(&mut self, skip: bool) -> Result<Option<Event>, Error> {
        match self.peeked.take() {
            Some(e) => Ok(e),
            None => self.lex(skip),
        }
    }

    fn start_value(&mut self, skip: bool) -> Result<Option<Event>, Error> {
        match self.pull(skip)? {
            end @ (None | Some(Event::Close)) => {
                self.peeked = Some(end);
                Ok(None)
            }
            e => Ok(e),
        }
    }

    fn eof(&self) -> Error {
        Error {
            offset: self.src.offset,
            kind: ErrorKind::UnexpectedEof,
        }
    }

    fn fail(&self, kind: ErrorKind) -> Error {
        Error {
            offset: self.tag_off,
            kind,
        }
    }

    fn lex(&mut self, skip: bool) -> Result<Option<Event>, Error> {
        self.lex_to(skip, None)
    }

    fn lex_to(
        &mut self,
        skip: bool,
        sink: Option<(&mut dyn Write, u64, &mut u64)>,
    ) -> Result<Option<Event>, Error> {
        loop {
            if self.pend == 0 {
                let at = self.src.offset;
                let b = self.src.byte().map_err(|e| Error {
                    offset: at,
                    kind: ErrorKind::Io(e.kind()),
                })?;
                let Some(b) = b else {
                    return if self.depth > 0 {
                        Err(self.eof())
                    } else {
                        Ok(None)
                    };
                };
                self.pend = b;
                self.tag_off = at;
            }
            let t = self.pend;
            if t & F_CLOSE != 0 {
                if self.depth == 0 {
                    return Err(self.fail(ErrorKind::Unbalanced));
                }
                self.pend = t & !F_CLOSE;
                self.depth -= 1;
                self.prev_elem = true;
                return Ok(Some(Event::Close));
            }
            if t & F_COMMA != 0 {
                self.pend = t & !F_COMMA;
                self.comma = true;
                continue;
            }
            if t & F_OPEN != 0 {
                if self.depth >= MAX_DEPTH {
                    return Err(self.fail(ErrorKind::TooDeep));
                }
                self.src.charge_node().map_err(|kind| self.fail(kind))?;
                self.pend = t & !F_OPEN;
                self.element();
                self.depth += 1;
                self.prev_elem = false;
                return Ok(Some(Event::Open));
            }
            self.pend = 0;
            if t == 0 {
                continue;
            }
            let at = self.tag_off;
            let atom = if let Some((sink, limit, copied)) = sink {
                *copied = Atom::copy_bytes(&mut self.src, t, sink, limit)
                    .map_err(|kind| Error { offset: at, kind })?;
                Atom::int(0)
            } else {
                self.src.charge_node().map_err(|kind| self.fail(kind))?;
                Atom::read(&mut self.src, t, skip).map_err(|kind| Error { offset: at, kind })?
            };
            self.element();
            self.prev_elem = true;
            return Ok(Some(Event::Atom(atom)));
        }
    }

    fn element(&mut self) {
        if self.prev_elem && !self.comma {
            self.missing_commas = self.missing_commas.saturating_add(1);
        }
        self.comma = false;
    }
}

pub fn decode_all(bytes: &[u8]) -> Result<Vec<BinNode>, Error> {
    let mut r = Reader::new(bytes);
    let mut out = Vec::new();
    while let Some(v) = r.read_value()? {
        out.push(v);
    }
    Ok(out)
}
