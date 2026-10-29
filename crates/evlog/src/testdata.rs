use std::collections::HashMap;
use std::path::{Path, PathBuf};

use brace::Node;

use crate::dict::DictKey;
use crate::reader::LogData;
use crate::{Error, Event, Period, SeparatorValue, TxStatus, TxnToken, Writer, WriterOptions};

pub fn out_dir() -> Option<PathBuf> {
    if let Some(value) = std::env::var_os("ONEC_EVLOG_OUT") {
        let path = PathBuf::from(value);
        assert!(
            !path.as_os_str().is_empty() && path.is_dir(),
            "ONEC_EVLOG_OUT must name an existing evlog corpus directory: {}",
            path.display()
        );
        return Some(path);
    }
    let p = match std::env::var_os("ONEC_OUT") {
        Some(p) => PathBuf::from(p),
        None => Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .find(|a| a.join("tests/conformance/manifest.json").is_file())?
            .join("out"),
    };
    p.is_dir().then_some(p)
}

pub fn log_dirs(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        if d.join("1Cv8.lgf").is_file() {
            out.push(d.clone());
        }
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push(e.path());
            }
        }
    }
    out.sort();
    out
}

pub fn files(dir: &Path, exts: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| {
            Path::new(n)
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| exts.contains(&x))
        })
        .collect();
    out.sort();
    out
}

fn period_of(name: &str) -> Result<Period, Error> {
    match name.get(8..10) {
        Some("00") => Ok(Period::Day),
        Some(h) if h.bytes().all(|b| b.is_ascii_digit()) => Ok(Period::Hour),
        _ => Err(Error::Malformed(format!("bad .lgp name {name}"))),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Replayed {
    pub begins_left_u: usize,
    pub reordered: usize,
}

const BEGIN: &str = "_$Transaction$_.Begin";
const COMMIT: &str = "_$Transaction$_.Commit";
const ROLLBACK: &str = "_$Transaction$_.Rollback";

fn value_positions(order: &[Node]) -> HashMap<(u32, u32), usize> {
    let mut m = HashMap::new();
    for (at, n) in order.iter().enumerate() {
        if let Some([k, _, sep, idx]) = n.as_list()
            && k.as_u32() == Some(10)
            && let (Some(sep), Some(idx)) = (sep.as_u32(), idx.as_u32())
        {
            m.entry((sep, idx)).or_insert(at);
        }
    }
    m
}

fn configuration_order(e: &mut Event, log: &LogData, pos: &HashMap<(u32, u32), usize>) {
    let at = |p: &SeparatorValue| {
        let sep = log.dicts.get(&DictKey::Separator(
            p.separator.uuid,
            p.separator.name.clone(),
        ))?;
        let val = log
            .dicts
            .get(&DictKey::SeparatorValue(sep, p.value.clone()))?;
        pos.get(&(sep, val)).copied()
    };
    e.separators
        .sort_by_cached_key(|p| at(p).unwrap_or(usize::MAX));
}

struct Lgf<'a> {
    order: &'a [Node],
    p: usize,
    after_reorder: bool,
}

fn record_kind(n: &Node) -> Option<u32> {
    n.as_list().and_then(|l| l.first()).and_then(Node::as_u32)
}

fn is_user_mark(n: &Node) -> bool {
    record_kind(n) == Some(13)
}

impl<'a> Lgf<'a> {
    fn new(order: &'a [Node]) -> Self {
        Lgf {
            order,
            p: 0,
            after_reorder: false,
        }
    }

    fn push(&mut self, w: &mut Writer) -> Result<(), Error> {
        let n = self
            .order
            .get(self.p)
            .ok_or_else(|| Error::Malformed("replay: original .lgf has no more records".into()))?;
        w.push_lgf_record(n)?;
        self.p += 1;
        Ok(())
    }

    fn flush_marks(&mut self, w: &mut Writer) -> Result<(), Error> {
        while let Some(n) = self.order.get(self.p).filter(|n| is_user_mark(n)) {
            if !self.after_reorder {
                return Err(Error::Malformed(format!(
                    "replay: 1C wrote {} that the writer did not",
                    brace::to_text(n)
                )));
            }
            self.push(w)?;
        }
        self.after_reorder = false;
        Ok(())
    }

    fn sync(&mut self, w: &mut Writer, e: &Event) -> Result<bool, Error> {
        self.flush_marks(w)?;
        let new = w.preview_lgf(e);
        let same = self.order.get(self.p..self.p + new.len()).is_some_and(|o| {
            o.iter()
                .zip(&new)
                .all(|(a, b)| brace::to_text(a) == brace::to_text(b))
        });
        if same {
            self.p += new.len();
            return Ok(false);
        }
        let mut moved = false;
        while !w.preview_lgf(e).is_empty() {
            if let Some(n) = self.order.get(self.p)
                && !matches!(record_kind(n), Some(1..=8 | 13))
            {
                return Err(Error::Malformed(format!(
                    "replay: 1C wrote {} before the event that needs it",
                    brace::to_text(n)
                )));
            }
            self.push(w)?;
            moved = true;
        }
        self.after_reorder = moved;
        Ok(moved)
    }
}

fn restore_unfinished(path: &Path, offset: u64) -> Result<(), Error> {
    use std::io::{Read, Seek, SeekFrom, Write};
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?;
    let at = offset + 16;
    let mut b = [0u8; 1];
    f.seek(SeekFrom::Start(at))?;
    f.read_exact(&mut b)?;
    if !matches!(&b, b"C" | b"R") {
        return Err(Error::Malformed(format!(
            "{}: no Begin status at {at}",
            path.display()
        )));
    }
    f.seek(SeekFrom::Start(at))?;
    f.write_all(b"U")?;
    Ok(())
}

pub fn replay(log: &LogData, dir: &Path) -> Result<Replayed, Error> {
    let open = |period| {
        Writer::open(
            dir,
            WriterOptions {
                period,
                guid: Some(log.guid),
            },
        )
    };
    let mut cur: Option<(Period, Writer)> = None;
    let mut open_txns: HashMap<TxnToken, (&str, TxStatus)> = HashMap::new();
    let mut lost = 0;
    let mut lgf = Lgf::new(&log.lgf_order);
    let pos = value_positions(&log.lgf_order);
    let mut reordered = 0;
    for r in &log.records {
        let period = period_of(&r.file)?;
        let w = match cur.take() {
            Some((p, w)) if p == period => cur.insert((p, w)),
            old => {
                drop(old);
                open_txns.clear();
                cur.insert((period, open(period)?))
            }
        };
        let w = &mut w.1;
        let mut e = r.event.clone();
        e.status = TxStatus::None;
        e.txn = None;
        configuration_order(&mut e, log, &pos);
        if lgf.sync(w, &e)? {
            reordered += 1;
        }
        let token = TxnToken(r.tx.1);
        let end = match e.event.as_str() {
            BEGIN => {
                let t = TxnToken(r.offset);
                w.begin_txn(t, e)?;
                open_txns.insert(t, (&r.file, r.status));
                continue;
            }
            COMMIT => TxStatus::Committed,
            ROLLBACK => TxStatus::RolledBack,
            _ => {
                if r.tx != (0, 0) {
                    e.txn = Some(token);
                }
                w.write(e)?;
                continue;
            }
        };
        w.end_txn(token, end, e)?;
        if let Some((file, TxStatus::Unfinished)) = open_txns.remove(&token) {
            restore_unfinished(&dir.join(file), token.0)?;
            lost += 1;
        }
    }
    match cur.as_mut() {
        Some((_, w)) => lgf.flush_marks(w)?,
        None => {
            open(Period::Day)?;
        }
    }
    Ok(Replayed {
        begins_left_u: lost,
        reordered,
    })
}

pub fn log_inf_files(out: &Path) -> Vec<PathBuf> {
    let skip = [out.join("brace/unf"), out.join("brace/private")];
    let is = |p: &Path, name: &str| {
        p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.eq_ignore_ascii_case(name))
    };
    let mut found = Vec::new();
    let mut stack = vec![out.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(t) = e.file_type() else { continue };
            if t.is_dir() {
                if !skip.contains(&p) {
                    stack.push(p);
                }
            } else if t.is_file()
                && is(&p, "log.inf")
                && p.parent().is_some_and(|q| is(q, "PARAMS"))
            {
                found.push(p);
            }
        }
    }
    found.sort();
    found
}
