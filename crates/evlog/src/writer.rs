use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::dict::Dicts;
use crate::reader::{FileKind, header_prefix, in_file, parse_file, scan_records};
use crate::record::{event_node, header, parse_header};
use crate::{BslDate, Error, Event, TxStatus, TxnToken, Uuid16};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Period {
    Day,
    Hour,
}

pub struct WriterOptions {
    pub period: Period,
    pub guid: Option<Uuid16>,
}

const SEP: &[u8] = b",\r\n";

pub fn lgp_name(t: BslDate, p: Period) -> String {
    let d = t.parts();
    let h = if p == Period::Hour { d.hour } else { 0 };
    format!("{:04}{:02}{:02}{:02}0000.lgp", d.year, d.month, d.day, h)
}

struct Current {
    name: String,
    file: File,
    len: u64,
}

struct OpenTxn {
    path: PathBuf,
    offset: u64,
    ticks: i64,
}

pub struct Writer {
    dir: PathBuf,
    guid: Uuid16,
    period: Period,
    dicts: Dicts,
    lgf: File,
    lgf_has_records: bool,
    cur: Option<Current>,
    txns: HashMap<TxnToken, OpenTxn>,
    stale: bool,
    checked: HashSet<String>,
}

fn random_guid() -> Uuid16 {
    let mut b: [u8; 16] = rand::random();
    if let Some(x) = b.get_mut(6) {
        *x = (*x & 0x0f) | 0x40;
    }
    if let Some(x) = b.get_mut(8) {
        *x = (*x & 0x3f) | 0x80;
    }
    Uuid16::from_bytes(b)
}

fn bad(s: &str) -> Error {
    Error::Malformed(s.to_owned())
}

fn load_lgf(path: &Path) -> Result<Option<(Uuid16, Dicts, bool)>, Error> {
    const LGF: &str = "1Cv8.lgf";
    let bytes = match std::fs::read(path) {
        Ok(b) if !b.is_empty() => b,
        Ok(_) => return Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let p = parse_file(&bytes, FileKind::Lgf).map_err(in_file(LGF))?;
    if let Some(&(off, len)) = p.damage.first() {
        return Err(Error::Malformed(format!(
            "{LGF}: damaged records at {off} ({len} bytes)"
        )));
    }
    let nodes: Vec<brace::Node> = p.records.into_iter().map(|(_, n)| n).collect();
    let dicts = Dicts::from_lgf_records(&nodes).map_err(in_file(LGF))?;
    if let Some(tail) = p.tail {
        OpenOptions::new().write(true).open(path)?.set_len(tail)?;
    }
    Ok(Some((p.guid, dicts, !nodes.is_empty())))
}

impl Writer {
    pub fn open(dir: &Path, o: WriterOptions) -> Result<Writer, Error> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join("1Cv8.lgf");
        let (guid, dicts, has_records) = match load_lgf(&path)? {
            Some(x) => x,
            None => {
                let guid = o.guid.unwrap_or_else(random_guid);
                std::fs::write(&path, header(&guid))?;
                (guid, Dicts::default(), false)
            }
        };
        let lgf = OpenOptions::new().append(true).open(&path)?;
        Ok(Writer {
            dir: dir.to_owned(),
            guid,
            period: o.period,
            dicts,
            lgf,
            lgf_has_records: has_records,
            cur: None,
            txns: HashMap::new(),
            stale: false,
            checked: HashSet::new(),
        })
    }

    pub fn guid(&self) -> Uuid16 {
        self.guid
    }

    #[cfg(feature = "testdata")]
    pub(crate) fn preview_lgf(&self, e: &Event) -> Vec<brace::Node> {
        self.dicts.preview(e)
    }

    #[cfg(feature = "testdata")]
    pub(crate) fn push_lgf_record(&mut self, n: &brace::Node) -> Result<(), Error> {
        self.refresh()?;
        self.dicts.apply(n, &mut 0)?;
        let r = self.write_lgf(std::slice::from_ref(n));
        if matches!(r, Err(Error::Io(_))) {
            self.stale = true;
        }
        r
    }

    fn refresh(&mut self) -> Result<(), Error> {
        if !self.stale {
            return Ok(());
        }
        let path = self.dir.join("1Cv8.lgf");
        let (guid, dicts, has) = load_lgf(&path)?.ok_or_else(|| bad(".lgf vanished"))?;
        if guid != self.guid {
            return Err(bad(".lgf guid changed"));
        }
        self.lgf = OpenOptions::new().append(true).open(&path)?;
        self.dicts = dicts;
        self.lgf_has_records = has;
        self.cur = None;
        self.checked.clear();
        let mut complete: HashMap<PathBuf, Vec<u64>> = HashMap::new();
        for t in self.txns.values() {
            if !complete.contains_key(&t.path) {
                let offsets = check_lgp(&t.path, &self.guid)
                    .map(|c| c.1)
                    .unwrap_or_default();
                complete.insert(t.path.clone(), offsets);
            }
        }
        self.txns.retain(|_, t| {
            complete
                .get(&t.path)
                .is_some_and(|o| o.binary_search(&t.offset).is_ok())
                && begin_intact(t)
        });
        self.stale = false;
        Ok(())
    }

    pub fn write(&mut self, e: Event) -> Result<(), Error> {
        self.refresh()?;
        let Some(t) = e.txn else {
            return self.append(e, TxStatus::None, None).map(|_| ());
        };
        let tx = self
            .txns
            .get(&t)
            .ok_or_else(|| bad("unknown transaction"))?;
        let pair = (tx.ticks, tx.offset);
        self.append(e, TxStatus::Unfinished, Some(pair)).map(|_| ())
    }

    pub fn begin_txn(&mut self, token: TxnToken, e: Event) -> Result<(), Error> {
        self.refresh()?;
        if self.txns.contains_key(&token) {
            return Err(bad("transaction already open"));
        }
        let ticks = e.time.trunc_seconds().ticks();
        let (path, offset) = self.append(e, TxStatus::Unfinished, None)?;
        self.txns.insert(
            token,
            OpenTxn {
                path,
                offset,
                ticks,
            },
        );
        Ok(())
    }

    pub fn end_txn(&mut self, token: TxnToken, status: TxStatus, e: Event) -> Result<(), Error> {
        if !matches!(status, TxStatus::Committed | TxStatus::RolledBack) {
            return Err(bad("transaction must end with C or R"));
        }
        self.refresh()?;
        let tx = self
            .txns
            .get(&token)
            .ok_or_else(|| bad("unknown transaction"))?;
        let (path, offset, ticks) = (tx.path.clone(), tx.offset, tx.ticks);
        self.append(e, status, Some((ticks, offset)))?;
        let r = rewrite_status(&path, offset, status);
        if r.is_err() {
            self.stale = true;
        }
        r?;
        self.txns.remove(&token);
        Ok(())
    }

    fn append(
        &mut self,
        e: Event,
        status: TxStatus,
        pair: Option<(i64, u64)>,
    ) -> Result<(PathBuf, u64), Error> {
        let r = self.append_inner(e, status, pair);
        if matches!(r, Err(Error::Io(_))) {
            self.stale = true;
        }
        r
    }

    fn append_inner(
        &mut self,
        e: Event,
        status: TxStatus,
        pair: Option<(i64, u64)>,
    ) -> Result<(PathBuf, u64), Error> {
        let (ix, new) = self.dicts.resolve(&e);
        self.write_lgf(&new)?;
        self.select_lgp(&e)?;
        let header_len = header(&self.guid).len() as u64;
        let cur = self.cur.as_mut().ok_or_else(|| bad("no current .lgp"))?;
        let sep = if cur.len > header_len { SEP.len() } else { 0 } as u64;
        let offset = cur.len + sep;
        let tx = match (pair, status) {
            (Some(p), _) => p,
            (None, TxStatus::Unfinished) => (e.time.trunc_seconds().ticks(), offset),
            (None, _) => (0, 0),
        };
        let node = event_node(&e, &ix, status, tx);
        let mut buf = Vec::new();
        if sep > 0 {
            buf.extend_from_slice(SEP);
        }
        buf.extend_from_slice(brace::to_text(&node).as_bytes());
        cur.file.write_all(&buf)?;
        cur.len += buf.len() as u64;
        Ok((self.dir.join(&cur.name), offset))
    }

    fn write_lgf(&mut self, nodes: &[brace::Node]) -> Result<(), Error> {
        let mut buf = Vec::new();
        for n in nodes {
            if self.lgf_has_records || !buf.is_empty() {
                buf.extend_from_slice(SEP);
            }
            buf.extend_from_slice(brace::to_text(n).as_bytes());
        }
        if !buf.is_empty() {
            self.lgf.write_all(&buf)?;
            self.lgf_has_records = true;
            self.lgf.sync_data()?;
        }
        Ok(())
    }

    fn select_lgp(&mut self, e: &Event) -> Result<(), Error> {
        let name = lgp_name(e.time, self.period);
        if self.cur.as_ref().is_some_and(|c| c.name == name) {
            return Ok(());
        }
        let path = self.dir.join(&name);
        let mut len = if self.checked.contains(&name) {
            match std::fs::metadata(&path) {
                Ok(m) => m.len(),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => 0,
                Err(err) => return Err(err.into()),
            }
        } else {
            check_lgp(&path, &self.guid).map_err(in_file(&name))?.0
        };
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        if len == 0 {
            let h = header(&self.guid);
            file.write_all(&h)?;
            len = h.len() as u64;
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(path.with_extension("lgx"))?;
        }
        self.checked.insert(name.clone());
        self.cur = Some(Current { name, file, len });
        Ok(())
    }
}

fn check_lgp(path: &Path, guid: &Uuid16) -> Result<(u64, Vec<u64>), Error> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((0, Vec::new())),
        Err(e) => return Err(e.into()),
    };
    let len = bytes.len() as u64;
    let (keep, records) = if header_prefix(&bytes, guid) {
        (0, Vec::new())
    } else {
        let (g, body_at) = parse_header(&bytes)?;
        if g != *guid {
            return Err(bad("GUID differs from 1Cv8.lgf"));
        }
        let body = bytes.get(body_at..).unwrap_or_default();
        let scan = scan_records(body, body_at as u64, FileKind::Lgp);
        let records = scan.records.iter().map(|r| r.0).collect();
        (scan.tail.unwrap_or(len), records)
    };
    if keep < len {
        OpenOptions::new().write(true).open(path)?.set_len(keep)?;
    }
    Ok((keep, records))
}

fn begin_intact(t: &OpenTxn) -> bool {
    let Ok(mut f) = File::open(&t.path) else {
        return false;
    };
    let mut b = [0u8; 18];
    let ok = f
        .seek(SeekFrom::Start(t.offset))
        .and_then(|_| std::io::Read::read_exact(&mut f, &mut b))
        .is_ok();
    ok && b.first() == Some(&b'{')
        && b.get(1..15)
            .is_some_and(|d| d.iter().all(u8::is_ascii_digit))
        && b.get(15..) == Some(b",U,")
}

fn rewrite_status(path: &Path, offset: u64, status: TxStatus) -> Result<(), Error> {
    let mut f = OpenOptions::new().write(true).open(path)?;
    f.seek(SeekFrom::Start(offset + 16))?;
    let mut letter = [0u8; 4];
    f.write_all(status.letter().encode_utf8(&mut letter).as_bytes())?;
    Ok(())
}
