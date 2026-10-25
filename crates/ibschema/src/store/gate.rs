use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::{StoreError, StoreResult};

pub struct WriterGate {
    busy: Mutex<bool>,
    freed: Condvar,
}

pub struct WriterTicket {
    gate: Arc<WriterGate>,
}

impl WriterGate {
    #[allow(clippy::new_ret_no_self)]
    pub fn new() -> Arc<WriterGate> {
        Arc::new(WriterGate {
            busy: Mutex::new(false),
            freed: Condvar::new(),
        })
    }

    pub fn acquire(self: &Arc<Self>, wait: Duration) -> StoreResult<WriterTicket> {
        let deadline = Instant::now().checked_add(wait);
        let mut busy = self.busy.lock().unwrap_or_else(PoisonError::into_inner);
        while *busy {
            let left = match deadline {
                Some(d) => d.saturating_duration_since(Instant::now()),
                None => wait,
            };
            if left.is_zero() {
                return Err(StoreError::WriterBusy);
            }
            busy = self
                .freed
                .wait_timeout(busy, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        *busy = true;
        Ok(WriterTicket {
            gate: Arc::clone(self),
        })
    }
}

impl Drop for WriterTicket {
    fn drop(&mut self) {
        *self
            .gate
            .busy
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = false;
        self.gate.freed.notify_one();
    }
}
