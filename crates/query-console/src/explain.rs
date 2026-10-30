use crate::Console;
use crate::run::{ErrorOut, Phase, RunRequest, later_text};
use query_testhost::{BatchMemory, RunMode, explain_text, explain_text_budgeted};
use serde::Serialize;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::time::{Duration, Instant};

pub const NOTE_LAST_ONLY: &str = "план только последнего оператора пакета (R-X-2)";
pub const NOTE_IR_L: &str = "IR-L: ждёт Q";

pub const EXPLAIN_TIMEOUT: Duration = Duration::from_secs(60);
pub const EXPLAIN_BUSY: &str = "план уже строится; дождитесь окончания";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExplainFail {
    Busy,
    Query(ErrorOut),
}

struct BusyGuard<'a>(&'a AtomicBool);

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[derive(Debug, Serialize)]
pub struct Explain {
    ir: String,
    plan: String,
    notes: Vec<String>,
    #[serde(skip)]
    _retained: Option<memory_budget::Reservation>,
}

impl Explain {
    pub fn ir(&self) -> &str {
        &self.ir
    }
    pub fn plan(&self) -> &str {
        &self.plan
    }
    pub fn notes(&self) -> &[String] {
        &self.notes
    }
}

impl Console {
    pub fn explain(&self, req: &RunRequest, cancel: &AtomicBool) -> Result<Explain, ErrorOut> {
        self.independent(cancel, |cancel| self.explain_active(req, cancel))?
    }

    fn explain_active(&self, req: &RunRequest, cancel: &AtomicBool) -> Result<Explain, ErrorOut> {
        let started = Instant::now();
        let memory = BatchMemory::new(&self.base)
            .transpose()
            .map_err(|error| ErrorOut::new(Phase::Runtime, error.public_text()))?;
        if let Some(memory) = &memory {
            let bytes = self.parameter_bytes(req, cancel)?;
            memory
                .params_stage(bytes, req.params.len())
                .map_err(|error| ErrorOut::new(Phase::Runtime, error.public_text()))?;
        }
        let params = self.request_params(req, cancel).map_err(|(e, _)| e)?;
        let mode = RunMode {
            lang: req.lang.syn(),
            inter: req.mode == crate::Mode::Inter,
        };
        #[cfg(test)]
        self.query_checkpoint("explain", cancel);
        if cancel.load(Ordering::Acquire) {
            return Err(crate::lifecycle::interrupted());
        }
        let run = match &memory {
            Some(memory) => {
                explain_text_budgeted(&self.base, &req.text, &params, mode, cancel, memory)
            }
            None => explain_text(&self.base, &req.text, &params, mode, cancel),
        }
        .map_err(|f| match f.later {
            Some(w) => ErrorOut::new(Phase::Later, later_text(w)),
            None => self
                .failure(started, f, cancel)
                .error
                .unwrap_or_else(|| ErrorOut::new(Phase::Runtime, "internal")),
        })?;
        if cancel.load(Ordering::Acquire) {
            return Err(crate::lifecycle::interrupted());
        }
        let retained = self
            .base
            .memory_budget()
            .map(|budget| {
                let bytes = run
                    .ir
                    .iter()
                    .fold(1024u64, |bytes, text| {
                        bytes
                            .saturating_add((text.capacity() as u64).saturating_mul(2))
                            .saturating_add(64)
                    })
                    .saturating_add((run.plan.capacity() as u64).saturating_mul(2));
                budget.try_reserve(memory_budget::Category::Result, bytes)
            })
            .transpose()
            .map_err(|error| ErrorOut::new(Phase::Runtime, error.public_text()))?;
        if cancel.load(Ordering::Acquire) {
            return Err(crate::lifecycle::interrupted());
        }
        Ok(Explain {
            _retained: retained,
            ir: run.ir.join("\n"),
            plan: run.plan,
            notes: vec![NOTE_LAST_ONLY.to_string(), NOTE_IR_L.to_string()],
        })
    }

    pub fn explain_guarded(&self, req: &RunRequest) -> Result<Explain, ExplainFail> {
        self.explain_guarded_with_cancel(req, &AtomicBool::new(false))
    }

    pub fn explain_guarded_with_cancel(
        &self,
        req: &RunRequest,
        cancel: &AtomicBool,
    ) -> Result<Explain, ExplainFail> {
        self.independent(cancel, |cancel| {
            self.explain_within_active(req, EXPLAIN_TIMEOUT, cancel)
        })
        .map_err(ExplainFail::Query)?
    }

    pub fn explain_within(
        &self,
        req: &RunRequest,
        limit: Duration,
    ) -> Result<Explain, ExplainFail> {
        self.independent(&AtomicBool::new(false), |cancel| {
            self.explain_within_active(req, limit, cancel)
        })
        .map_err(ExplainFail::Query)?
    }

    fn explain_within_active(
        &self,
        req: &RunRequest,
        limit: Duration,
        external: &AtomicBool,
    ) -> Result<Explain, ExplainFail> {
        if self.explain_busy.swap(true, Ordering::AcqRel) {
            return Err(ExplainFail::Busy);
        }
        let _busy = BusyGuard(&self.explain_busy);
        let cancel = external;
        let timed_out = Arc::new(AtomicBool::new(false));
        let (stop, wait) = channel::<()>();
        let t = Arc::clone(&timed_out);
        std::thread::scope(|scope| {
            let dog = std::thread::Builder::new()
                .name("x1-explain-watchdog".to_string())
                .stack_size(64 * 1024)
                .spawn_scoped(scope, move || {
                    let started = Instant::now();
                    loop {
                        if external.load(Ordering::Acquire) || started.elapsed() >= limit {
                            t.store(started.elapsed() >= limit, Ordering::Release);
                            external.store(true, Ordering::Release);
                            break;
                        }
                        if wait.recv_timeout(Duration::from_millis(2))
                            != Err(RecvTimeoutError::Timeout)
                        {
                            break;
                        }
                    }
                })
                .map_err(|e| {
                    ExplainFail::Query(ErrorOut::new(
                        Phase::Runtime,
                        format!("cannot start explain watchdog: {e}"),
                    ))
                })?;
            let out = self.explain_active(req, cancel);
            drop(stop);
            let _ = dog.join();
            match out {
                Ok(_) if cancel.load(Ordering::Acquire) => {
                    Err(ExplainFail::Query(crate::lifecycle::interrupted()))
                }
                Ok(x) => Ok(x),
                Err(_) if timed_out.load(Ordering::Acquire) => {
                    Err(ExplainFail::Query(ErrorOut::new(
                        Phase::Runtime,
                        format!(
                            "Выполнение прервано: построение плана дольше {} с",
                            limit.as_secs().max(1)
                        ),
                    )))
                }
                Err(e) => Err(ExplainFail::Query(e)),
            }
        })
    }
}
