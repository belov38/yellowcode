use std::convert::Infallible;
use std::io::{self, Write};
use std::slice;

use super::{
    Atom, AtomValue, BASE_MASK, BinNode, F_CLOSE, F_COMMA, F_OPEN, T_BSTR8, T_NEG_U8, T_NEG_U16,
    T_NEG_U32, T_NEG_U64, T_U8, T_U16, T_U32, T_U64, T_WSTR8,
};
use crate::Guid;

const FLUSH_AT: usize = 64 * 1024;

struct Enc {
    buf: Vec<u8>,
    pend: u8,
    has_prev: bool,
    depth: usize,
}

impl Enc {
    fn new() -> Enc {
        Enc {
            buf: Vec::new(),
            pend: 0,
            has_prev: false,
            depth: 0,
        }
    }

    fn flush_pend(&mut self) {
        self.buf.push(self.pend);
        self.pend = 0;
    }

    fn comma(&mut self) {
        if self.has_prev {
            if self.pend & !F_CLOSE != 0 {
                self.flush_pend();
            }
            self.pend |= F_COMMA;
            self.has_prev = false;
        }
    }

    fn open(&mut self) {
        if self.pend & (F_OPEN | BASE_MASK) != 0 {
            self.flush_pend();
        }
        self.comma();
        self.pend |= F_OPEN;
        self.depth = self.depth.saturating_add(1);
    }

    fn close(&mut self) {
        if self.pend != 0 {
            self.buf.push(self.pend);
        }
        self.pend = F_CLOSE;
        self.has_prev = true;
        self.depth = self.depth.saturating_sub(1);
    }

    fn finish(&mut self) {
        if self.pend != 0 {
            self.flush_pend();
        }
    }

    fn atom(&mut self, a: &Atom) {
        self.comma();
        if self.pend & BASE_MASK != 0 {
            self.flush_pend();
        }
        self.buf.push(self.pend | a.tag());
        self.pend = 0;
        self.has_prev = true;
        self.payload(a);
    }

    fn payload(&mut self, a: &Atom) {
        let tag = a.tag();
        match a.value() {
            AtomValue::Int(v) => {
                let n = match tag {
                    T_U8 | T_NEG_U8 => 1,
                    T_U16 | T_NEG_U16 => 2,
                    T_U32 | T_NEG_U32 => 4,
                    T_U64 | T_NEG_U64 => 8,
                    _ => 0,
                };
                self.buf
                    .extend_from_slice(v.unsigned_abs().to_le_bytes().get(..n).unwrap_or(&[]));
            }
            AtomValue::F32(bits) => self.buf.extend_from_slice(&bits.to_le_bytes()),
            AtomValue::F64(bits) => self.buf.extend_from_slice(&bits.to_le_bytes()),
            AtomValue::Guid(g) => self.buf.extend_from_slice(&g.to_bytes_le()),
            AtomValue::Numeric(inner) => {
                self.buf.push(inner.tag());
                self.payload(inner);
            }
            AtomValue::Wide(u) => {
                self.length(tag, T_WSTR8, u.len());
                for unit in u {
                    self.buf.extend_from_slice(&unit.to_le_bytes());
                }
            }
            AtomValue::Narrow(b) => {
                self.length(tag, T_BSTR8, b.len());
                self.buf.extend_from_slice(b);
            }
        }
    }

    fn length(&mut self, tag: u8, first: u8, len: usize) {
        let n = u64::try_from(len).unwrap_or(u64::MAX).to_le_bytes();
        let width = match tag.saturating_sub(first) {
            0 => 1,
            1 => 2,
            _ => 8,
        };
        self.buf.extend_from_slice(n.get(..width).unwrap_or(&n));
    }
}

fn encode_node<E>(
    enc: &mut Enc,
    n: &BinNode,
    mut after: impl FnMut(&mut Enc) -> Result<(), E>,
) -> Result<(), E> {
    let mut stack: Vec<slice::Iter<'_, BinNode>> = Vec::new();
    match n {
        BinNode::Atom(a) => enc.atom(a),
        BinNode::List(v) => {
            enc.open();
            stack.push(v.iter());
        }
    }
    after(enc)?;
    while let Some(top) = stack.last_mut() {
        match top.next() {
            Some(BinNode::Atom(a)) => enc.atom(a),
            Some(BinNode::List(v)) => {
                enc.open();
                stack.push(v.iter());
            }
            None => {
                stack.pop();
                enc.close();
            }
        }
        after(enc)?;
    }
    Ok(())
}

fn flush<W: Write>(enc: &mut Enc, w: &mut W) -> io::Result<()> {
    if enc.buf.len() >= FLUSH_AT {
        w.write_all(&enc.buf)?;
        enc.buf.clear();
    }
    Ok(())
}

fn invalid(msg: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg)
}

pub struct Writer<W: Write> {
    enc: Enc,
    w: W,
}

impl<W: Write> Writer<W> {
    pub fn new(w: W) -> Self {
        Writer { enc: Enc::new(), w }
    }

    pub fn open(&mut self) -> io::Result<()> {
        self.enc.open();
        flush(&mut self.enc, &mut self.w)
    }

    pub fn close(&mut self) -> io::Result<()> {
        if self.enc.depth == 0 {
            return Err(invalid("close without an open list"));
        }
        self.enc.close();
        flush(&mut self.enc, &mut self.w)
    }

    pub fn atom(&mut self, a: &Atom) -> io::Result<()> {
        self.enc.atom(a);
        flush(&mut self.enc, &mut self.w)
    }

    pub fn atom_joined(&mut self, a: &Atom) -> io::Result<()> {
        self.enc.has_prev = false;
        self.atom(a)
    }

    pub fn int(&mut self, v: i128) -> io::Result<()> {
        self.atom(&Atom::int(v))
    }

    pub fn bool(&mut self, v: bool) -> io::Result<()> {
        self.int(i128::from(v))
    }

    pub fn str(&mut self, s: &str) -> io::Result<()> {
        self.atom(&Atom::str(s))
    }

    pub fn utf16(&mut self, u: &[u16]) -> io::Result<()> {
        self.atom(&Atom::utf16(u))
    }

    pub fn bytes(&mut self, b: &[u8]) -> io::Result<()> {
        self.atom(&Atom::bytes(b))
    }

    pub fn guid(&mut self, g: &Guid) -> io::Result<()> {
        self.atom(&Atom::guid(*g))
    }

    pub fn numeric_text(&mut self, t: &str) -> io::Result<()> {
        let a = Atom::numeric_text(t).ok_or_else(|| invalid("invalid numeric text"))?;
        self.atom(&a)
    }

    pub fn f32(&mut self, v: f32) -> io::Result<()> {
        self.atom(&Atom::f32(v))
    }

    pub fn f64(&mut self, v: f64) -> io::Result<()> {
        self.atom(&Atom::f64(v))
    }

    pub fn node(&mut self, n: &BinNode) -> io::Result<()> {
        let Writer { enc, w } = self;
        encode_node(enc, n, |e| flush(e, w))
    }

    pub fn finish(mut self) -> io::Result<W> {
        if self.enc.depth != 0 {
            return Err(invalid("unclosed list"));
        }
        self.enc.finish();
        self.w.write_all(&self.enc.buf)?;
        Ok(self.w)
    }
}

pub fn encode_all(items: &[BinNode]) -> Vec<u8> {
    let mut enc = Enc::new();
    for n in items {
        let r: Result<(), Infallible> = encode_node(&mut enc, n, |_| Ok(()));
        let Ok(()) = r;
    }
    enc.finish();
    enc.buf
}
