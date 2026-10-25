use std::path::Path;

use brace::Node;

use crate::dict::Dicts;
use crate::record::{event_from_node, header, parse_header};
use crate::{Error, Event, TxStatus, Uuid16};

#[derive(Debug, Clone, PartialEq)]
pub struct LogRecord {
    pub file: String,
    pub offset: u64,
    pub event: Event,
    pub tx: (i64, u64),
    pub status: TxStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Damage {
    pub file: String,
    pub offset: u64,
    pub len: u64,
}

#[derive(Debug, Clone)]
pub struct LogData {
    pub guid: Uuid16,
    pub dicts: Dicts,
    pub lgf_order: Vec<Node>,
    pub records: Vec<LogRecord>,
    pub damage: Vec<Damage>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Lgf,
    Lgp,
}

const LGF: &str = "1Cv8.lgf";

pub(crate) struct ParsedFile {
    pub guid: Uuid16,
    pub records: Vec<(u64, Node)>,
    pub damage: Vec<(u64, u64)>,
    pub tail: Option<u64>,
}

pub(crate) fn parse_file(bytes: &[u8], kind: FileKind) -> Result<ParsedFile, Error> {
    let (guid, body_at) = parse_header(bytes)?;
    let body = bytes.get(body_at..).unwrap_or_default();
    let scan = scan_records(body, body_at as u64, kind);
    let mut records = Vec::with_capacity(scan.records.len());
    for (off, raw) in scan.records {
        let n = brace::parse(record_text(off, raw)?)
            .map_err(|e| Error::Malformed(format!("record at {off}: {e}")))?;
        records.push((off, n));
    }
    Ok(ParsedFile {
        guid,
        records,
        damage: scan.damage,
        tail: scan.tail,
    })
}

fn record_text(off: u64, raw: &[u8]) -> Result<&str, Error> {
    std::str::from_utf8(raw)
        .map_err(|_| Error::Malformed(format!("invalid UTF-8 in record at {off}")))
}

pub(crate) fn in_file(name: &str) -> impl Fn(Error) -> Error + '_ {
    move |e| match e {
        Error::Malformed(m) => Error::Malformed(format!("{name}: {m}")),
        e => e,
    }
}

fn trim_nul(b: &[u8]) -> &[u8] {
    b.get(..b.iter().rposition(|&x| x != 0).map_or(0, |i| i + 1))
        .unwrap_or_default()
}

pub(crate) fn header_prefix(bytes: &[u8], guid: &Uuid16) -> bool {
    let h = header(guid);
    let data = trim_nul(bytes);
    data.len() < h.len() && h.starts_with(data)
}

pub fn read_dir(dir: &Path) -> Result<LogData, Error> {
    read_dir_with(dir, &mut |p| std::fs::read(p))
}

fn read_dir_with(
    dir: &Path,
    read: &mut dyn FnMut(&Path) -> std::io::Result<Vec<u8>>,
) -> Result<LogData, Error> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".lgp") && entry.file_type()?.is_file() {
            names.push(name);
        }
    }
    names.sort();
    let mut lgps = Vec::with_capacity(names.len());
    for name in names {
        let bytes = read(&dir.join(&name))?;
        lgps.push((name, bytes));
    }
    let lgf = parse_file(&read(&dir.join(LGF))?, FileKind::Lgf).map_err(in_file(LGF))?;
    let guid = lgf.guid;
    let mut damage = Vec::new();
    let mut add_damage = |file: &str, regions: &[(u64, u64)]| {
        damage.extend(regions.iter().map(|&(offset, len)| Damage {
            file: file.to_owned(),
            offset,
            len,
        }));
    };
    add_damage(LGF, &lgf.damage);
    let lgf_order: Vec<Node> = lgf.records.into_iter().map(|(_, n)| n).collect();
    let dicts = if lgf.damage.is_empty() {
        Dicts::from_lgf_records(&lgf_order)
    } else {
        let lost = lgf
            .damage
            .iter()
            .map(|d| (d.1.saturating_add(3) / 10).saturating_add(1))
            .sum();
        Dicts::from_lgf_records_with_gaps(&lgf_order, lost)
    }
    .map_err(in_file(LGF))?;
    let mut records = Vec::new();
    for (name, bytes) in lgps {
        if header_prefix(&bytes, &guid) {
            continue;
        }
        let ctx = in_file(&name);
        let p = parse_file(&bytes, FileKind::Lgp).map_err(&ctx)?;
        if p.guid != guid {
            return Err(Error::Malformed(format!(
                "{name}: GUID differs from 1Cv8.lgf"
            )));
        }
        add_damage(&name, &p.damage);
        for (offset, node) in p.records {
            let (event, tx) = event_from_node(&node, &dicts).map_err(&ctx)?;
            records.push(LogRecord {
                file: name.clone(),
                offset,
                status: event.status,
                event,
                tx,
            });
        }
    }
    Ok(LogData {
        guid,
        dicts,
        lgf_order,
        records,
        damage,
    })
}

const SEP: &[u8] = b",\r\n";

fn record_end(body: &[u8], start: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_str = false;
    for (i, &b) in body.iter().enumerate().skip(start) {
        match b {
            b'"' => in_str = !in_str,
            b'{' if !in_str => depth += 1,
            b'}' if !in_str => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
    }
    None
}

fn is_head(b: &[u8], kind: FileKind) -> bool {
    let digits = b.iter().skip(1).take_while(|c| c.is_ascii_digit()).count();
    let len_ok = match kind {
        FileKind::Lgf => digits > 0,
        FileKind::Lgp => digits == 14,
    };
    b.first() == Some(&b'{') && len_ok && b.get(1 + digits) == Some(&b',')
}

struct Candidate {
    at: usize,
    end: Option<usize>,
    nul: bool,
}

fn candidates(body: &[u8], kind: FileKind) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    let mut depth = [0usize; 2];
    let mut open: [Vec<(usize, usize)>; 2] = [Vec::new(), Vec::new()];
    let mut parity = 0usize;
    let mut last_nul = None;
    for (i, &b) in body.iter().enumerate() {
        let (Some(d), Some(heads)) = (depth.get_mut(parity), open.get_mut(parity)) else {
            continue;
        };
        match b {
            b'"' => parity ^= 1,
            0 => last_nul = Some(i),
            b'{' => {
                *d += 1;
                if is_head(body.get(i..).unwrap_or_default(), kind) {
                    heads.push((out.len(), *d));
                    out.push(Candidate {
                        at: i,
                        end: None,
                        nul: false,
                    });
                }
            }
            b'}' if *d > 0 => {
                if heads.last().is_some_and(|&(_, hd)| hd == *d)
                    && let Some((k, _)) = heads.pop()
                    && let Some(c) = out.get_mut(k)
                {
                    c.end = Some(i + 1);
                    c.nul = last_nul.is_some_and(|n| n > c.at);
                }
                *d -= 1;
            }
            _ => {}
        }
    }
    out
}

fn terminated(body: &[u8], end: usize) -> bool {
    let rest = body.get(end..).unwrap_or_default();
    let k = rest.iter().zip(SEP).take_while(|(a, b)| a == b).count();
    let after = rest.get(k..).unwrap_or_default();
    k == SEP.len() || matches!(after.first(), None | Some(0)) || after.starts_with(SEP)
}

fn is_tail(rest: &[u8], after_record: bool, kind: FileKind) -> bool {
    let data = trim_nul(rest);
    if data.is_empty() || after_record && SEP.starts_with(data) {
        return true;
    }
    let rec = if after_record {
        match data.strip_prefix(SEP) {
            Some(r) => r,
            None => return false,
        }
    } else {
        data
    };
    let head_inside = rec
        .iter()
        .enumerate()
        .skip(1)
        .any(|(i, &b)| b == b'{' && is_head(rec.get(i..).unwrap_or_default(), kind));
    rec.first() == Some(&b'{') && !rec.contains(&0) && !head_inside && record_end(rec, 0).is_none()
}

struct ParseBudget(usize);

impl ParseBudget {
    fn parses(&mut self, raw: &[u8]) -> bool {
        let Some(left) = self.0.checked_sub(raw.len()) else {
            return true;
        };
        self.0 = left;
        std::str::from_utf8(raw).is_ok_and(|t| brace::parse(t).is_ok())
    }
}

fn trim_seps(gap: &[u8], lead: bool, trail: bool) -> (usize, usize) {
    let s = if lead && gap.len() > SEP.len() && gap.starts_with(SEP) {
        SEP.len()
    } else {
        0
    };
    let e = if trail && gap.len() - s > SEP.len() && gap.ends_with(SEP) {
        gap.len() - SEP.len()
    } else {
        gap.len()
    };
    (s, e)
}

#[derive(Default)]
struct BodyScan {
    records: Vec<(usize, usize)>,
    damage: Vec<(usize, usize)>,
    tail: Option<usize>,
}

fn scan_body(body: &[u8], kind: FileKind) -> BodyScan {
    let cands = candidates(body, kind);
    let solid: Vec<usize> = cands
        .iter()
        .filter(|c| !c.nul && c.end.is_some_and(|e| terminated(body, e)))
        .map(|c| c.at)
        .collect();
    let solid_inside = |at: usize, end: usize| {
        let i = solid.partition_point(|&a| a <= at);
        solid.get(i).is_some_and(|&a| a <= end)
    };
    let head_inside = |at: usize, end: usize| {
        let i = cands.partition_point(|x| x.at <= at);
        cands.get(i).is_some_and(|x| x.at < end)
    };
    let mut budget = ParseBudget(body.len());
    let mut scan = BodyScan::default();
    let mut prev: Option<usize> = None;
    for c in &cands {
        let from = prev.unwrap_or(0);
        let Some(end) = c.end else { continue };
        if c.at < from || c.nul {
            continue;
        }
        let gap = body.get(from..c.at).unwrap_or_default();
        let normal = gap == if prev.is_some() { SEP } else { b"" };
        let complete = if terminated(body, end) {
            !head_inside(c.at, end) || budget.parses(body.get(c.at..end).unwrap_or_default())
        } else {
            normal && !solid_inside(c.at, end)
        };
        if !complete {
            continue;
        }
        if !normal {
            let (s, e) = trim_seps(gap, prev.is_some(), true);
            scan.damage.push((from + s, from + e));
        }
        scan.records.push((c.at, end));
        prev = Some(end);
    }
    let from = prev.unwrap_or(0);
    let rest = body.get(from..).unwrap_or_default();
    let spans = cands
        .iter()
        .any(|c| c.at < from && c.end.is_some_and(|e| e > from));
    if !spans && is_tail(rest, prev.is_some(), kind) {
        scan.tail = (!rest.is_empty()).then_some(from);
    } else {
        let (s, e) = trim_seps(rest, prev.is_some(), false);
        scan.damage.push((from + s, from + e));
    }
    scan
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scan<'a> {
    pub records: Vec<(u64, &'a [u8])>,
    pub damage: Vec<(u64, u64)>,
    pub tail: Option<u64>,
}

pub fn scan_records(body: &[u8], base: u64, kind: FileKind) -> Scan<'_> {
    let at = |p: usize| base.saturating_add(p as u64);
    let s = scan_body(body, kind);
    Scan {
        records: s
            .records
            .iter()
            .map(|&(a, e)| (at(a), body.get(a..e).unwrap_or_default()))
            .collect(),
        damage: s
            .damage
            .iter()
            .map(|&(a, e)| (at(a), (e - a) as u64))
            .collect(),
        tail: s.tail.map(at),
    }
}

#[allow(clippy::type_complexity)]
pub fn split_records(body: &[u8], base: u64) -> Result<(Vec<(u64, &str)>, usize), Error> {
    let kind = if is_head(body, FileKind::Lgp) {
        FileKind::Lgp
    } else {
        FileKind::Lgf
    };
    let at = |p: usize| base.saturating_add(p as u64);
    let s = scan_body(body, kind);
    if let Some(&(a, e)) = s.damage.first() {
        return Err(Error::Malformed(format!(
            "damaged records at {} ({} bytes)",
            at(a),
            e - a
        )));
    }
    let mut out = Vec::with_capacity(s.records.len());
    for &(a, e) in &s.records {
        out.push((
            at(a),
            record_text(at(a), body.get(a..e).unwrap_or_default())?,
        ));
    }
    Ok((out, s.records.last().map_or(0, |r| r.1)))
}
