#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]
#![deny(unsafe_op_in_unsafe_fn)]

use std::ffi::{CStr, c_int};
use std::io::{self, Read, Write};
use std::ptr::{self, NonNull};

use libz_sys as z;

mod managed;
pub use managed::{ManagedInflateError, inflate_budgeted};

pub const LEVEL_1C: u32 = 6;
pub const DEFAULT_LIMIT: u64 = 1 << 30;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("corrupt deflate stream: {0}")]
    Corrupt(String),
    #[error("deflate stream is truncated")]
    Truncated,
    #[error("data after the end of the deflate stream")]
    Trailing,
    #[error("inflated size exceeds {0} bytes")]
    TooLarge(u64),
    #[error("zlib: {0}")]
    Zlib(String),
}

impl Error {
    fn into_io(self) -> io::Error {
        let kind = match self {
            Error::Zlib(_) => io::ErrorKind::Other,
            _ => io::ErrorKind::InvalidData,
        };
        io::Error::new(kind, self)
    }
}

pub fn deflate(data: &[u8], level: u32) -> Result<Vec<u8>, Error> {
    let mut st = Stream::new_deflate(level)?;
    let mut out = Vec::new();
    let mut buf = vec![0u8; BUF_SIZE];
    let mut pos = 0usize;
    loop {
        let rest = data.get(pos..).unwrap_or_default();
        let flush = if rest.len() <= MAX_CHUNK {
            z::Z_FINISH
        } else {
            z::Z_NO_FLUSH
        };
        let (used, n, ret) = st.step(rest, &mut buf, flush);
        pos = pos.saturating_add(used);
        out.extend_from_slice(buf.get(..n).unwrap_or_default());
        match ret {
            z::Z_STREAM_END => return Ok(out),
            z::Z_OK => {}
            _ => return Err(st.error(ret)),
        }
    }
}

pub fn inflate(data: &[u8], limit: u64) -> Result<Vec<u8>, Error> {
    inflate_stream(data, limit, false)
}

pub fn inflate_prefix(data: &[u8], limit: u64) -> Result<Vec<u8>, Error> {
    inflate_stream(data, limit, true)
}

fn inflate_stream(data: &[u8], limit: u64, allow_tail: bool) -> Result<Vec<u8>, Error> {
    let mut st = Stream::new_inflate()?;
    let mut out = Vec::new();
    let mut buf = vec![0u8; BUF_SIZE];
    let mut pos = 0usize;
    let mut total = 0u64;
    loop {
        let rest = data.get(pos..).unwrap_or_default();
        let (used, n, ret) = st.step(rest, &mut buf, z::Z_NO_FLUSH);
        pos = pos.saturating_add(used);
        total = total.saturating_add(n as u64);
        if total > limit {
            return Err(Error::TooLarge(limit));
        }
        out.extend_from_slice(buf.get(..n).unwrap_or_default());
        match ret {
            z::Z_OK => {}
            z::Z_STREAM_END if !allow_tail && pos < data.len() => return Err(Error::Trailing),
            z::Z_STREAM_END => return Ok(out),
            z::Z_BUF_ERROR => return Err(Error::Truncated),
            _ => return Err(st.error(ret)),
        }
    }
}

pub fn try_inflate(data: &[u8], limit: u64) -> Option<Vec<u8>> {
    inflate(data, limit).ok()
}

pub struct DeflateWriter<W: Write> {
    st: Stream,
    w: W,
    buf: Box<[u8]>,
    done: usize,
    len: usize,
}

impl<W: Write> DeflateWriter<W> {
    pub fn new(w: W, level: u32) -> Result<Self, Error> {
        Ok(DeflateWriter {
            st: Stream::new_deflate(level)?,
            w,
            buf: vec![0u8; BUF_SIZE].into_boxed_slice(),
            done: 0,
            len: 0,
        })
    }

    pub fn finish(mut self) -> io::Result<W> {
        loop {
            self.drain()?;
            let (_, n, ret) = self.st.step(&[], &mut self.buf, z::Z_FINISH);
            self.done = 0;
            self.len = n;
            match ret {
                z::Z_STREAM_END => {
                    self.drain()?;
                    return Ok(self.w);
                }
                z::Z_OK => {}
                _ => return Err(self.st.error(ret).into_io()),
            }
        }
    }

    fn drain(&mut self) -> io::Result<()> {
        while let Some(chunk) = self.buf.get(self.done..self.len).filter(|c| !c.is_empty()) {
            match self.w.write(chunk) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(n) => self.done = self.done.saturating_add(n),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        self.done = 0;
        self.len = 0;
        Ok(())
    }
}

impl<W: Write> Write for DeflateWriter<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if data.is_empty() {
            return Ok(0);
        }
        self.drain()?;
        let mut pos = 0usize;
        loop {
            let rest = data.get(pos..).unwrap_or_default();
            let (used, n, ret) = self.st.step(rest, &mut self.buf, z::Z_NO_FLUSH);
            pos = pos.saturating_add(used);
            self.done = 0;
            self.len = n;
            match ret {
                z::Z_OK => {}
                z::Z_BUF_ERROR if pos >= data.len() => {}
                _ => return Err(self.st.error(ret).into_io()),
            }
            if let Err(e) = self.drain() {
                return if pos > 0 { Ok(pos) } else { Err(e) };
            }
            if pos >= data.len() && n < self.buf.len() {
                return Ok(data.len());
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.drain()?;
        self.w.flush()
    }
}

pub struct InflateReader<R: Read> {
    st: Stream,
    r: R,
    buf: Box<[u8]>,
    pos: usize,
    len: usize,
    eof: bool,
    ended: bool,
}

impl<R: Read> InflateReader<R> {
    pub fn new(r: R) -> Result<Self, Error> {
        Ok(InflateReader {
            st: Stream::new_inflate()?,
            r,
            buf: vec![0u8; BUF_SIZE].into_boxed_slice(),
            pos: 0,
            len: 0,
            eof: false,
            ended: false,
        })
    }

    fn fill(&mut self) -> io::Result<()> {
        loop {
            match self.r.read(&mut self.buf) {
                Ok(n) => {
                    self.pos = 0;
                    self.len = n.min(self.buf.len());
                    self.eof = n == 0;
                    return Ok(());
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }
}

impl<R: Read> Read for InflateReader<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        loop {
            if self.pos >= self.len && !self.eof {
                self.fill()?;
            }
            let input = self.buf.get(self.pos..self.len).unwrap_or_default();
            if self.ended {
                return if input.is_empty() {
                    Ok(0)
                } else {
                    Err(Error::Trailing.into_io())
                };
            }
            let (used, n, ret) = self.st.step(input, out, z::Z_NO_FLUSH);
            self.pos = self.pos.saturating_add(used);
            match ret {
                z::Z_OK => {}
                z::Z_STREAM_END => self.ended = true,
                z::Z_BUF_ERROR if self.eof => return Err(Error::Truncated.into_io()),
                z::Z_BUF_ERROR => {}
                _ => return Err(self.st.error(ret).into_io()),
            }
            if n > 0 {
                return Ok(n);
            }
        }
    }
}

const MAX_CHUNK: usize = 1 << 30;
const BUF_SIZE: usize = 64 * 1024;
const WINDOW_BITS: c_int = -15;
const MEM_LEVEL: c_int = 9;
const STREAM_SIZE: c_int = std::mem::size_of::<z::z_stream>() as c_int;

struct Stream {
    s: NonNull<z::z_stream>,
    deflate: bool,
    managed: Option<memory_budget::Charged<Vec<z::z_stream>>>,
}

unsafe impl Send for Stream {}

impl Stream {
    fn new_deflate(level: u32) -> Result<Self, Error> {
        let level = c_int::try_from(level)
            .ok()
            .filter(|l| (0..=9).contains(l))
            .ok_or_else(|| Error::Zlib(format!("invalid compression level {level}")))?;
        Self::init(Some(level))
    }

    fn new_inflate() -> Result<Self, Error> {
        Self::init(None)
    }

    fn init(level: Option<c_int>) -> Result<Self, Error> {
        let s = NonNull::from(Box::leak(Box::new(z::z_stream {
            next_in: ptr::null_mut(),
            avail_in: 0,
            total_in: 0,
            next_out: ptr::null_mut(),
            avail_out: 0,
            total_out: 0,
            msg: ptr::null_mut(),
            state: ptr::null_mut(),
            zalloc,
            zfree,
            opaque: ptr::null_mut(),
            data_type: 0,
            adler: 0,
            reserved: 0,
        })));
        let ret = unsafe {
            match level {
                Some(level) => z::deflateInit2_(
                    s.as_ptr(),
                    level,
                    z::Z_DEFLATED,
                    WINDOW_BITS,
                    MEM_LEVEL,
                    z::Z_DEFAULT_STRATEGY,
                    z::zlibVersion(),
                    STREAM_SIZE,
                ),
                None => z::inflateInit2_(s.as_ptr(), WINDOW_BITS, z::zlibVersion(), STREAM_SIZE),
            }
        };
        if ret == z::Z_OK {
            return Ok(Stream {
                s,
                deflate: level.is_some(),
                managed: None,
            });
        }
        let msg = unsafe {
            let msg = zlib_message(s.as_ptr(), ret);
            drop(Box::from_raw(s.as_ptr()));
            msg
        };
        Err(Error::Zlib(msg))
    }

    fn step(&mut self, input: &[u8], output: &mut [u8], flush: c_int) -> (usize, usize, c_int) {
        let in_len = input.len().min(MAX_CHUNK);
        let out_len = output.len().min(MAX_CHUNK);
        let s = self.s.as_ptr();
        unsafe {
            (*s).next_in = input.as_ptr().cast_mut();
            (*s).avail_in = in_len as z::uInt;
            (*s).next_out = output.as_mut_ptr();
            (*s).avail_out = out_len as z::uInt;
            (*s).msg = ptr::null_mut();
            let ret = if self.deflate {
                z::deflate(s, flush)
            } else {
                z::inflate(s, flush)
            };
            let consumed = in_len.saturating_sub((*s).avail_in as usize);
            let produced = out_len.saturating_sub((*s).avail_out as usize);
            (*s).next_in = ptr::null_mut();
            (*s).avail_in = 0;
            (*s).next_out = ptr::null_mut();
            (*s).avail_out = 0;
            (consumed, produced, ret)
        }
    }

    fn error(&self, ret: c_int) -> Error {
        let msg = unsafe { zlib_message(self.s.as_ptr(), ret) };
        match ret {
            z::Z_DATA_ERROR | z::Z_NEED_DICT => Error::Corrupt(msg),
            _ => Error::Zlib(msg),
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        let s = self.s.as_ptr();
        unsafe {
            if self.deflate {
                z::deflateEnd(s);
            } else {
                z::inflateEnd(s);
            }
            if self.managed.is_none() {
                drop(Box::from_raw(s));
            }
        }
    }
}

unsafe fn zlib_message(s: *const z::z_stream, ret: c_int) -> String {
    let msg = unsafe { (*s).msg };
    if !msg.is_null() {
        return unsafe { CStr::from_ptr(msg) }
            .to_string_lossy()
            .into_owned();
    }
    match ret {
        z::Z_STREAM_ERROR => "stream error".into(),
        z::Z_DATA_ERROR => "data error".into(),
        z::Z_MEM_ERROR => "insufficient memory".into(),
        z::Z_BUF_ERROR => "buffer error".into(),
        z::Z_VERSION_ERROR => "incompatible version".into(),
        z::Z_NEED_DICT => "preset dictionary required".into(),
        other => format!("return code {other}"),
    }
}

const ALIGN: usize = 16;

extern "C" fn zalloc(_opaque: z::voidpf, items: z::uInt, size: z::uInt) -> z::voidpf {
    let Some(n) = (items as usize)
        .checked_mul(size as usize)
        .and_then(|n| n.checked_add(ALIGN))
    else {
        return std::ptr::null_mut();
    };
    let Ok(layout) = std::alloc::Layout::from_size_align(n, ALIGN) else {
        return std::ptr::null_mut();
    };
    unsafe {
        let p = std::alloc::alloc_zeroed(layout);
        if p.is_null() {
            return std::ptr::null_mut();
        }
        p.cast::<usize>().write(n);
        p.add(ALIGN).cast()
    }
}

unsafe extern "C" fn zfree(_opaque: z::voidpf, ptr: z::voidpf) {
    if ptr.is_null() {
        return;
    }
    unsafe {
        let base = ptr.cast::<u8>().sub(ALIGN);
        let n = base.cast::<usize>().read();
        std::alloc::dealloc(
            base,
            std::alloc::Layout::from_size_align_unchecked(n, ALIGN),
        );
    }
}
