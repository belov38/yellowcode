use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use brace::binary::{Atom, BinNode, ErrorKind, Event, Reader, Writer};

use crate::container::{
    DtHeader, HEADER_LEN, deflate_writer, inflate_reader, read_header, write_header,
};
use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Folder(String),
    Users,
    UsersSpr,
    PasswordPolicies,
    CompromisedPasswords,
    DataBase,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RawEvent {
    CompatibilityMode(BinNode),
    CustomFlag { name: String, value: BinNode },
    TableStart { source: Source, ordinal: usize },
    Row(Vec<BinNode>),
    TableEnd,
    DataBaseStart(BinNode),
    DataBaseEnd,
    Unknown { name: String, items: Vec<BinNode> },
}

#[derive(Default)]
struct Trace {
    read: AtomicU64,
    error: OnceLock<String>,
}

struct Counted<R> {
    r: R,
    trace: Arc<Trace>,
}

impl<R: Read> Read for Counted<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.r.read(buf)?;
        self.trace.read.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

enum Src<R: Read> {
    Inflate(zraw::InflateReader<Counted<R>>),
    Plain(R),
}

struct Input<R: Read> {
    src: Src<R>,
    trace: Arc<Trace>,
}

impl<R: Read> Read for Input<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let r = match &mut self.src {
            Src::Inflate(z) => z.read(buf),
            Src::Plain(p) => p.read(buf),
        };
        if let Err(e) = &r
            && e.kind() != io::ErrorKind::Interrupted
        {
            let text = match self.src {
                Src::Inflate(_) => {
                    let read = self.trace.read.load(Ordering::Relaxed);
                    format!(
                        "deflate: {e} (read to file offset {})",
                        HEADER_LEN.saturating_add(read)
                    )
                }
                Src::Plain(_) => e.to_string(),
            };
            let _ = self.trace.error.set(text);
        }
        r
    }
}

enum State {
    Top,
    Rows { db: bool },
    Db,
    FolderData,
}

pub struct RawReader<R: Read> {
    header: DtHeader,
    rd: Reader<Input<R>>,
    trace: Arc<Trace>,
    state: State,
    ordinal: usize,
    failed: Option<Error>,
    limits: Option<brace::binary::ReadLimits>,
}

fn bin_err(e: brace::binary::Error, trace: &Trace) -> Error {
    match e.kind {
        ErrorKind::Io(k) => {
            let text = trace.error.get();
            if k == io::ErrorKind::InvalidData {
                Error::Dt {
                    offset: e.offset,
                    what: text.map_or_else(|| "deflate: stream: invalid data".into(), Clone::clone),
                }
            } else {
                Error::Io(match text {
                    Some(t) => io::Error::new(k, t.clone()),
                    None => io::Error::from(k),
                })
            }
        }
        ErrorKind::Limit(what) => Error::Limit {
            offset: e.offset,
            what,
        },
        k => Error::Dt {
            offset: e.offset,
            what: k.to_string(),
        },
    }
}

impl<R: Read> RawReader<R> {
    pub fn open(mut r: R) -> Result<RawReader<R>> {
        let header = read_header(&mut r)?;
        let trace = Arc::new(Trace::default());
        let file = Counted {
            r,
            trace: Arc::clone(&trace),
        };
        Ok(Self::with(
            header,
            Src::Inflate(inflate_reader(file)?),
            trace,
        ))
    }

    #[doc(hidden)]
    pub fn from_payload(r: R, h: DtHeader) -> RawReader<R> {
        Self::with(h, Src::Plain(r), Arc::new(Trace::default()))
    }

    fn with(header: DtHeader, src: Src<R>, trace: Arc<Trace>) -> Self {
        let input = Input {
            src,
            trace: Arc::clone(&trace),
        };
        RawReader {
            header,
            rd: Reader::new(input),
            trace,
            state: State::Top,
            ordinal: 0,
            failed: None,
            limits: None,
        }
    }

    pub fn set_read_limits(&mut self, limits: brace::binary::ReadLimits) {
        self.limits = Some(limits);
    }

    pub fn header(&self) -> DtHeader {
        self.header
    }

    pub fn offset(&self) -> u64 {
        self.rd.offset()
    }

    fn fail<T>(&self, what: &str) -> Result<T> {
        Err(Error::Dt {
            offset: self.rd.offset(),
            what: what.to_string(),
        })
    }

    fn at_close(&mut self) -> Result<bool> {
        Ok(matches!(
            self.rd.peek_event().map_err(|e| bin_err(e, &self.trace))?,
            Some(Event::Close)
        ))
    }

    fn expect_open(&mut self, what: &str) -> Result<()> {
        match self.rd.next_event().map_err(|e| bin_err(e, &self.trace))? {
            Some(Event::Open) => Ok(()),
            _ => self.fail(&format!("{what}: expected a list")),
        }
    }

    fn expect_close(&mut self, what: &str) -> Result<()> {
        match self.rd.next_event().map_err(|e| bin_err(e, &self.trace))? {
            Some(Event::Close) => Ok(()),
            _ => self.fail(&format!("{what}: unexpected extra value")),
        }
    }

    fn value(&mut self, what: &str) -> Result<BinNode> {
        match self.rd.read_value().map_err(|e| bin_err(e, &self.trace))? {
            Some(n) => Ok(n),
            None => self.fail(&format!("{what}: missing value")),
        }
    }

    fn string(&mut self, what: &str) -> Result<String> {
        let s = match self.value(what)? {
            BinNode::Atom(a) => a.as_str(),
            BinNode::List(_) => None,
        };
        match s {
            Some(s) => Ok(s),
            None => self.fail(&format!("{what}: expected a string")),
        }
    }

    fn open_rows(&mut self, what: &str) -> Result<()> {
        self.expect_open(what)?;
        match self.value(what)? {
            BinNode::Atom(a) if a.as_int() == Some(0) => Ok(()),
            _ => self.fail(&format!("{what}: row list must start with 0")),
        }
    }

    fn table_start(&mut self, source: Source, db: bool, what: &str) -> Result<RawEvent> {
        self.open_rows(what)?;
        self.state = State::Rows { db };
        let ordinal = if db {
            let o = self.ordinal;
            self.ordinal = o.saturating_add(1);
            o
        } else {
            0
        };
        Ok(RawEvent::TableStart { source, ordinal })
    }

    fn top(&mut self) -> Result<Option<RawEvent>> {
        match self.rd.next_event().map_err(|e| bin_err(e, &self.trace))? {
            None => return Ok(None),
            Some(Event::Open) => {}
            Some(_) => return self.fail("expected an element"),
        }
        let name = self.string("element name")?;
        let kind = name.to_ascii_lowercase();
        let ev = match kind.as_str() {
            "compatibilitymode" => {
                let v = self.value("CompatibilityMode")?;
                self.expect_close("CompatibilityMode")?;
                RawEvent::CompatibilityMode(v)
            }
            "customflag" => {
                let name = self.string("CustomFlag name")?;
                let value = self.value("CustomFlag")?;
                self.expect_close("CustomFlag")?;
                RawEvent::CustomFlag { name, value }
            }
            "folder" => {
                let f = self.string("Folder name")?;
                self.table_start(Source::Folder(f), false, "Folder")?
            }
            "users" => self.table_start(Source::Users, false, "Users")?,
            "usersspr" => self.table_start(Source::UsersSpr, false, "UsersSpr")?,
            "passwordpolicies" => {
                self.table_start(Source::PasswordPolicies, false, "PasswordPolicies")?
            }
            "compromisedpasswords" => {
                self.table_start(Source::CompromisedPasswords, false, "CompromisedPasswords")?
            }
            "database" => {
                self.expect_open("DataBase")?;
                let schema = self.value("DataBase schema")?;
                self.state = State::Db;
                self.ordinal = 0;
                RawEvent::DataBaseStart(schema)
            }
            _ => {
                let mut items = Vec::new();
                while !self.at_close()? {
                    items.push(self.value("element")?);
                }
                self.expect_close("element")?;
                RawEvent::Unknown { name, items }
            }
        };
        Ok(Some(ev))
    }

    fn rows(&mut self, db: bool) -> Result<RawEvent> {
        if self.at_close()? {
            self.expect_close("row list")?;
            if db {
                self.state = State::Db;
            } else {
                self.expect_close("element")?;
                self.state = State::Top;
            }
            return Ok(RawEvent::TableEnd);
        }
        match self.value("row")? {
            BinNode::List(row) => Ok(RawEvent::Row(row)),
            BinNode::Atom(_) => self.fail("row must be a list"),
        }
    }

    fn db(&mut self) -> Result<RawEvent> {
        if self.at_close()? {
            self.expect_close("DataBase")?;
            self.expect_close("DataBase")?;
            self.state = State::Top;
            return Ok(RawEvent::DataBaseEnd);
        }
        self.table_start(Source::DataBase, true, "DataBase table")
    }

    pub fn folder_header(&mut self) -> Result<Option<Vec<BinNode>>> {
        if let Some(e) = &self.failed {
            return Err(e.again());
        }
        if let Some(limits) = self.limits {
            self.rd.begin_budget(limits);
        }
        let result = self.folder_header_inner();
        if let Err(e) = &result {
            self.failed = Some(e.again());
        }
        result
    }

    fn folder_header_inner(&mut self) -> Result<Option<Vec<BinNode>>> {
        if !matches!(self.state, State::Rows { db: false }) {
            return self.fail("streamed folder header out of order");
        }
        if self.at_close()? {
            self.rows(false)?;
            return Ok(None);
        }
        self.expect_open("folder row")?;
        let mut fields = Vec::with_capacity(10);
        for _ in 0..5 {
            let flag = self.value("folder NULL flag")?;
            if !matches!(&flag, BinNode::Atom(a) if a.as_int() == Some(1)) {
                return self.fail("folder field must have non-null flag 1");
            }
            fields.push(flag);
            fields.push(self.value("folder field")?);
        }
        if !matches!(self.value("folder binary flag")?, BinNode::Atom(a) if a.as_int() == Some(1)) {
            return self.fail("folder binary field must have non-null flag 1");
        }
        self.state = State::FolderData;
        Ok(Some(fields))
    }

    pub fn copy_folder_binary(&mut self, sink: &mut impl Write, limit: u64) -> Result<u64> {
        self.copy_folder_binary_with_chunk_limit(sink, limit, u64::MAX)
    }

    pub fn copy_folder_binary_with_chunk_limit(
        &mut self,
        sink: &mut impl Write,
        limit: u64,
        max_chunks: u64,
    ) -> Result<u64> {
        self.copy_folder_binary_bounded(sink, limit, max_chunks, false)
    }

    pub fn copy_folder_binary_with_work_limit(
        &mut self,
        sink: &mut impl Write,
        limit: u64,
        base_chunks: u64,
    ) -> Result<u64> {
        self.copy_folder_binary_bounded(sink, limit, base_chunks, true)
    }

    fn copy_folder_binary_bounded(
        &mut self,
        sink: &mut impl Write,
        limit: u64,
        chunks: u64,
        adaptive: bool,
    ) -> Result<u64> {
        if let Some(e) = &self.failed {
            return Err(e.again());
        }
        let result = (|| {
            if !matches!(self.state, State::FolderData) {
                return self.fail("folder binary field out of order");
            }
            let len = if adaptive {
                self.rd.copy_binary_with_work_limit(sink, limit, chunks)
            } else {
                self.rd.copy_binary_with_chunk_limit(sink, limit, chunks)
            }
            .map_err(|e| bin_err(e, &self.trace))?;
            self.expect_close("folder row")?;
            self.state = State::Rows { db: false };
            Ok(len)
        })();
        if let Err(e) = &result {
            self.failed = Some(e.again());
        }
        result
    }

    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<Option<RawEvent>> {
        if let Some(e) = &self.failed {
            return Err(e.again());
        }
        if let Some(limits) = self.limits {
            self.rd.begin_budget(limits);
        }
        let r = match self.state {
            State::Top => self.top(),
            State::Rows { db } => self.rows(db).map(Some),
            State::Db => self.db().map(Some),
            State::FolderData => self.fail("folder binary field is pending"),
        };
        if let Err(e) = &r {
            self.failed = Some(e.again());
        }
        r
    }
}

pub(crate) enum Sink<W: Write> {
    Plain(W),
    Deflate(zraw::DeflateWriter<W>),
}

impl<W: Write> Write for Sink<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Sink::Plain(w) => w.write(buf),
            Sink::Deflate(w) => w.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Sink::Plain(w) => w.flush(),
            Sink::Deflate(w) => w.flush(),
        }
    }
}

pub struct RawWriter<W: Write> {
    w: Writer<Sink<W>>,
    state: State,
    ordinal: usize,
}

fn order(what: &str) -> Error {
    Error::Unsupported(format!("element order: {what}"))
}

impl<W: Write> RawWriter<W> {
    pub fn new(mut w: W, h: DtHeader) -> Result<RawWriter<W>> {
        write_header(&mut w, h)?;
        Ok(Self::with(Sink::Deflate(deflate_writer(w)?)))
    }

    #[doc(hidden)]
    pub fn payload(w: W) -> RawWriter<W> {
        Self::with(Sink::Plain(w))
    }

    fn with(s: Sink<W>) -> Self {
        RawWriter {
            w: Writer::new(s),
            state: State::Top,
            ordinal: 0,
        }
    }

    fn head(&mut self, kind: &str) -> Result<()> {
        self.w.open()?;
        self.w.str(kind)?;
        Ok(())
    }

    fn list_start(&mut self) -> Result<()> {
        self.w.open()?;
        self.w.atom(&Atom::int(0))?;
        Ok(())
    }

    pub fn event(&mut self, e: &RawEvent) -> Result<()> {
        match (&self.state, e) {
            (State::Top, RawEvent::CompatibilityMode(v)) => {
                self.head("CompatibilityMode")?;
                self.w.node(v)?;
                self.w.close()?;
            }
            (State::Top, RawEvent::CustomFlag { name, value }) => {
                self.head("CustomFlag")?;
                self.w.str(name)?;
                self.w.node(value)?;
                self.w.close()?;
            }
            (State::Top, RawEvent::TableStart { source, ordinal }) => {
                if *ordinal != 0 {
                    return Err(order("system table ordinal must be 0"));
                }
                match source {
                    Source::Folder(n) => {
                        self.head("Folder")?;
                        self.w.str(n)?;
                    }
                    Source::Users => self.head("Users")?,
                    Source::UsersSpr => self.head("UsersSpr")?,
                    Source::PasswordPolicies => self.head("PasswordPolicies")?,
                    Source::CompromisedPasswords => self.head("CompromisedPasswords")?,
                    Source::DataBase => return Err(order("DataBase table outside DataBase")),
                }
                self.list_start()?;
                self.state = State::Rows { db: false };
            }
            (State::Top, RawEvent::DataBaseStart(schema)) => {
                self.head("DataBase")?;
                self.w.open()?;
                self.w.node(schema)?;
                self.state = State::Db;
                self.ordinal = 0;
            }
            (State::Top, RawEvent::Unknown { name, items }) => {
                self.head(name)?;
                for n in items {
                    self.w.node(n)?;
                }
                self.w.close()?;
            }
            (State::Db, RawEvent::TableStart { source, ordinal }) => {
                if *source != Source::DataBase || *ordinal != self.ordinal {
                    return Err(order("expected the next DataBase table"));
                }
                self.list_start()?;
                self.ordinal = self.ordinal.saturating_add(1);
                self.state = State::Rows { db: true };
            }
            (State::Db, RawEvent::DataBaseEnd) => {
                self.w.close()?;
                self.w.close()?;
                self.state = State::Top;
            }
            (State::Rows { .. }, RawEvent::Row(row)) => {
                self.w.open()?;
                for n in row {
                    self.w.node(n)?;
                }
                self.w.close()?;
            }
            (State::Rows { db }, RawEvent::TableEnd) => {
                let db = *db;
                self.w.close()?;
                if db {
                    self.state = State::Db;
                } else {
                    self.w.close()?;
                    self.state = State::Top;
                }
            }
            _ => return Err(order("event not allowed here")),
        }
        Ok(())
    }

    pub(crate) fn row_with(
        &mut self,
        f: impl FnOnce(&mut Writer<Sink<W>>) -> Result<()>,
    ) -> Result<()> {
        if !matches!(self.state, State::Rows { .. }) {
            return Err(order("row outside a table"));
        }
        f(&mut self.w)
    }

    pub fn finish(self) -> Result<W> {
        if !matches!(self.state, State::Top) {
            return Err(order("finish inside an element"));
        }
        match self.w.finish()? {
            Sink::Plain(w) => Ok(w),
            Sink::Deflate(d) => Ok(d.finish()?),
        }
    }
}
