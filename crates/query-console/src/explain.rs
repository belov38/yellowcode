use crate::Console;
use crate::run::{ErrorOut, Phase, RunRequest, later_text};
use query_testhost::{RunMode, explain_text};
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Explain {
    pub ir: String,
    pub plan: String,
    pub notes: Vec<String>,
}

impl Console {
    pub fn explain(&self, req: &RunRequest, cancel: &AtomicBool) -> Result<Explain, ErrorOut> {
        let started = Instant::now();
        let params = self.request_params(req).map_err(|(e, _)| e)?;
        let mode = RunMode {
            lang: req.lang.syn(),
            inter: req.mode == crate::Mode::Inter,
        };
        let run =
            explain_text(&self.base, &req.text, &params, mode, cancel).map_err(|f| {
                match f.later {
                    Some(w) => ErrorOut::new(Phase::Later, later_text(w)),
                    None => self
                        .failure(started, f, cancel)
                        .error
                        .unwrap_or_else(|| ErrorOut::new(Phase::Runtime, "internal")),
                }
            })?;
        Ok(Explain {
            ir: run.ir.join("\n"),
            plan: run.plan,
            notes: vec![NOTE_LAST_ONLY.to_string(), NOTE_IR_L.to_string()],
        })
    }

    pub fn explain_guarded(&self, req: &RunRequest) -> Result<Explain, ExplainFail> {
        self.explain_within(req, EXPLAIN_TIMEOUT)
    }

    pub fn explain_within(
        &self,
        req: &RunRequest,
        limit: Duration,
    ) -> Result<Explain, ExplainFail> {
        if self.explain_busy.swap(true, Ordering::AcqRel) {
            return Err(ExplainFail::Busy);
        }
        let _busy = BusyGuard(&self.explain_busy);
        let cancel = Arc::new(AtomicBool::new(false));
        let timed_out = Arc::new(AtomicBool::new(false));
        let (stop, wait) = channel::<()>();
        let (c, t) = (Arc::clone(&cancel), Arc::clone(&timed_out));
        let dog = std::thread::Builder::new()
            .name("x1-explain-watchdog".to_string())
            .spawn(move || {
                if wait.recv_timeout(limit) == Err(RecvTimeoutError::Timeout) {
                    t.store(true, Ordering::Release);
                    c.store(true, Ordering::Release);
                }
            });
        let out = self.explain(req, &cancel);
        drop(stop);
        if let Ok(h) = dog {
            let _ = h.join();
        }
        match out {
            Ok(x) => Ok(x),
            Err(e) if timed_out.load(Ordering::Acquire) => Err(ExplainFail::Query(ErrorOut::new(
                Phase::Runtime,
                format!(
                    "Выполнение прервано: построение плана дольше {} с ({})",
                    limit.as_secs().max(1),
                    e.chain.first().map_or("", String::as_str)
                ),
            ))),
            Err(e) => Err(ExplainFail::Query(e)),
        }
    }
}
