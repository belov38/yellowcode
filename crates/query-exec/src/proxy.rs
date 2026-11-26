use std::ops::Bound;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, Sender, channel};

use ibschema::storage::{FieldId, IndexId, StorageMap, TableId};
use query_ir::host::PresentationInput;
use query_ir::{ExecError, QStr, QValue, RefAtom};
use query_syntax::{INLINE_DEPTH, Lang, MAX_DEPTH};

use crate::ops::present::Presenter;
use crate::source::{DataSource, IndexPrefixes, Row, RowIter};

const BATCH_INIT_ROWS: usize = 32;
const BATCH_MAX_ROWS: usize = 256;
const BATCH_MAX_BYTES: u64 = 256 << 10;

enum Req {
    Scan(TableId),
    Range {
        t: TableId,
        i: IndexId,
        lo: Bound<Vec<QValue>>,
        hi: Bound<Vec<QValue>>,
        desc: bool,
    },
    NextBatch(usize, usize),
    Close(usize),
    GetById(TableId, QValue),
    Count(TableId),
    ScanFields(TableId, Vec<FieldId>),
    RangeFields {
        t: TableId,
        i: IndexId,
        lo: Bound<Vec<QValue>>,
        hi: Bound<Vec<QValue>>,
        desc: bool,
        fields: Vec<FieldId>,
    },
    GetByIdFields(TableId, QValue, Vec<FieldId>),
    IndexPrefixes {
        t: TableId,
        i: IndexId,
        n: usize,
        limit: usize,
    },
    GetByIdIn(TableId, Vec<QValue>, QValue, Vec<FieldId>),
    PresentLang,
    TypeName(RefAtom),
    HostPresentation(PresentationInput),
}

enum BatchTail {
    More,
    Eof,
    Err(ExecError),
}

enum Resp {
    Handle(Result<usize, ExecError>),
    Batch { rows: Vec<Row>, tail: BatchTail },
    Row(Result<Option<Row>, ExecError>),
    Count(u64),
    Prefixes(Result<Option<IndexPrefixes>, ExecError>),
    Lang(Lang),
    Text(String),
    Presented(Result<QStr, ExecError>),
}

fn gone() -> ExecError {
    ExecError::Fatal("proxy: the thread of the data source is gone".to_string())
}

fn protocol() -> ExecError {
    ExecError::Fatal("proxy: unexpected response".to_string())
}

fn cancelled() -> ExecError {
    ExecError::Fatal("выполнение прервано".to_string())
}

fn is_cancelled() -> bool {
    ibschema::store::scoped_key_cancel(|flag| flag.load(Ordering::Relaxed))
}

struct Proxy<'m> {
    map: &'m StorageMap,
    tx: Sender<Req>,
    rx: Receiver<Resp>,
}

impl Proxy<'_> {
    fn call(&self, r: Req) -> Result<Resp, ExecError> {
        self.tx.send(r).map_err(|_| gone())?;
        self.rx.recv().map_err(|_| gone())
    }

    fn open(&self, r: Req) -> Result<RowIter<'_>, ExecError> {
        match self.call(r)? {
            Resp::Handle(h) => Ok(Box::new(ProxyIter {
                p: self,
                h: h?,
                buf: Vec::new().into_iter(),
                tail: None,
                done: false,
                batch_cap: BATCH_INIT_ROWS,
            })),
            _ => Err(protocol()),
        }
    }
}

struct ProxyIter<'p, 'm> {
    p: &'p Proxy<'m>,
    h: usize,
    buf: std::vec::IntoIter<Row>,
    tail: Option<ExecError>,
    done: bool,
    batch_cap: usize,
}

impl Iterator for ProxyIter<'_, '_> {
    type Item = Result<Row, ExecError>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(row) = self.buf.next() {
            return Some(Ok(row));
        }
        if let Some(err) = self.tail.take() {
            self.done = true;
            return Some(Err(err));
        }
        if self.done {
            return None;
        }
        let cap = self.batch_cap;
        self.batch_cap = cap.saturating_mul(2).min(BATCH_MAX_ROWS);
        match self.p.call(Req::NextBatch(self.h, cap)) {
            Ok(Resp::Batch { rows, tail }) => {
                {
                    self.buf = rows.into_iter();
                }
                match tail {
                    BatchTail::More => {}
                    BatchTail::Eof => self.done = true,
                    BatchTail::Err(e) => {
                        self.tail = Some(e);
                        self.done = true;
                    }
                }
                if let Some(row) = self.buf.next() {
                    return Some(Ok(row));
                }
                if let Some(err) = self.tail.take() {
                    return Some(Err(err));
                }
                self.done = true;
                None
            }
            Ok(_) => {
                self.done = true;
                Some(Err(protocol()))
            }
            Err(e) => {
                self.done = true;
                Some(Err(e))
            }
        }
    }
}

impl Drop for ProxyIter<'_, '_> {
    fn drop(&mut self) {
        {
            self.buf = Vec::new().into_iter();
            self.tail = None;
        }
        let _ = self.p.tx.send(Req::Close(self.h));
    }
}

impl DataSource for Proxy<'_> {
    fn map(&self) -> &StorageMap {
        self.map
    }

    fn scan(&self, t: TableId) -> Result<RowIter<'_>, ExecError> {
        self.open(Req::Scan(t))
    }

    fn range(
        &self,
        t: TableId,
        i: IndexId,
        lo: Bound<&[QValue]>,
        hi: Bound<&[QValue]>,
        desc: bool,
    ) -> Result<RowIter<'_>, ExecError> {
        self.open(Req::Range {
            t,
            i,
            lo: lo.map(<[QValue]>::to_vec),
            hi: hi.map(<[QValue]>::to_vec),
            desc,
        })
    }

    fn get_by_id(&self, t: TableId, id: &QValue) -> Result<Option<Row>, ExecError> {
        match self.call(Req::GetById(t, id.clone()))? {
            Resp::Row(r) => r,
            _ => Err(protocol()),
        }
    }

    fn row_count_estimate(&self, t: TableId) -> u64 {
        match self.call(Req::Count(t)) {
            Ok(Resp::Count(n)) => n,
            _ => 0,
        }
    }

    fn scan_fields(&self, t: TableId, fields: &[FieldId]) -> Result<RowIter<'_>, ExecError> {
        self.open(Req::ScanFields(t, fields.to_vec()))
    }

    fn range_fields(
        &self,
        t: TableId,
        i: IndexId,
        lo: Bound<&[QValue]>,
        hi: Bound<&[QValue]>,
        desc: bool,
        fields: &[FieldId],
    ) -> Result<RowIter<'_>, ExecError> {
        self.open(Req::RangeFields {
            t,
            i,
            lo: lo.map(<[QValue]>::to_vec),
            hi: hi.map(<[QValue]>::to_vec),
            desc,
            fields: fields.to_vec(),
        })
    }

    fn get_by_id_fields(
        &self,
        t: TableId,
        id: &QValue,
        fields: &[FieldId],
    ) -> Result<Option<Row>, ExecError> {
        match self.call(Req::GetByIdFields(t, id.clone(), fields.to_vec()))? {
            Resp::Row(r) => r,
            _ => Err(protocol()),
        }
    }

    fn index_prefixes(
        &self,
        t: TableId,
        i: IndexId,
        n: usize,
        limit: usize,
    ) -> Result<Option<IndexPrefixes>, ExecError> {
        match self.call(Req::IndexPrefixes { t, i, n, limit })? {
            Resp::Prefixes(r) => r,
            _ => Err(protocol()),
        }
    }

    fn get_by_id_in(
        &self,
        t: TableId,
        area: &[QValue],
        id: &QValue,
        fields: &[FieldId],
    ) -> Result<Option<Row>, ExecError> {
        match self.call(Req::GetByIdIn(
            t,
            area.to_vec(),
            id.clone(),
            fields.to_vec(),
        ))? {
            Resp::Row(r) => r,
            _ => Err(protocol()),
        }
    }
}

impl Presenter for Proxy<'_> {
    fn lang(&self) -> Lang {
        match self.call(Req::PresentLang) {
            Ok(Resp::Lang(l)) => l,
            _ => Lang::Ru,
        }
    }

    fn type_name(&self, atom: RefAtom) -> String {
        match self.call(Req::TypeName(atom)) {
            Ok(Resp::Text(s)) => s,
            _ => String::new(),
        }
    }

    fn host_presentation(&self, input: &PresentationInput) -> Result<QStr, ExecError> {
        match self.call(Req::HostPresentation(input.clone()))? {
            Resp::Presented(r) => r,
            _ => Err(protocol()),
        }
    }
}

struct Open<'s> {
    iters: Vec<Option<RowIter<'s>>>,
    free: Vec<usize>,
}

impl<'s> Open<'s> {
    fn put(&mut self, it: Result<RowIter<'s>, ExecError>) -> Resp {
        Resp::Handle(it.map(|it| match self.free.pop() {
            Some(h) => {
                if let Some(slot) = self.iters.get_mut(h) {
                    *slot = Some(it);
                }
                h
            }
            None => {
                self.iters.push(Some(it));
                self.iters.len() - 1
            }
        }))
    }

    fn next_batch(&mut self, h: usize, cap: usize) -> Resp {
        let Some(it) = self.iters.get_mut(h).and_then(Option::as_mut) else {
            return Resp::Batch {
                rows: Vec::new(),
                tail: BatchTail::Eof,
            };
        };
        let cap = cap.clamp(1, BATCH_MAX_ROWS);
        let mut rows = Vec::new();
        let mut bytes = 0u64;
        let mut tail = BatchTail::More;
        while rows.len() < cap {
            if is_cancelled() {
                tail = BatchTail::Err(cancelled());
                break;
            }
            match it.next() {
                Some(Ok(row)) => {
                    bytes = bytes.saturating_add(crate::model::row_cap_model(&row));
                    rows.push(row);
                    if bytes >= BATCH_MAX_BYTES {
                        break;
                    }
                }
                Some(Err(e)) => {
                    tail = BatchTail::Err(e);
                    break;
                }
                None => {
                    tail = BatchTail::Eof;
                    break;
                }
            }
        }
        Resp::Batch { rows, tail }
    }

    fn close(&mut self, h: usize) {
        if let Some(slot) = self.iters.get_mut(h)
            && slot.take().is_some()
        {
            self.free.push(h);
        }
    }
}

fn serve(
    src: &dyn DataSource,
    presenter: Option<&dyn Presenter>,
    rx: Receiver<Req>,
    tx: Sender<Resp>,
) {
    let mut open = Open {
        iters: Vec::new(),
        free: Vec::new(),
    };
    for req in rx.iter() {
        let resp = match req {
            Req::Scan(t) => open.put(src.scan(t)),
            Req::Range { t, i, lo, hi, desc } => open.put(src.range(
                t,
                i,
                lo.as_ref().map(Vec::as_slice),
                hi.as_ref().map(Vec::as_slice),
                desc,
            )),
            Req::NextBatch(h, cap) => open.next_batch(h, cap),
            Req::Close(h) => {
                open.close(h);
                continue;
            }
            Req::GetById(t, id) => Resp::Row(src.get_by_id(t, &id)),
            Req::Count(t) => Resp::Count(src.row_count_estimate(t)),
            Req::ScanFields(t, fields) => open.put(src.scan_fields(t, &fields)),
            Req::RangeFields {
                t,
                i,
                lo,
                hi,
                desc,
                fields,
            } => open.put(src.range_fields(
                t,
                i,
                lo.as_ref().map(Vec::as_slice),
                hi.as_ref().map(Vec::as_slice),
                desc,
                &fields,
            )),
            Req::GetByIdFields(t, id, fields) => Resp::Row(src.get_by_id_fields(t, &id, &fields)),
            Req::IndexPrefixes { t, i, n, limit } => {
                Resp::Prefixes(src.index_prefixes(t, i, n, limit))
            }
            Req::GetByIdIn(t, area, id, fields) => {
                Resp::Row(src.get_by_id_in(t, &area, &id, &fields))
            }
            Req::PresentLang => Resp::Lang(presenter.map_or(Lang::Ru, Presenter::lang)),
            Req::TypeName(atom) => {
                Resp::Text(presenter.map_or_else(String::new, |p| p.type_name(atom)))
            }
            Req::HostPresentation(input) => Resp::Presented(match presenter {
                Some(p) => p.host_presentation(&input),
                None => Err(ExecError::Later {
                    what: "ПРЕДСТАВЛЕНИЕ без хоста",
                    stage: "Q3",
                }),
            }),
        };
        if tx.send(resp).is_err() {
            break;
        }
    }
}

pub(crate) fn stack_size(depth: u32) -> usize {
    (4 << 20) + (64 << 10) * depth.min(MAX_DEPTH + 1) as usize
}

pub(crate) fn with_source_and_presenter<R: Send>(
    depth: u32,
    src: &dyn DataSource,
    presenter: Option<&dyn Presenter>,
    f: impl FnOnce(&dyn DataSource, Option<&dyn Presenter>) -> R + Send,
) -> Result<R, ExecError> {
    if depth <= INLINE_DEPTH {
        return Ok(f(src, presenter));
    }
    let map = src.map();
    let has_presenter = presenter.is_some();
    let (req_tx, req_rx) = channel::<Req>();
    let (resp_tx, resp_rx) = channel::<Resp>();
    let mut job = Some(f);
    let mut out: Option<R> = None;
    let (job_ref, out_ref) = (&mut job, &mut out);
    std::thread::scope(move |s| {
        #[cfg(test)]
        agg_bridge_ref.before_spawn()?;
        let spawned = std::thread::Builder::new()
            .stack_size(stack_size(depth))
            .spawn_scoped(s, move || {
                let proxy = Proxy {
                    map,
                    tx: req_tx,
                    rx: resp_rx,
                };
                if let Some(f) = job_ref.take() {
                    let proxy_presenter = has_presenter.then_some(&proxy as &dyn Presenter);
                    *out_ref = Some(f(&proxy, proxy_presenter));
                }
            });
        match spawned {
            Ok(h) => {
                serve(src, presenter, req_rx, resp_tx);
                if let Err(p) = h.join() {
                    std::panic::resume_unwind(p);
                }
                Ok(())
            }
            Err(e) => Err(e),
        }
    })
    .map_err(|e| ExecError::Fatal(format!("proxy: cannot start the executor thread: {e}")))?;
    out.ok_or_else(|| ExecError::Fatal("proxy: the job did not run".to_string()))
}
