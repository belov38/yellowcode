use crate::report::{CodeError, Outcome, RunReport};
use crate::{lock, millis};
use appobj::{
    AppKind, AppOpen, AppOptions, Application, CallOptions, NoHooks, SessionError, SessionStart,
    SessionState, SessionUser,
};
use bsl_compiler::{CompileOptions, CompiledModule, ExecContext, FragmentScope, ModuleKind};
use bsl_syntax::{ParseMode, SourceText};
use bsl_vm::{
    CompatMode, Exception, ExecOptions, Execution, FatalKind, ModuleContext, Registry, Runtime,
    SessionEnv,
};
use serde::Serialize;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Instant;

pub const BUSY: &str = "консоль занята";
pub const INTERNAL: &str = "Внутренняя ошибка реплики";

const NOT_STARTED: &str = "сеанс не запущен";
const HOST_MODULE: &str = "КонсольКода";
const FRAGMENT_MODULE: &str = "";
const COMPUTER: &str = "yellowcode";
const EVAL_STACK: usize = 256 << 20;
const BUDGET: u64 = 1_000_000_000;
const MEMORY_LIMIT: u64 = 256 << 20;
const MAX_NATIVE_DEPTH: u32 = 512;
const FIRST_NUMBER: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SessionMode {
    Full,
    Bare { reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsoleInfo {
    pub session: SessionMode,
    pub rights_checked: bool,
}

enum Slot {
    Empty,
    Idle(Box<SessionState>),
    Busy,
    Starting,
}

struct Session {
    slot: Slot,
    mode: Option<SessionMode>,
    next_number: u32,
}

struct Worked {
    report: RunReport,
    state: Option<SessionState>,
}

impl Worked {
    fn internal(messages: Option<(Vec<bsl_vm::UserMessage>, usize)>) -> Worked {
        let report = RunReport::bare(Outcome::Internal, INTERNAL);
        let report = match messages {
            Some((kept, dropped)) => report.with_messages(kept, dropped),
            None => report,
        };
        Worked {
            report,
            state: None,
        }
    }
}

pub struct CodeConsole {
    host: Arc<CompiledModule>,
    runtime: Runtime,
    app: Arc<Application>,
    env: Arc<dyn SessionEnv>,
    session: Mutex<Session>,
    freed: Condvar,
    _ib: infobase::Infobase,
}

fn past_start<'a>(freed: &Condvar, mut s: MutexGuard<'a, Session>) -> MutexGuard<'a, Session> {
    while matches!(s.slot, Slot::Starting) {
        s = freed.wait(s).unwrap_or_else(|p| p.into_inner());
    }
    s
}

struct Release<'a> {
    console: &'a CodeConsole,
    state: Option<SessionState>,
}

impl Drop for Release<'_> {
    fn drop(&mut self) {
        let mut s = lock(&self.console.session);
        s.slot = match self.state.take() {
            Some(state) => Slot::Idle(Box::new(state)),
            None => Slot::Empty,
        };
        drop(s);
        self.console.freed.notify_all();
    }
}

impl CodeConsole {
    pub fn open(path: &Path) -> Result<CodeConsole, String> {
        CodeConsole::open_guarded(path, |_| ())
    }

    #[cfg(feature = "testing")]
    pub fn open_with(
        path: &Path,
        extend: impl FnOnce(&mut Registry),
    ) -> Result<CodeConsole, String> {
        CodeConsole::open_guarded(path, extend)
    }

    fn open_guarded(
        path: &Path,
        extend: impl FnOnce(&mut Registry),
    ) -> Result<CodeConsole, String> {
        catch_unwind(AssertUnwindSafe(|| CodeConsole::open_inner(path, extend)))
            .unwrap_or_else(|_| Err(format!("{INTERNAL}: {}", path.display())))
    }

    fn open_inner(path: &Path, extend: impl FnOnce(&mut Registry)) -> Result<CodeConsole, String> {
        let ib = infobase::Infobase::open(
            path,
            infobase::OpenOptions {
                locking: onecd::Locking::None,
                schema_check: infobase::SchemaCheck::Warn,
                ..infobase::OpenOptions::mode(infobase::OpenMode::ReadOnly)
            },
        )
        .map_err(|e| format!("{}: {e}", path.display()))?;
        let config = ib
            .configuration()
            .ok_or_else(|| format!("{}: в базе нет конфигурации", path.display()))?;
        let catalog = platform_catalog::catalog();
        let app = Application::open(AppOpen {
            store: ib.store(),
            config,
            catalog,
            event_sink: Arc::new(evlog::NullSink),
            options: AppOptions {
                file_system: None,
                ..AppOptions::default()
            },
        })
        .map_err(|e| e.to_string())?;
        let mut reg = Registry::new(catalog);
        bsl_stdlib::register(&mut reg);
        app.register(&mut reg).map_err(|e| e.to_string())?;
        extend(&mut reg);
        let runtime = Runtime::new(catalog, reg, app.host());
        let o = CompileOptions {
            module_name: HOST_MODULE,
            kind: ModuleKind::Object,
            context: ExecContext::Server,
            compat: CompatMode::DontUse,
            catalog,
            app: runtime.app().names(),
            extension: None,
        };
        let host = bsl_compiler::compile(&SourceText::from_str(HOST_MODULE, ""), &o)
            .map_err(|e| e.cause_text(HOST_MODULE, bsl_vm::Lang::Ru))?;
        let env =
            bsl_testhost::StandEnv::bench_default().with_config_language(&app.model().lang_code);
        Ok(CodeConsole {
            host,
            runtime,
            app,
            env: Arc::new(env),
            session: Mutex::new(Session {
                slot: Slot::Empty,
                mode: None,
                next_number: FIRST_NUMBER,
            }),
            freed: Condvar::new(),
            _ib: ib,
        })
    }

    pub fn info(&self) -> ConsoleInfo {
        let never_started = {
            let s = lock(&self.session);
            matches!(s.slot, Slot::Empty) && s.next_number == FIRST_NUMBER
        };
        if never_started {
            let _ = self.exclusive(None, &Arc::new(AtomicBool::new(false)));
        }
        let mut s = lock(&self.session);
        while s.mode.is_none() && matches!(s.slot, Slot::Busy | Slot::Starting) {
            s = self.freed.wait(s).unwrap_or_else(|p| p.into_inner());
        }
        let session = s.mode.clone().unwrap_or_else(|| SessionMode::Bare {
            reason: NOT_STARTED.to_string(),
        });
        ConsoleInfo {
            session,
            rights_checked: false,
        }
    }

    pub fn is_busy(&self) -> bool {
        matches!(lock(&self.session).slot, Slot::Busy | Slot::Starting)
    }

    pub fn run(&self, text: &str, cancel: Arc<AtomicBool>) -> RunReport {
        let t0 = Instant::now();
        let mut report = self
            .exclusive(Some(text), &cancel)
            .unwrap_or_else(|| RunReport::bare(Outcome::Busy, BUSY));
        report.elapsed_ms = millis(t0);
        report
    }

    fn exclusive(&self, text: Option<&str>, cancel: &Arc<AtomicBool>) -> Option<RunReport> {
        let (state, number) = {
            let mut s = past_start(&self.freed, lock(&self.session));
            let state = match &s.slot {
                Slot::Busy | Slot::Starting => return None,
                Slot::Idle(_) if text.is_none() => return None,
                Slot::Idle(_) | Slot::Empty => {
                    let taken = if text.is_some() {
                        Slot::Busy
                    } else {
                        Slot::Starting
                    };
                    match std::mem::replace(&mut s.slot, taken) {
                        Slot::Idle(state) => Some(*state),
                        _ => None,
                    }
                }
            };
            let number = s.next_number;
            if state.is_none() {
                s.next_number = s.next_number.saturating_add(1);
            }
            (state, number)
        };
        let mut release = Release {
            console: self,
            state: None,
        };
        let worked = std::thread::scope(|s| {
            let spawned = std::thread::Builder::new()
                .name("x4-code".into())
                .stack_size(EVAL_STACK)
                .spawn_scoped(s, || self.guarded(state, number, text, cancel));
            match spawned {
                Ok(h) => h.join().unwrap_or_else(|_| Worked::internal(None)),
                Err(_) => Worked::internal(None),
            }
        });
        release.state = worked.state;
        drop(release);
        Some(worked.report)
    }

    fn guarded(
        &self,
        state: Option<SessionState>,
        number: u32,
        text: Option<&str>,
        cancel: &Arc<AtomicBool>,
    ) -> Worked {
        let mut execution: Option<Execution> = None;
        let result = catch_unwind(AssertUnwindSafe(|| {
            self.work(&mut execution, state, number, text, cancel)
        }));
        match result {
            Ok(mut worked) => {
                if catch_unwind(AssertUnwindSafe(|| drop(execution.take()))).is_err() {
                    worked.report.outcome = Outcome::Internal;
                    worked.report.error = Some(CodeError::text(INTERNAL));
                    worked.state = None;
                }
                worked
            }
            Err(_) => {
                let messages = catch_unwind(AssertUnwindSafe(|| {
                    execution.as_mut().map(Execution::take_messages)
                }))
                .ok()
                .flatten();
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    if let Some(ex) = execution.as_mut()
                        && ex.host_data_ref().is_some()
                    {
                        drop(self.app.end_call(ex));
                    }
                }));
                let _ = catch_unwind(AssertUnwindSafe(|| drop(execution.take())));
                Worked::internal(messages)
            }
        }
    }

    fn work(
        &self,
        execution: &mut Option<Execution>,
        state: Option<SessionState>,
        number: u32,
        text: Option<&str>,
        cancel: &Arc<AtomicBool>,
    ) -> Worked {
        let opts = ExecOptions {
            budget: Some(BUDGET),
            memory_limit: Some(MEMORY_LIMIT),
            max_native_depth: MAX_NATIVE_DEPTH,
            cancel: Some(Arc::clone(cancel)),
            ..ExecOptions::default()
        };
        let state = match state {
            Some(state) => state,
            None => {
                let ex = execution.insert(self.runtime.execution(self.env.clone(), opts.clone()));
                match self
                    .app
                    .start_session(ex, self.session_start(number), &mut NoHooks)
                {
                    Ok(state) => {
                        self.publish(SessionMode::Full);
                        state
                    }
                    Err(SessionError::SessionModule(Exception::Fatal(f)))
                        if f.kind == FatalKind::Cancelled =>
                    {
                        let (kept, dropped) = ex.take_messages();
                        let report = RunReport {
                            error: Some(CodeError::of_fatal(&f, FRAGMENT_MODULE)),
                            ..RunReport::empty(Outcome::Cancelled)
                        };
                        return Worked {
                            report: report.with_messages(kept, dropped),
                            state: None,
                        };
                    }
                    Err(e) => {
                        self.publish(SessionMode::Bare {
                            reason: e.to_string(),
                        });
                        self.app.bare_session(self.session_start(number))
                    }
                }
            }
        };
        let Some(text) = text else {
            return Worked {
                report: RunReport::empty(Outcome::Done),
                state: Some(state),
            };
        };
        let ex = execution.insert(self.runtime.execution(self.env.clone(), opts));
        self.app.begin_call(ex, state, CallOptions::default());
        let result = self.execute(ex, text);
        let (kept, dropped) = ex.take_messages();
        let (state, _) = self.app.end_call(ex);
        Worked {
            report: self.report(result, text).with_messages(kept, dropped),
            state: Some(state),
        }
    }

    fn publish(&self, mode: SessionMode) {
        lock(&self.session).mode = Some(mode);
        self.freed.notify_all();
    }

    fn execute(&self, ex: &mut Execution, text: &str) -> Result<(), Exception> {
        let inst = ex.instantiate(&self.host, ModuleContext::None)?;
        ex.execute_text(&inst, text)
    }

    fn report(&self, result: Result<(), Exception>, text: &str) -> RunReport {
        let (outcome, error) = match result {
            Ok(()) => (Outcome::Done, None),
            Err(Exception::Catchable(info)) => {
                let mut e = CodeError::of_info(&info, FRAGMENT_MODULE);
                if e.at.is_none() {
                    e.at = self.compile_error_line(text);
                }
                (Outcome::Error, Some(e))
            }
            Err(Exception::Fatal(f)) => {
                let outcome = match f.kind {
                    FatalKind::Cancelled => Outcome::Cancelled,
                    FatalKind::Budget => Outcome::Budget,
                    _ => Outcome::Error,
                };
                (outcome, Some(CodeError::of_fatal(&f, FRAGMENT_MODULE)))
            }
        };
        RunReport {
            error,
            ..RunReport::empty(outcome)
        }
    }

    fn compile_error_line(&self, text: &str) -> Option<u32> {
        let o = CompileOptions {
            module_name: &self.host.name,
            kind: self.host.kind,
            context: self.host.context,
            compat: self.host.compat,
            catalog: platform_catalog::catalog(),
            app: self.runtime.app().names(),
            extension: None,
        };
        let scope = FragmentScope {
            caller_slots: &[],
            module: Some(&self.host),
        };
        bsl_compiler::compile_fragment(
            &SourceText::from_str("", text),
            &scope,
            ParseMode::Fragment,
            &o,
        )
        .err()
        .map(|e| e.line)
    }

    fn session_start(&self, number: u32) -> SessionStart {
        SessionStart {
            user: SessionUser {
                uuid: Default::default(),
                name: String::new(),
                full_name: String::new(),
                roles: Vec::new(),
                no_users: true,
            },
            number,
            app: AppKind::HttpService,
            computer: COMPUTER.to_string(),
            started_at: self.env.now_utc(),
        }
    }
}
