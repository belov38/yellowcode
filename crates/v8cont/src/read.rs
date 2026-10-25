use crate::error::{Error, Result};
use crate::format::{FileHeader, Format, PageHeader, parse_entry_header};
use crate::source::ReadAt;
use crate::time::V8Time;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageRec(pub u64, pub u64, pub u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryInfo {
    pub name: String,
    pub ctime: V8Time,
    pub mtime: V8Time,
    pub attrs: u32,
    pub header_addr: u64,
    pub data_addr: Option<u64>,
    pub data_len: u64,
}

fn corrupt(offset: u64, what: &str) -> Error {
    Error::Corrupt {
        offset,
        what: what.to_string(),
    }
}

fn check_format<R: ReadAt + ?Sized>(src: &R, size: u64, f: Format) -> io::Result<bool> {
    let (base, fh, ph) = (f.base(), f.file_header_size(), f.page_header_size());
    if size < base + fh {
        return Ok(false);
    }
    let mut header = [0u8; 20];
    let hb = header.get_mut(..fh as usize).unwrap_or_default();
    src.read_exact_at(hb, base)?;
    let Some(h) = FileHeader::parse(f, hb) else {
        return Ok(false);
    };
    if size < base + fh + ph {
        return Ok(h.next_free_page == f.undefined());
    }
    let mut page = [0u8; 55];
    let pb = page.get_mut(..ph as usize).unwrap_or_default();
    src.read_exact_at(pb, base + fh)?;
    Ok(PageHeader::parse(f, pb).is_some())
}

pub fn detect<R: ReadAt + ?Sized>(src: &R) -> io::Result<Option<Format>> {
    let size = src.size()?;
    for f in [Format::F16, Format::F15] {
        if check_format(src, size, f)? {
            return Ok(Some(f));
        }
    }
    Ok(None)
}

pub fn is_container(bytes: &[u8]) -> bool {
    matches!(detect(bytes), Ok(Some(_)))
}

#[derive(Debug)]
pub struct Container<R: ReadAt> {
    src: R,
    format: Format,
    header: FileHeader,
    size: u64,
    entries: Vec<EntryInfo>,
}

impl<R: ReadAt> Container<R> {
    pub fn open(src: R) -> Result<Self> {
        let format = detect(&src)?.ok_or(Error::NotContainer)?;
        let size = src.size()?;
        let mut hb = vec![0u8; format.file_header_size() as usize];
        src.read_exact_at(&mut hb, format.base())?;
        let header =
            FileHeader::parse(format, &hb).ok_or_else(|| corrupt(format.base(), "file header"))?;
        let mut c = Container {
            src,
            format,
            header,
            size,
            entries: Vec::new(),
        };
        if c.has_toc() {
            c.entries = c.load_entries()?;
        }
        Ok(c)
    }

    pub fn format(&self) -> Format {
        self.format
    }
    pub fn header(&self) -> &FileHeader {
        &self.header
    }
    pub fn entries(&self) -> &[EntryInfo] {
        &self.entries
    }
    pub fn find(&self, name: &str) -> Option<&EntryInfo> {
        self.entries.iter().find(|e| e.name == name)
    }
    pub fn source(&self) -> &R {
        &self.src
    }

    pub fn file_len(&self) -> u64 {
        self.size
    }

    pub(crate) fn has_toc(&self) -> bool {
        let f = self.format;
        self.size >= f.base() + f.file_header_size() + f.page_header_size()
    }

    pub fn read_data(&self, e: &EntryInfo) -> Result<Vec<u8>> {
        match e.data_addr {
            Some(a) => self.read_chain(a),
            None => Ok(Vec::new()),
        }
    }

    pub fn data_reader(&self, e: &EntryInfo) -> ChainReader<'_, R> {
        ChainReader::new(self, e.data_addr)
    }

    pub fn read_chain(&self, addr: u64) -> Result<Vec<u8>> {
        self.read_chain_counted(addr).map(|(v, _)| v)
    }

    fn read_chain_counted(&self, addr: u64) -> Result<(Vec<u8>, u64)> {
        let mut r = ChainReader::new(self, Some(addr));
        let mut out = Vec::new();
        let mut buf = vec![0u8; 1 << 16];
        loop {
            let n = io::Read::read(&mut r, &mut buf).map_err(from_io)?;
            if n == 0 {
                return Ok((out, r.seen.len() as u64));
            }
            out.extend_from_slice(buf.get(..n).unwrap_or_default());
        }
    }

    pub fn placeholder(&self) -> Result<Vec<u8>> {
        let mut v = vec![0u8; self.format.base() as usize];
        self.src.read_exact_at(&mut v, 0)?;
        Ok(v)
    }

    fn page_header(&self, addr: u64) -> Result<PageHeader> {
        #[cfg(feature = "testing")]
        crate::testing::probe::hit();
        let ph = self.format.page_header_size();
        let abs = self
            .format
            .base()
            .checked_add(addr)
            .filter(|a| a.checked_add(ph).is_some_and(|e| e <= self.size))
            .ok_or_else(|| corrupt(addr, "page beyond the end of the file"))?;
        let mut b = vec![0u8; ph as usize];
        self.src.read_exact_at(&mut b, abs)?;
        PageHeader::parse(self.format, &b).ok_or_else(|| corrupt(abs, "bad page header"))
    }

    pub(crate) fn chain_pages(
        &self,
        addr: u64,
        chain: usize,
        owners: &mut HashMap<u64, usize>,
    ) -> Result<Vec<PageRec>> {
        let max = self.size / self.format.page_header_size();
        let mut out = Vec::new();
        let mut cur = addr;
        loop {
            let at = self.format.base().saturating_add(cur);
            match owners.insert(cur, chain) {
                Some(c) if c == chain => return Err(corrupt(at, "page loop")),
                Some(_) => return Err(corrupt(at, "overlapping pages")),
                None if owners.len() as u64 > max => return Err(corrupt(at, "overlapping pages")),
                None => {}
            }
            let h = self.page_header(cur)?;
            out.push(PageRec(cur, h.page_size, h.data_size));
            if h.next_page == self.format.undefined() {
                return Ok(out);
            }
            cur = h.next_page;
        }
    }

    fn load_entries(&self) -> Result<Vec<EntryInfo>> {
        let f = self.format;
        let toc = self.read_chain(f.file_header_size())?;
        let asz = f.addr_size() as usize;
        let rd = |b: &[u8], i: usize| -> Option<u64> {
            let s = b.get(i * asz..(i + 1) * asz)?;
            let mut w = [0u8; 8];
            w.get_mut(..asz)?.copy_from_slice(s);
            Some(u64::from_le_bytes(w))
        };
        let mut out = Vec::new();
        let mut budget = self.size;
        let mut owners = HashMap::new();
        for (i, rec) in toc.chunks_exact(f.toc_record_size() as usize).enumerate() {
            let (Some(h), Some(d), Some(t)) = (rd(rec, 0), rd(rec, 1), rd(rec, 2)) else {
                break;
            };
            if t != f.undefined() {
                break;
            }
            let (hb, pages) = self.read_chain_counted(h)?;
            let cost = pages
                .saturating_mul(f.page_header_size())
                .saturating_add(hb.len() as u64);
            budget = budget
                .checked_sub(cost)
                .ok_or_else(|| corrupt(f.base().saturating_add(h), "entry headers share pages"))?;
            let (ctime, mtime, attrs, name) =
                parse_entry_header(&hb).ok_or_else(|| corrupt(f.base() + h, "entry header"))?;
            let data_addr = (d != f.undefined()).then_some(d);
            let data_len = match data_addr {
                Some(a) => self.walk_data(a, i, &mut owners, &mut budget)?,
                None => 0,
            };
            out.push(EntryInfo {
                name,
                ctime,
                mtime,
                attrs,
                header_addr: h,
                data_addr,
                data_len,
            });
        }
        Ok(out)
    }

    fn walk_data(
        &self,
        addr: u64,
        entry: usize,
        owners: &mut HashMap<u64, usize>,
        budget: &mut u64,
    ) -> Result<u64> {
        let (f, ph) = (self.format, self.format.page_header_size());
        let max = self.size / ph;
        let mut cur = addr;
        let mut first = None;
        let mut left = 0u64;
        for _ in 0..max {
            let at = f.base().saturating_add(cur);
            if owners.get(&cur) == Some(&entry) {
                break;
            }
            let h = match self.page_header(cur) {
                Ok(h) => h,
                Err(Error::Io(e)) => return Err(Error::Io(e)),
                Err(_) => break,
            };
            if owners.insert(cur, entry).is_some() {
                return Err(corrupt(at, "entries share pages"));
            }
            let total = match first {
                Some(_) => left,
                None => {
                    first = Some(h.data_size);
                    h.data_size
                }
            };
            let take = h.page_size.min(total);
            let fits = total <= self.size
                && (at.saturating_add(ph))
                    .checked_add(take)
                    .is_some_and(|e| e <= self.size);
            let cost = ph.saturating_add(if fits { take } else { 0 });
            *budget = budget
                .checked_sub(cost)
                .ok_or_else(|| corrupt(at, "overlapping pages"))?;
            left = total - take;
            if !fits || left == 0 || h.next_page == f.undefined() {
                break;
            }
            cur = h.next_page;
        }
        Ok(first.unwrap_or(0))
    }
}

fn to_io(e: Error) -> io::Error {
    match e {
        Error::Io(e) => e,
        other => io::Error::new(io::ErrorKind::InvalidData, other),
    }
}

pub(crate) fn from_io(e: io::Error) -> Error {
    if e.get_ref().is_some_and(|i| i.is::<Error>()) {
        if let Some(b) = e.into_inner() {
            return match b.downcast::<Error>() {
                Ok(b) => *b,
                Err(b) => Error::Io(io::Error::new(io::ErrorKind::InvalidData, b)),
            };
        }
        return Error::NotContainer;
    }
    Error::Io(e)
}

pub struct ChainReader<'a, R: ReadAt> {
    c: &'a Container<R>,
    next: Option<u64>,
    pos: u64,
    page_left: u64,
    total_left: Option<u64>,
    at: u64,
    seen: HashSet<u64>,
}

impl<'a, R: ReadAt> ChainReader<'a, R> {
    fn new(c: &'a Container<R>, first: Option<u64>) -> Self {
        ChainReader {
            c,
            next: first,
            pos: 0,
            page_left: 0,
            total_left: if first.is_none() { Some(0) } else { None },
            at: 0,
            seen: HashSet::new(),
        }
    }

    fn advance(&mut self) -> Result<()> {
        let c = self.c;
        let addr = self.next.ok_or_else(|| corrupt(self.at, "short chain"))?;
        let abs = c.format.base().saturating_add(addr);
        if !self.seen.insert(addr) || self.seen.len() as u64 > c.size / c.format.page_header_size()
        {
            return Err(corrupt(abs, "page loop"));
        }
        let h = c.page_header(addr)?;
        self.at = abs;
        let total = match self.total_left {
            Some(t) => t,
            None => {
                if h.data_size > c.size {
                    return Err(corrupt(abs, "data size exceeds the file"));
                }
                h.data_size
            }
        };
        let take = h.page_size.min(total);
        let start = abs + c.format.page_header_size();
        if start.checked_add(take).is_none_or(|e| e > c.size) {
            return Err(corrupt(start, "page data beyond the end of the file"));
        }
        self.total_left = Some(total);
        self.pos = start;
        self.page_left = take;
        self.next = (h.next_page != c.format.undefined()).then_some(h.next_page);
        Ok(())
    }

    fn read_inner(&mut self, buf: &mut [u8]) -> Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.total_left.is_none() {
            self.advance()?;
        }
        loop {
            if self.total_left == Some(0) {
                return Ok(0);
            }
            if self.page_left == 0 {
                self.advance()?;
                continue;
            }
            let n = (buf.len() as u64).min(self.page_left);
            let dst = buf
                .get_mut(..n as usize)
                .ok_or_else(|| corrupt(self.pos, "buffer"))?;
            self.c.src.read_exact_at(dst, self.pos)?;
            self.pos += n;
            self.page_left -= n;
            self.total_left = self.total_left.map(|t| t - n);
            return Ok(n as usize);
        }
    }
}

impl<R: ReadAt> io::Read for ChainReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_inner(buf).map_err(to_io)
    }
}
