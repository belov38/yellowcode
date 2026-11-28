use std::io::{Read, Write};

use super::NativeError;

pub const GREETING: [u8; 5] = [0x53, 0xF5, 0xC6, 0x1A, 0x7B];
pub const M0: [u8; 4] = [0x65, 0x52, 0xB1, 0xA5];
pub const M1: [u8; 4] = [0x66, 0x53, 0xB2, 0xA6];
pub const M2: [u8; 4] = [0x21, 0x47, 0x54, 0xB3];
pub const M3: [u8; 4] = [0x22, 0x48, 0x55, 0xB5];
const MARKERS: [[u8; 4]; 4] = [M0, M1, M2, M3];

pub const MAX_MESSAGE: usize = crate::transport::MAX_RESPONSE_BODY + 1024 * 1024;
const READ_CHUNK: usize = 32 * 1024;
pub const MAX_CONTROL: usize = 1024;

pub fn escape(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 4);
    let mut i = 0;
    while i < payload.len() {
        match payload.get(i..i + 4) {
            Some(q) if MARKERS.iter().any(|m| m == q) => {
                out.extend_from_slice(&M0);
                out.extend_from_slice(q);
                i += 4;
            }
            _ => {
                out.push(payload[i]);
                i += 1;
            }
        }
    }
    out.extend_from_slice(&M1);
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    Message {
        payload: Vec<u8>,
        pings: u64,
        signals: u64,
    },
    Closed,
    Silent,
    Failed(String),
}

pub struct MessageStream<S> {
    io: S,
    buf: Vec<u8>,
    pos: usize,
    max_message: usize,
    pings: u64,
    signals: u64,
    last_signal: Option<[u8; 16]>,
}

impl<S: Read + Write> MessageStream<S> {
    pub fn new(io: S) -> Self {
        Self::with_limit(io, MAX_MESSAGE)
    }

    pub fn with_limit(io: S, max_message: usize) -> Self {
        Self {
            io,
            buf: Vec::new(),
            pos: 0,
            max_message,
            pings: 0,
            signals: 0,
            last_signal: None,
        }
    }

    pub fn pings(&self) -> u64 {
        self.pings
    }

    pub fn signals(&self) -> u64 {
        self.signals
    }

    pub fn last_signal(&self) -> Option<[u8; 16]> {
        self.last_signal
    }

    pub fn get_ref(&self) -> &S {
        &self.io
    }

    pub fn expect_greeting(&mut self) -> Result<(), NativeError> {
        self.need(GREETING.len())?;
        let got = &self.buf[self.pos..self.pos + GREETING.len()];
        if got != GREETING {
            return Err(NativeError::NotOneC(hex(got)));
        }
        self.pos += GREETING.len();
        Ok(())
    }

    pub fn send_signal(&mut self, id: &[u8; 16]) -> Result<(), NativeError> {
        let mut b = M3.to_vec();
        b.extend_from_slice(id);
        self.io.write_all(&b)?;
        self.io.flush()?;
        Ok(())
    }

    pub fn send_message(&mut self, payload: &[u8]) -> Result<(), NativeError> {
        self.io.write_all(&escape(payload))?;
        self.io.flush()?;
        Ok(())
    }

    pub fn read_message(&mut self) -> Result<Vec<u8>, NativeError> {
        let mut msg = Vec::new();
        let mut control = 0usize;
        loop {
            self.need(4)?;
            let at = self.pos;
            match MARKERS.iter().position(|m| self.buf[at..at + 4] == *m) {
                Some(0) => {
                    self.need(8)?;
                    msg.extend_from_slice(&self.buf[self.pos + 4..self.pos + 8]);
                    self.pos += 8;
                }
                Some(1) => {
                    self.pos += 4;
                    self.compact();
                    return Ok(msg);
                }
                Some(2) => {
                    control += 1;
                    self.pos += 4;
                    self.pings += 1;
                }
                Some(_) => {
                    control += 1;
                    self.need(20)?;
                    let mut id = [0u8; 16];
                    id.copy_from_slice(&self.buf[self.pos + 4..self.pos + 20]);
                    self.signals += 1;
                    self.last_signal = Some(id);
                    self.pos += 20;
                }
                None => {
                    let end = self.buf[at + 1..]
                        .iter()
                        .position(|b| matches!(b, 0x65 | 0x66 | 0x21 | 0x22))
                        .map_or(self.buf.len(), |i| at + 1 + i);
                    msg.extend_from_slice(&self.buf[at..end]);
                    self.pos = end;
                }
            }
            if msg.len() > self.max_message {
                return Err(NativeError::TooLarge("message", self.max_message));
            }
            if control > MAX_CONTROL {
                return Err(NativeError::TooLarge("pings and signals", MAX_CONTROL));
            }
        }
    }

    pub fn trail(&mut self, max: usize) -> Vec<Next> {
        let mut out = Vec::new();
        while out.len() < max {
            let (pings, signals) = (self.pings, self.signals);
            match self.read_message() {
                Ok(m) => out.push(Next::Message {
                    payload: m,
                    pings: self.pings - pings,
                    signals: self.signals - signals,
                }),
                Err(NativeError::Closed) => {
                    out.push(Next::Closed);
                    break;
                }
                Err(NativeError::Io(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    out.push(Next::Silent);
                    break;
                }
                Err(e) => {
                    out.push(Next::Failed(e.to_string()));
                    break;
                }
            }
        }
        out
    }

    fn need(&mut self, n: usize) -> Result<(), NativeError> {
        while self.buf.len() - self.pos < n {
            self.compact();
            let mut chunk = [0u8; READ_CHUNK];
            let got = self.io.read(&mut chunk)?;
            if got == 0 {
                return Err(NativeError::Closed);
            }
            self.buf.extend_from_slice(&chunk[..got]);
        }
        Ok(())
    }

    fn compact(&mut self) {
        self.buf.drain(..self.pos);
        self.pos = 0;
    }
}

pub(crate) fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
