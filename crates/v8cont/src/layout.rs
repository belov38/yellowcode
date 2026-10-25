use crate::error::{Error, Result};
use crate::format::{FileHeader, Format, PageHeader, entry_header_bytes, parse_entry_header};
use crate::read::{Container, EntryInfo, PageRec};
use crate::source::ReadAt;
use crate::write::{DataSource, EntrySpec, FORMAT16_PLACEHOLDER};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{self, BufWriter, Write};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainLayout {
    pub header: Vec<PageRec>,
    pub data: Option<Vec<PageRec>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Patch {
    pub offset: u64,
    pub len: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Layout {
    pub format: Format,
    pub header: FileHeader,
    pub file_len: u64,
    pub toc: Vec<PageRec>,
    pub entries: Vec<ChainLayout>,
    pub patches: Vec<Patch>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Captured {
    pub layout: Layout,
    pub residue: Vec<u8>,
    pub placeholder: Option<Vec<u8>>,
}

const CHUNK: usize = 1 << 20;
const MERGE_GAP: u64 = 8;

#[derive(Debug, Clone, Copy)]
enum Kind {
    Toc,
    Header(usize),
    Data(usize),
}

impl Kind {
    fn describe(self) -> String {
        match self {
            Kind::Toc => "table of contents".into(),
            Kind::Header(i) => format!("entry {i} header"),
            Kind::Data(i) => format!("entry {i} data"),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct PlanPage {
    abs: u64,
    end: u64,
    hdr: PageHeader,
    kind: Kind,
    head: bool,
    off: u64,
    total: u64,
}

impl PlanPage {
    fn payload(&self) -> u64 {
        self.total.saturating_sub(self.off).min(self.hdr.page_size)
    }
}

struct Plan {
    pages: Vec<PlanPage>,
    data_pages: Vec<usize>,
}

type Defect = (u64, String);

fn add_chain(
    l: &Layout,
    kind: Kind,
    chain: &[PageRec],
    pages: &mut Vec<PlanPage>,
) -> std::result::Result<usize, Defect> {
    let f = l.format;
    let (base, ph, und) = (f.base(), f.page_header_size(), f.undefined());
    let first = base + f.file_header_size();
    let total = chain
        .first()
        .ok_or_else(|| (first, format!("{}: empty chain", kind.describe())))?
        .2;
    let (mut off, mut carrying) = (0u64, 0usize);
    for (k, p) in chain.iter().enumerate() {
        let PageRec(addr, page_size, size_field) = *p;
        let at = base.saturating_add(addr);
        let f15_max = u64::from(u32::MAX);
        if addr == und
            || (f == Format::F15 && [addr, page_size, size_field].iter().any(|&v| v > f15_max))
        {
            return Err((at, format!("{}: page field out of range", kind.describe())));
        }
        let abs = base
            .checked_add(addr)
            .filter(|&a| a.checked_add(ph).is_some_and(|e| e <= l.file_len))
            .ok_or_else(|| (at, format!("{}: page outside the file", kind.describe())))?;
        if abs < first {
            return Err((
                abs,
                format!("{}: page overlaps the file header", kind.describe()),
            ));
        }
        let end = (abs + ph)
            .checked_add(page_size)
            .ok_or_else(|| (abs, format!("{}: page size overflows", kind.describe())))?;
        let next = chain.get(k + 1).map_or(und, |n| n.0);
        let page = PlanPage {
            abs,
            end,
            hdr: PageHeader {
                data_size: size_field,
                page_size,
                next_page: next,
            },
            kind,
            head: k == 0,
            off,
            total,
        };
        if page.payload() > 0 {
            carrying += 1;
        }
        pages.push(page);
        off = off.saturating_add(page_size);
    }
    if off < total {
        let at = chain.first().map_or(first, |p| base.saturating_add(p.0));
        return Err((
            at,
            format!(
                "{}: chain is shorter than its size {total}",
                kind.describe()
            ),
        ));
    }
    Ok(carrying)
}

impl Plan {
    fn new(l: &Layout) -> std::result::Result<Plan, Defect> {
        let f = l.format;
        let first = f.base() + f.file_header_size();
        if l.file_len < first {
            return Err((
                0,
                format!("file length {} is shorter than the file header", l.file_len),
            ));
        }
        if f == Format::F15 && l.header.next_free_page > u64::from(u32::MAX) {
            return Err((f.base(), "Format15 next_free_page exceeds 32 bits".into()));
        }
        match l.toc.first() {
            None if !l.entries.is_empty() => {
                return Err((first, "entries without a table of contents".into()));
            }
            Some(p) if p.0 != f.file_header_size() => {
                return Err((
                    first,
                    "the table of contents is not after the file header".into(),
                ));
            }
            _ => {}
        }
        let mut pages = Vec::new();
        let mut data_pages = vec![0; l.entries.len()];
        if let Some(p) = l.toc.first() {
            let need = u64::try_from(l.entries.len())
                .ok()
                .and_then(|n| n.checked_mul(f.toc_record_size()));
            if need.is_none_or(|n| n > p.2) {
                return Err((
                    first,
                    format!(
                        "the table of contents is too small for {} entries",
                        l.entries.len()
                    ),
                ));
            }
            add_chain(l, Kind::Toc, &l.toc, &mut pages)?;
        }
        for (i, e) in l.entries.iter().enumerate() {
            add_chain(l, Kind::Header(i), &e.header, &mut pages)?;
            if let (Some(d), Some(n)) = (&e.data, data_pages.get_mut(i)) {
                *n = add_chain(l, Kind::Data(i), d, &mut pages)?;
            }
        }
        pages.sort_by_key(|p| p.abs);
        for w in pages.windows(2) {
            if let [a, b] = w
                && a.end > b.abs
            {
                return Err((
                    b.abs,
                    format!(
                        "overlapping pages ({}, {})",
                        a.kind.describe(),
                        b.kind.describe()
                    ),
                ));
            }
        }
        Ok(Plan { pages, data_pages })
    }
}

fn mismatch(what: String) -> Error {
    Error::LayoutMismatch(what)
}

fn header_fits(s: &EntrySpec, total: u64) -> bool {
    let h = entry_header_bytes(s.ctime, s.mtime, s.attrs, &s.name);
    h.len() as u64 == total
        && parse_entry_header(&h).is_some_and(|(c, m, a, name)| {
            (c, m, a) == (s.ctime, s.mtime, s.attrs) && name == s.name
        })
}

impl Layout {
    pub fn check(&self, specs: &[EntrySpec]) -> Result<()> {
        self.plan_for(specs).map(|_| ())
    }

    fn plan_for(&self, specs: &[EntrySpec]) -> Result<Plan> {
        if specs.len() != self.entries.len() {
            return Err(mismatch(format!(
                "{} entries, the layout has {}",
                specs.len(),
                self.entries.len()
            )));
        }
        let plan =
            Plan::new(self).map_err(|(at, what)| mismatch(format!("at 0x{at:x}: {what}")))?;
        for (i, (s, e)) in specs.iter().zip(&self.entries).enumerate() {
            let stored = e.data.as_ref().and_then(|d| d.first()).map(|p| p.2);
            if s.data_len != stored {
                return Err(mismatch(format!(
                    "entry {i} ({}): data length {:?}, the layout has {:?}",
                    s.name, s.data_len, stored
                )));
            }
            let total = e.header.first().map_or(0, |p| p.2);
            if !header_fits(s, total) {
                return Err(mismatch(format!(
                    "entry {i} ({}): the header does not fit its chain of {total} bytes",
                    s.name
                )));
            }
        }
        let mut pos = 0u64;
        for p in &self.patches {
            let end = p.offset.checked_add(p.len);
            if p.len == 0 || p.offset < pos || end.is_none_or(|e| e > self.file_len) {
                return Err(mismatch(format!(
                    "patch at 0x{:x} of {} bytes is empty, unordered or beyond the file",
                    p.offset, p.len
                )));
            }
            pos = end.unwrap_or(pos);
        }
        let ph = self.format.page_header_size();
        let mut payload = plan
            .pages
            .iter()
            .filter(|p| matches!(p.kind, Kind::Data(_)) && p.payload() > 0)
            .map(|p| {
                let start = p.abs.saturating_add(ph);
                (start, start.saturating_add(p.payload()))
            })
            .peekable();
        for p in &self.patches {
            let end = p.offset.saturating_add(p.len);
            while payload.next_if(|&(_, e)| e <= p.offset).is_some() {}
            if let Some(&(s, _)) = payload.peek()
                && s < end
            {
                return Err(mismatch(format!(
                    "patch at 0x{:x} of {} bytes covers data payload at 0x{s:x}",
                    p.offset, p.len
                )));
            }
        }
        Ok(plan)
    }
}

struct Content {
    toc: Vec<u8>,
    headers: Vec<Vec<u8>>,
}

impl Content {
    fn new(l: &Layout, specs: &[EntrySpec]) -> Content {
        let f = l.format;
        let a = usize::try_from(f.addr_size()).unwrap_or(8);
        let mut toc = Vec::with_capacity(l.entries.len().saturating_mul(a * 3));
        for e in &l.entries {
            let h = e.header.first().map_or(f.undefined(), |p| p.0);
            let d = e
                .data
                .as_ref()
                .and_then(|d| d.first())
                .map_or(f.undefined(), |p| p.0);
            for v in [h, d, f.undefined()] {
                toc.extend_from_slice(v.to_le_bytes().get(..a).unwrap_or_default());
            }
        }
        let headers = specs
            .iter()
            .map(|s| entry_header_bytes(s.ctime, s.mtime, s.attrs, &s.name))
            .collect();
        Content { toc, headers }
    }
}

trait Target {
    fn bytes(&mut self, b: &[u8]) -> Result<()>;
    fn zeros(&mut self, n: u64) -> Result<()>;
    fn data(&mut self, page: &PlanPage, keep: u64) -> Result<()>;
}

struct Out<'t> {
    t: &'t mut dyn Target,
    pos: u64,
    end: u64,
}

impl Out<'_> {
    fn room(&self) -> u64 {
        self.end.saturating_sub(self.pos)
    }
    fn bytes(&mut self, b: &[u8]) -> Result<()> {
        let k = usize::try_from(self.room()).map_or(b.len(), |r| r.min(b.len()));
        if k > 0 {
            self.t.bytes(b.get(..k).unwrap_or_default())?;
            self.pos = self.pos.saturating_add(k as u64);
        }
        Ok(())
    }
    fn zeros(&mut self, n: u64) -> Result<()> {
        let k = n.min(self.room());
        if k > 0 {
            self.t.zeros(k)?;
            self.pos = self.pos.saturating_add(k);
        }
        Ok(())
    }
    fn content(&mut self, real: &[u8], off: u64, n: u64) -> Result<()> {
        let len = real.len() as u64;
        let (a, b) = (off.min(len), off.saturating_add(n).min(len));
        let s = usize::try_from(a)
            .ok()
            .zip(usize::try_from(b).ok())
            .and_then(|(a, b)| real.get(a..b))
            .unwrap_or_default();
        self.bytes(s)?;
        self.zeros(n.saturating_sub(s.len() as u64))
    }
}

fn render(
    l: &Layout,
    plan: &Plan,
    placeholder: &[u8],
    c: &Content,
    t: &mut dyn Target,
) -> Result<()> {
    let f = l.format;
    let mut o = Out {
        t,
        pos: 0,
        end: l.file_len,
    };
    o.bytes(placeholder)?;
    o.bytes(&l.header.encode(f))?;
    for p in &plan.pages {
        let gap = p
            .abs
            .checked_sub(o.pos)
            .ok_or_else(|| mismatch(format!("page at 0x{:x} overlaps preceding bytes", p.abs)))?;
        o.zeros(gap)?;
        let hb = p.hdr.encode(f).ok_or_else(|| {
            mismatch(format!(
                "page at 0x{:x}: header does not fit the format",
                p.abs
            ))
        })?;
        o.bytes(&hb)?;
        let n = p.payload();
        match p.kind {
            Kind::Toc => o.content(&c.toc, p.off, n)?,
            Kind::Header(i) => {
                o.content(c.headers.get(i).map_or(&[][..], Vec::as_slice), p.off, n)?
            }
            Kind::Data(_) if n > 0 || (p.head && p.total == 0) => {
                let keep = n.min(o.room());
                o.t.data(p, keep)?;
                o.pos = o.pos.saturating_add(keep);
            }
            Kind::Data(_) => {}
        }
        o.zeros(p.hdr.page_size.saturating_sub(n))?;
    }
    let tail = o.room();
    o.zeros(tail)
}

struct Compare<'a, R: ReadAt + ?Sized> {
    src: &'a R,
    exp: Vec<u8>,
    start: u64,
    act: Vec<u8>,
    open: Option<(u64, u64)>,
    patches: Vec<Patch>,
}

impl<'a, R: ReadAt + ?Sized> Compare<'a, R> {
    fn new(src: &'a R) -> Self {
        Compare {
            src,
            exp: Vec::with_capacity(CHUNK),
            start: 0,
            act: Vec::new(),
            open: None,
            patches: Vec::new(),
        }
    }

    fn close(&mut self) {
        if let Some((s, e)) = self.open.take() {
            self.patches.push(Patch {
                offset: s,
                len: e.saturating_sub(s),
            });
        }
    }

    fn differs(&mut self, at: u64) {
        match &mut self.open {
            Some((_, e)) if at.saturating_sub(*e) < MERGE_GAP => *e = at.saturating_add(1),
            _ => {
                self.close();
                self.open = Some((at, at.saturating_add(1)));
            }
        }
    }

    fn flush(&mut self) -> Result<()> {
        if self.exp.is_empty() {
            return Ok(());
        }
        let (exp, mut act) = (std::mem::take(&mut self.exp), std::mem::take(&mut self.act));
        act.resize(exp.len(), 0);
        self.src.read_exact_at(&mut act, self.start)?;
        for (k, (e, a)) in exp.chunks(64).zip(act.chunks(64)).enumerate() {
            if e == a {
                continue;
            }
            for (j, (x, y)) in e.iter().zip(a).enumerate() {
                if x != y {
                    self.differs(self.start.saturating_add((k * 64 + j) as u64));
                }
            }
        }
        self.start = self.start.saturating_add(exp.len() as u64);
        (self.exp, self.act) = (exp, act);
        self.exp.clear();
        Ok(())
    }

    fn finish(mut self) -> Result<Vec<Patch>> {
        self.flush()?;
        self.close();
        Ok(self.patches)
    }
}

impl<R: ReadAt + ?Sized> Target for Compare<'_, R> {
    fn bytes(&mut self, mut b: &[u8]) -> Result<()> {
        while !b.is_empty() {
            let k = CHUNK.saturating_sub(self.exp.len()).min(b.len());
            let (head, rest) = b.split_at_checked(k).unwrap_or((b, &[]));
            self.exp.extend_from_slice(head);
            b = rest;
            if self.exp.len() >= CHUNK {
                self.flush()?;
            }
        }
        Ok(())
    }
    fn zeros(&mut self, mut n: u64) -> Result<()> {
        while n > 0 {
            let room = CHUNK.saturating_sub(self.exp.len());
            let k = usize::try_from(n).map_or(room, |n| n.min(room));
            self.exp.resize(self.exp.len().saturating_add(k), 0);
            n = n.saturating_sub(k as u64);
            if self.exp.len() >= CHUNK {
                self.flush()?;
            }
        }
        Ok(())
    }
    fn data(&mut self, _: &PlanPage, keep: u64) -> Result<()> {
        if keep == 0 {
            return Ok(());
        }
        self.flush()?;
        self.close();
        self.start = self.start.saturating_add(keep);
        Ok(())
    }
}

fn read_residue<R: ReadAt + ?Sized>(src: &R, patches: &[Patch]) -> Result<Vec<u8>> {
    let mut v = Vec::new();
    for p in patches {
        let mut at = p.offset;
        let end = p.offset.saturating_add(p.len);
        while at < end {
            let k = usize::try_from(end - at).map_or(CHUNK, |n| n.min(CHUNK));
            let old = v.len();
            v.resize(old.saturating_add(k), 0);
            src.read_exact_at(v.get_mut(old..).unwrap_or_default(), at)?;
            at = at.saturating_add(k as u64);
        }
    }
    Ok(v)
}

pub(crate) fn spec_of(e: &EntryInfo) -> EntrySpec {
    EntrySpec {
        name: e.name.clone(),
        ctime: e.ctime,
        mtime: e.mtime,
        attrs: e.attrs,
        data_len: e.data_addr.map(|_| e.data_len),
    }
}

impl<R: ReadAt> Container<R> {
    pub fn capture_layout(&self) -> Result<Captured> {
        let f = self.format();
        let mut owners = HashMap::new();
        let toc = if self.has_toc() {
            self.chain_pages(f.file_header_size(), 0, &mut owners)?
        } else {
            Vec::new()
        };
        let mut entries = Vec::with_capacity(self.entries().len());
        for (i, e) in self.entries().iter().enumerate() {
            let h = i.saturating_mul(2).saturating_add(1);
            entries.push(ChainLayout {
                header: self.chain_pages(e.header_addr, h, &mut owners)?,
                data: e
                    .data_addr
                    .map(|a| self.chain_pages(a, h.saturating_add(1), &mut owners))
                    .transpose()?,
            });
        }
        drop(owners);
        let mut layout = Layout {
            format: f,
            header: *self.header(),
            file_len: self.file_len(),
            toc,
            entries,
            patches: Vec::new(),
        };
        let plan = Plan::new(&layout).map_err(|(offset, what)| Error::Corrupt { offset, what })?;
        let placeholder = self.placeholder()?;
        let specs: Vec<EntrySpec> = self.entries().iter().map(spec_of).collect();
        let content = Content::new(&layout, &specs);
        let mut cmp = Compare::new(self.source());
        render(&layout, &plan, &placeholder, &content, &mut cmp)?;
        layout.patches = cmp.finish()?;
        let residue = read_residue(self.source(), &layout.patches)?;
        let custom = f == Format::F16 && placeholder != FORMAT16_PLACEHOLDER;
        Ok(Captured {
            layout,
            residue,
            placeholder: custom.then_some(placeholder),
        })
    }
}

struct Sink<'a> {
    out: BufWriter<&'a mut dyn Write>,
    pos: u64,
    patches: &'a [Patch],
    pi: usize,
    res_at: u64,
    residue: &'a [u8],
}

fn bad_residue() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "residue does not cover the patches",
    )
}

impl Sink<'_> {
    fn put(&mut self, mut b: &[u8]) -> io::Result<()> {
        while !b.is_empty() {
            while let Some(p) = self.patches.get(self.pi) {
                if p.offset.saturating_add(p.len) > self.pos {
                    break;
                }
                self.res_at = self.res_at.saturating_add(p.len);
                self.pi += 1;
            }
            let n = b.len() as u64;
            let k = match self.patches.get(self.pi) {
                Some(p) if p.offset <= self.pos => {
                    let k = n.min(p.offset.saturating_add(p.len) - self.pos);
                    let from = self.res_at.saturating_add(self.pos - p.offset);
                    let s = usize::try_from(from)
                        .ok()
                        .zip(usize::try_from(from.saturating_add(k)).ok())
                        .and_then(|(a, z)| self.residue.get(a..z))
                        .ok_or_else(bad_residue)?;
                    self.out.write_all(s)?;
                    k
                }
                Some(p) => {
                    let k = n.min(p.offset - self.pos);
                    self.out
                        .write_all(b.get(..usize::try_from(k).unwrap_or(b.len())).unwrap_or(b))?;
                    k
                }
                None => {
                    self.out.write_all(b)?;
                    n
                }
            };
            b = b
                .get(usize::try_from(k).unwrap_or(b.len())..)
                .unwrap_or_default();
            self.pos = self.pos.saturating_add(k);
        }
        Ok(())
    }
}

struct Stream<'s, 'a> {
    sink: &'s mut Sink<'a>,
    keep: u64,
    left: u64,
    over: bool,
}

impl Write for Stream<'_, '_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = buf.len() as u64;
        if n > self.left {
            self.over = true;
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "data source wrote more than declared",
            ));
        }
        let k = n.min(self.keep);
        self.sink.put(
            buf.get(..usize::try_from(k).unwrap_or(buf.len()))
                .unwrap_or(buf),
        )?;
        self.keep -= k;
        self.left -= n;
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct Collect {
    v: Vec<u8>,
    cap: u64,
    over: bool,
}

impl Write for Collect {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if (self.v.len() as u64).saturating_add(buf.len() as u64) > self.cap {
            self.over = true;
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "data source wrote more than declared",
            ));
        }
        self.v.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct Writer<'a, 'd> {
    sink: Sink<'a>,
    data: &'d mut dyn DataSource,
    specs: &'a [EntrySpec],
    data_pages: &'a [usize],
    bufs: HashMap<usize, (Vec<u8>, usize)>,
}

impl Writer<'_, '_> {
    fn wrong_len(&self, i: usize, wrote: u64, more: bool, total: u64) -> Error {
        let name = self.specs.get(i).map_or("", |s| s.name.as_str());
        let wrote = if more {
            "more".to_string()
        } else {
            wrote.to_string()
        };
        mismatch(format!(
            "entry {i} ({name}): data source wrote {wrote} bytes, declared {total}"
        ))
    }
}

impl Target for Writer<'_, '_> {
    fn bytes(&mut self, b: &[u8]) -> Result<()> {
        Ok(self.sink.put(b)?)
    }
    fn zeros(&mut self, mut n: u64) -> Result<()> {
        static Z: [u8; 1 << 16] = [0; 1 << 16];
        while n > 0 {
            let k = usize::try_from(n).map_or(Z.len(), |n| n.min(Z.len()));
            self.sink.put(Z.get(..k).unwrap_or(&Z))?;
            n = n.saturating_sub(k as u64);
        }
        Ok(())
    }
    fn data(&mut self, page: &PlanPage, keep: u64) -> Result<()> {
        let Kind::Data(i) = page.kind else {
            return Err(mismatch("data payload outside a data chain".into()));
        };
        let total = page.total;
        if self.data_pages.get(i).is_none_or(|&n| n <= 1) {
            let mut w = Stream {
                sink: &mut self.sink,
                keep,
                left: total,
                over: false,
            };
            let r = self.data.write_data(i, &mut w);
            let (over, left) = (w.over, w.left);
            if over || (r.is_ok() && left != 0) {
                return Err(self.wrong_len(i, total - left, over, total));
            }
            return r;
        }
        if !self.bufs.contains_key(&i) {
            let mut w = Collect {
                v: Vec::new(),
                cap: total,
                over: false,
            };
            let r = self.data.write_data(i, &mut w);
            if w.over || (r.is_ok() && w.v.len() as u64 != total) {
                return Err(self.wrong_len(i, w.v.len() as u64, w.over, total));
            }
            r?;
            let pages = self.data_pages.get(i).copied().unwrap_or(0);
            self.bufs.insert(i, (w.v, pages));
        }
        let (v, left) = self
            .bufs
            .get_mut(&i)
            .ok_or_else(|| mismatch(format!("entry {i}: no data")))?;
        let s = usize::try_from(page.off)
            .ok()
            .zip(usize::try_from(page.off.saturating_add(keep)).ok())
            .and_then(|(a, z)| v.get(a..z))
            .ok_or_else(|| mismatch(format!("entry {i}: data page beyond the data")))?;
        self.sink.put(s)?;
        *left = left.saturating_sub(1);
        if *left == 0 {
            self.bufs.remove(&i);
        }
        Ok(())
    }
}

pub fn write_exact(
    out: &mut dyn Write,
    layout: &Layout,
    residue: &[u8],
    placeholder: Option<&[u8]>,
    specs: &[EntrySpec],
    data: &mut dyn DataSource,
) -> Result<()> {
    let plan = layout.plan_for(specs)?;
    let f = layout.format;
    let want = layout
        .patches
        .iter()
        .try_fold(0u64, |a, p| a.checked_add(p.len))
        .ok_or_else(|| mismatch("patch lengths overflow".into()))?;
    if residue.len() as u64 != want {
        return Err(mismatch(format!(
            "residue has {} bytes, the patches need {want}",
            residue.len()
        )));
    }
    let ph: &[u8] = match placeholder {
        None if f == Format::F16 => FORMAT16_PLACEHOLDER,
        None => &[],
        Some(p) if p.len() as u64 == f.base() => p,
        Some(p) => {
            return Err(mismatch(format!(
                "placeholder of {} bytes, {f:?} needs {}",
                p.len(),
                f.base()
            )));
        }
    };
    let content = Content::new(layout, specs);
    let mut w = Writer {
        sink: Sink {
            out: BufWriter::with_capacity(CHUNK, out),
            pos: 0,
            patches: &layout.patches,
            pi: 0,
            res_at: 0,
            residue,
        },
        data,
        specs,
        data_pages: &plan.data_pages,
        bufs: HashMap::new(),
    };
    render(layout, &plan, ph, &content, &mut w)?;
    if w.sink.pos != layout.file_len {
        return Err(mismatch(format!(
            "rendered {} bytes, the layout has {}",
            w.sink.pos, layout.file_len
        )));
    }
    w.sink.out.flush()?;
    Ok(())
}
