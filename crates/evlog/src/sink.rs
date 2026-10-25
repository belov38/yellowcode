use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use crate::model::{Event, TxStatus, TxnToken};
use crate::{Error, Writer};

pub trait EventSink: Send + Sync {
    fn write(&self, e: Event);
    fn begin_txn(&self, token: TxnToken, e: Event);
    fn end_txn(&self, token: TxnToken, status: TxStatus, e: Event);
}

pub struct NullSink;

impl EventSink for NullSink {
    fn write(&self, _e: Event) {}
    fn begin_txn(&self, _token: TxnToken, _e: Event) {}
    fn end_txn(&self, _token: TxnToken, _status: TxStatus, _e: Event) {}
}

#[derive(Default)]
struct Inner {
    events: Vec<Event>,
    begins: HashMap<TxnToken, usize>,
}

#[derive(Default)]
pub struct MemorySink {
    inner: Mutex<Inner>,
}

impl MemorySink {
    pub fn new() -> MemorySink {
        MemorySink::default()
    }

    pub fn events(&self) -> Vec<Event> {
        self.lock().events.clone()
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl EventSink for MemorySink {
    fn write(&self, mut e: Event) {
        e.status = if e.txn.is_some() {
            TxStatus::Unfinished
        } else {
            TxStatus::None
        };
        self.lock().events.push(e);
    }

    fn begin_txn(&self, token: TxnToken, mut e: Event) {
        e.status = TxStatus::Unfinished;
        e.txn = Some(token);
        let mut g = self.lock();
        let idx = g.events.len();
        g.events.push(e);
        g.begins.insert(token, idx);
    }

    fn end_txn(&self, token: TxnToken, status: TxStatus, mut e: Event) {
        let mut g = self.lock();
        if let Some(idx) = g.begins.remove(&token)
            && let Some(b) = g.events.get_mut(idx)
        {
            b.status = status;
        }
        e.status = status;
        e.txn = Some(token);
        g.events.push(e);
    }
}

pub struct FileSink {
    writer: Mutex<Writer>,
    error: Mutex<Option<Error>>,
}

impl FileSink {
    pub fn new(writer: Writer) -> FileSink {
        FileSink {
            writer: Mutex::new(writer),
            error: Mutex::new(None),
        }
    }

    pub fn take_error(&self) -> Option<Error> {
        self.error.lock().unwrap_or_else(|p| p.into_inner()).take()
    }

    fn run(&self, f: impl FnOnce(&mut Writer) -> Result<(), Error>) {
        let r = f(&mut self.writer.lock().unwrap_or_else(|p| p.into_inner()));
        if let Err(e) = r {
            let mut slot = self.error.lock().unwrap_or_else(|p| p.into_inner());
            if slot.is_none() {
                *slot = Some(e);
            }
        }
    }
}

impl EventSink for FileSink {
    fn write(&self, e: Event) {
        self.run(|w| w.write(e));
    }
    fn begin_txn(&self, token: TxnToken, e: Event) {
        self.run(|w| w.begin_txn(token, e));
    }
    fn end_txn(&self, token: TxnToken, status: TxStatus, e: Event) {
        self.run(|w| w.end_txn(token, status, e));
    }
}
