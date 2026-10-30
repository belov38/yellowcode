use crate::run::{ErrorOut, Phase};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

pub(crate) const RELAY_STACK: usize = 64 * 1024;

#[derive(Default)]
pub(crate) struct Lifecycle {
    state: Mutex<State>,
}
#[derive(Default)]
struct State {
    closed: bool,
    active: Vec<Arc<AtomicBool>>,
}
fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value.lock().unwrap_or_else(|e| e.into_inner())
}
pub(crate) fn interrupted() -> ErrorOut {
    ErrorOut::new(Phase::Runtime, "Выполнение прервано")
}
impl Lifecycle {
    pub(crate) fn cancel(&self, close: bool) {
        let mut state = lock(&self.state);
        state.closed |= close;
        for token in &state.active {
            token.store(true, Ordering::Release);
        }
    }
    fn admit(&self) -> Result<Operation<'_>, ErrorOut> {
        let mut state = lock(&self.state);
        if state.closed {
            return Err(ErrorOut::new(Phase::Runtime, "query session is closed"));
        }
        if state.active.len() >= crate::MAX_RUNNING {
            return Err(ErrorOut::new(Phase::Runtime, crate::TOO_MANY_RUNNING));
        }
        let token = Arc::new(AtomicBool::new(false));
        state.active.push(Arc::clone(&token));
        Ok(Operation {
            lifecycle: self,
            token,
        })
    }
    pub(crate) fn run<T>(
        &self,
        external: &AtomicBool,
        budget: Option<&memory_budget::MemoryBudget>,
        strict: bool,
        work: impl FnOnce(&AtomicBool) -> T,
    ) -> Result<T, ErrorOut> {
        if strict && external.load(Ordering::Acquire) {
            return Err(interrupted());
        }
        let operation = self.admit()?;
        if external.load(Ordering::Acquire) {
            operation.token.store(true, Ordering::Release);
        }
        let _stack = budget
            .map(|b| b.try_reserve(memory_budget::Category::Metadata, RELAY_STACK as u64))
            .transpose()
            .map_err(|e| ErrorOut::new(Phase::Runtime, e.public_text()))?;
        std::thread::scope(|scope| {
            let (stop, wait) = std::sync::mpsc::channel::<()>();
            let token = &operation.token;
            let relay = std::thread::Builder::new()
                .name("query-cancel".into())
                .stack_size(RELAY_STACK)
                .spawn_scoped(scope, move || {
                    loop {
                        if external.load(Ordering::Acquire) {
                            token.store(true, Ordering::Release);
                            break;
                        }
                        if token.load(Ordering::Acquire) {
                            break;
                        }
                        match wait.recv_timeout(Duration::from_millis(2)) {
                            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => (),
                            _ => break,
                        }
                    }
                })
                .map_err(|e| {
                    ErrorOut::new(
                        Phase::Runtime,
                        format!("cannot start cancellation relay: {e}"),
                    )
                })?;
            let result = work(&operation.token);
            drop(stop);
            let _ = relay.join();
            if strict
                && (external.load(Ordering::Acquire) || operation.token.load(Ordering::Acquire))
            {
                return Err(interrupted());
            }
            Ok(result)
        })
    }
}
struct Operation<'a> {
    lifecycle: &'a Lifecycle,
    token: Arc<AtomicBool>,
}
impl Drop for Operation<'_> {
    fn drop(&mut self) {
        lock(&self.lifecycle.state)
            .active
            .retain(|t| !Arc::ptr_eq(t, &self.token));
    }
}
impl crate::Console {
    pub fn cancel_all(&self) {
        self.lifecycle.cancel(false);
    }
    pub fn close(&self) {
        self.lifecycle.cancel(true);
    }
    pub(crate) fn independent<T>(
        &self,
        cancel: &AtomicBool,
        work: impl FnOnce(&AtomicBool) -> T,
    ) -> Result<T, ErrorOut> {
        self.lifecycle
            .run(cancel, self.base.memory_budget(), true, work)
    }
    pub(crate) fn independent_complete<T>(
        &self,
        cancel: &AtomicBool,
        work: impl FnOnce(&AtomicBool) -> T,
    ) -> Result<T, ErrorOut> {
        self.lifecycle
            .run(cancel, self.base.memory_budget(), false, work)
    }
}
