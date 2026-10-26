use super::RelL;
use crate::exec_error::ExecError;
use crate::ir::SrcId;

pub(super) type R<T> = Result<T, ExecError>;

pub(super) fn fatal<T>(msg: impl Into<String>) -> R<T> {
    Err(ExecError::Fatal(msg.into()))
}

pub(super) fn later<T>(what: &'static str, stage: &'static str) -> R<T> {
    Err(ExecError::Later { what, stage })
}

pub(super) fn internal<T>(msg: &str) -> R<T> {
    Err(ExecError::Fatal(format!("внутренняя ошибка lower: {msg}")))
}

pub(super) fn p16(n: i64) -> R<u16> {
    match u16::try_from(n) {
        Ok(v) => Ok(v),
        Err(_) => fatal("слишком много колонок в строке запроса"),
    }
}

pub(super) struct Row {
    pub signal: Option<u16>,
    pub rel: RelL,
    pub width: usize,
    pub rep: Option<SrcId>,
    pub keys: Option<usize>,
}

impl Row {
    pub fn empty() -> Row {
        Row {
            signal: None,
            rel: RelL::Empty,
            width: 0,
            rep: None,
            keys: None,
        }
    }

    pub fn take(&mut self) -> Box<RelL> {
        Box::new(std::mem::replace(&mut self.rel, RelL::Empty))
    }
}

pub(super) struct Offsets {
    parent: Vec<u32>,
    delta: Vec<i64>,
    origin: Vec<i64>,
    size: Vec<u32>,
}

impl Offsets {
    pub fn new(n: usize) -> Offsets {
        Offsets {
            parent: (0..n as u32).collect(),
            delta: vec![0; n],
            origin: vec![0; n],
            size: vec![1; n],
        }
    }

    fn par(&self, i: u32) -> u32 {
        self.parent.get(i as usize).copied().unwrap_or(i)
    }

    fn del(&self, i: u32) -> i64 {
        self.delta.get(i as usize).copied().unwrap_or(0)
    }

    fn find(&mut self, s: u32) -> (u32, i64) {
        let (mut root, mut total) = (s, 0i64);
        while self.par(root) != root {
            total += self.del(root);
            root = self.par(root);
        }
        let (mut cur, mut rem) = (s, total);
        while self.par(cur) != cur {
            let (next, d) = (self.par(cur), self.del(cur));
            if let Some(p) = self.parent.get_mut(cur as usize) {
                *p = root;
            }
            if let Some(x) = self.delta.get_mut(cur as usize) {
                *x = rem;
            }
            rem -= d;
            cur = next;
        }
        (root, total)
    }

    pub fn offset(&mut self, s: SrcId) -> i64 {
        let (root, off) = self.find(s.0);
        off + self.origin.get(root as usize).copied().unwrap_or(0)
    }

    pub fn same(&mut self, a: SrcId, b: SrcId) -> bool {
        self.find(a.0).0 == self.find(b.0).0
    }

    pub fn merge(&mut self, a: SrcId, wa: usize, b: SrcId, wb: usize, a_first: bool) -> SrcId {
        let (ra, _) = self.find(a.0);
        let (rb, _) = self.find(b.0);
        if ra == rb {
            return SrcId(ra);
        }
        let (start_a, start_b) = if a_first {
            (0, wa as i64)
        } else {
            (wb as i64, 0)
        };
        let pos_a = start_a + self.origin.get(ra as usize).copied().unwrap_or(0);
        let pos_b = start_b + self.origin.get(rb as usize).copied().unwrap_or(0);
        let (sa, sb) = (self.sz(ra), self.sz(rb));
        let (big, small, pos_big, pos_small) = if sa >= sb {
            (ra, rb, pos_a, pos_b)
        } else {
            (rb, ra, pos_b, pos_a)
        };
        if let Some(p) = self.parent.get_mut(small as usize) {
            *p = big;
        }
        if let Some(d) = self.delta.get_mut(small as usize) {
            *d = pos_small - pos_big;
        }
        if let Some(o) = self.origin.get_mut(big as usize) {
            *o = pos_big;
        }
        if let Some(s) = self.size.get_mut(big as usize) {
            *s = sa.saturating_add(sb);
        }
        SrcId(big)
    }

    fn sz(&self, i: u32) -> u32 {
        self.size.get(i as usize).copied().unwrap_or(1)
    }
}
