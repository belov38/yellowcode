use crate::users::{Credentials, UserDirectory};
use appobj::{
    AppKind, AppOpen, Application, CallOptions, Right, RightTarget, SessionError, SessionHooks,
    SessionStart, SessionState, SessionUser,
};
use bsl_vm::{ExecOptions, Execution, Registry, Runtime, SessionEnv};
use rand::{TryRngCore, rngs::OsRng};
use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

pub const NATIVE_VERSION: u16 = 1;

#[derive(Clone, Debug)]
pub struct NativeContext {
    pub version: u16,
    pub process_id: [u8; 16],
    pub epoch: [u8; 16],
    pub correlation_id: [u8; 16],
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionHandle([u8; 16]);
impl std::fmt::Debug for SessionHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionHandle(<redacted>)")
    }
}

#[derive(Clone, Copy)]
pub struct HttpMethod<'a> {
    pub service: appobj::model::Uuid16,
    pub template: &'a str,
    pub method: &'a str,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Reuse {
    Keep,
    Close,
}

pub struct ProcessConfig {
    pub env: Arc<dyn SessionEnv>,
    pub exec: ExecOptions,
    pub idle_timeout: Duration,
    pub computer: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ProcessError {
    #[error("cannot initialize the application runtime")]
    Open,
    #[error("invalid native request context")]
    Context,
    #[error("invalid credentials or session owner")]
    Unauthorized,
    #[error("method use is forbidden")]
    Forbidden,
    #[error("session is unavailable")]
    Gone,
    #[error("session is busy")]
    Busy,
    #[error("process is stopped")]
    Stopped,
    #[error("application call failed")]
    Call,
    #[error("instruction budget exhausted")]
    InstructionBudget,
    #[error("Внутренняя ошибка реплики")]
    Internal,
}
impl ProcessError {
    pub fn status(&self) -> u16 {
        match self {
            Self::Context => 400,
            Self::Unauthorized => 401,
            Self::Forbidden => 403,
            Self::Gone => 404,
            Self::Busy => 409,
            Self::Stopped => 503,
            Self::Open | Self::Call | Self::InstructionBudget | Self::Internal => 500,
        }
    }
}
impl From<bsl_vm::Exception> for ProcessError {
    fn from(error: bsl_vm::Exception) -> Self {
        match error {
            bsl_vm::Exception::Fatal(fatal) if fatal.kind == bsl_vm::FatalKind::Budget => {
                Self::InstructionBudget
            }
            _ => Self::Call,
        }
    }
}

impl From<appobj::SessionError> for ProcessError {
    fn from(error: appobj::SessionError) -> Self {
        match error {
            appobj::SessionError::GlobalModule(error)
            | appobj::SessionError::SessionModule(error) => error.into(),
            appobj::SessionError::Hook(_) => Self::Call,
        }
    }
}

struct Slot {
    user: SessionUser,
    number: u32,
    last_used: Instant,
    state: Option<SessionState>,
}
struct Sessions {
    accepting: bool,
    next_number: u32,
    slots: HashMap<SessionHandle, Slot>,
}

pub struct RuntimeProcess {
    app: Arc<Application>,
    services: crate::service::ServiceTable,
    runtime: Runtime,
    directory: UserDirectory,
    config: ProcessConfig,
    process_id: [u8; 16],
    epoch: [u8; 16],
    sessions: Mutex<Sessions>,
}

fn opaque_id() -> Result<[u8; 16], ProcessError> {
    let mut bytes = [0; 16];
    OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(|_| ProcessError::Internal)?;
    if bytes == [0; 16] {
        return Err(ProcessError::Internal);
    }
    Ok(bytes)
}

impl RuntimeProcess {
    pub fn open(
        open: AppOpen,
        directory: UserDirectory,
        config: ProcessConfig,
    ) -> Result<Self, ProcessError> {
        catch_unwind(AssertUnwindSafe(|| {
            let catalog = open.catalog;
            let services = crate::service::ServiceTable::from_config(&open.config)?;
            let app = Application::open(open).map_err(|_| ProcessError::Open)?;
            let mut registry = Registry::new(catalog);
            bsl_stdlib::register(&mut registry);
            app.register(&mut registry)
                .map_err(|_| ProcessError::Open)?;
            crate::service::objects::register(&mut registry);
            let runtime = Runtime::new(catalog, registry, app.host());
            Ok(Self {
                app,
                services,
                runtime,
                directory,
                config,
                process_id: opaque_id()?,
                epoch: opaque_id()?,
                sessions: Mutex::new(Sessions {
                    accepting: true,
                    next_number: 5,
                    slots: HashMap::new(),
                }),
            })
        }))
        .unwrap_or(Err(ProcessError::Internal))
    }

    pub fn context(&self, correlation_id: [u8; 16]) -> NativeContext {
        NativeContext {
            version: NATIVE_VERSION,
            process_id: self.process_id,
            epoch: self.epoch,
            correlation_id,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Sessions> {
        self.sessions.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn admit(
        &self,
        context: &NativeContext,
        credentials: Option<Credentials<'_>>,
    ) -> Result<SessionUser, ProcessError> {
        if context.version != NATIVE_VERSION
            || context.process_id != self.process_id
            || context.epoch != self.epoch
            || context.correlation_id == [0; 16]
        {
            return Err(ProcessError::Context);
        }
        if !self.lock().accepting {
            return Err(ProcessError::Stopped);
        }
        self.directory
            .authenticate(credentials)
            .map_err(|_| ProcessError::Unauthorized)
    }

    pub fn open_session(
        &self,
        context: &NativeContext,
        credentials: Option<Credentials<'_>>,
    ) -> Result<SessionHandle, ProcessError> {
        let user = self.admit(context, credentials)?;
        let number = {
            let mut sessions = self.lock();
            if !sessions.accepting {
                return Err(ProcessError::Stopped);
            }
            let number = sessions.next_number;
            sessions.next_number = number.checked_add(1).ok_or(ProcessError::Internal)?;
            number
        };
        let handle = SessionHandle(opaque_id()?);
        let mut execution = None;
        let started = catch_unwind(AssertUnwindSafe(|| {
            self.initialize_session(&mut execution, user.clone(), number)
        }));
        let cleanup = self.cleanup(&mut execution);
        let disposed = catch_unwind(AssertUnwindSafe(|| drop(execution)));
        let state = match (started, cleanup, disposed) {
            (Ok(Ok(state)), Ok(None), Ok(()))
                if state.number() == number && state.user() == &user =>
            {
                state
            }
            (Ok(Err(error)), Ok(None), Ok(())) => return Err(error),
            _ => return Err(ProcessError::Internal),
        };
        let mut sessions = self.lock();
        if !sessions.accepting {
            return Err(ProcessError::Stopped);
        }
        if sessions.slots.contains_key(&handle) {
            return Err(ProcessError::Internal);
        }
        sessions.slots.insert(
            handle,
            Slot {
                user,
                number,
                last_used: Instant::now(),
                state: Some(state),
            },
        );
        Ok(handle)
    }

    fn initialize_session(
        &self,
        execution: &mut Option<Execution>,
        user: SessionUser,
        number: u32,
    ) -> Result<SessionState, ProcessError> {
        let mut init_options = self.config.exec.clone();
        init_options.safe_mode_disabled = false;
        *execution = Some(
            self.runtime
                .execution(self.config.env.clone(), init_options),
        );
        let ex = execution.as_mut().ok_or(ProcessError::Internal)?;
        let start = SessionStart {
            user,
            number,
            app: AppKind::HttpService,
            computer: self.config.computer.clone(),
            started_at: self.config.env.now_utc(),
        };
        self.app
            .start_session(ex, start, &mut SessionEvents { process: self })
            .map_err(Into::into)
    }

    pub fn call<T: Send>(
        &self,
        context: &NativeContext,
        handle: SessionHandle,
        credentials: Option<Credentials<'_>>,
        method: HttpMethod<'_>,
        reuse: Reuse,
        run: impl FnOnce(&Application, &mut Execution) -> Result<T, ProcessError>,
    ) -> Result<T, ProcessError> {
        let user = self.admit(context, credentials)?;
        let (state, number) = {
            let mut sessions = self.lock();
            if !sessions.accepting {
                return Err(ProcessError::Stopped);
            }
            let slot = sessions.slots.get_mut(&handle).ok_or(ProcessError::Gone)?;
            if slot.user != user {
                return Err(ProcessError::Unauthorized);
            }
            if slot.state.is_some() && slot.last_used.elapsed() >= self.config.idle_timeout {
                let removed = sessions.slots.remove(&handle);
                drop(sessions);
                drop(removed);
                return Err(ProcessError::Gone);
            }
            let use_right =
                md_model::parts::Right::from_name("Use").ok_or(ProcessError::Internal)?;
            if !self.app.has_right(
                &user,
                RightTarget::HttpMethod {
                    service: method.service,
                    template: method.template,
                    method: method.method,
                },
                Right(use_right),
            ) {
                return Err(ProcessError::Forbidden);
            }
            (slot.state.take().ok_or(ProcessError::Busy)?, slot.number)
        };
        let mut execution = None;
        let result = catch_unwind(AssertUnwindSafe(|| {
            execution = Some(
                self.runtime
                    .execution(self.config.env.clone(), self.config.exec.clone()),
            );
            let ex = execution.as_mut().ok_or(ProcessError::Internal)?;
            self.app.begin_call(
                ex,
                state,
                CallOptions {
                    safe_mode_disabled: self.config.exec.safe_mode_disabled,
                    job: None,
                },
            );
            run(&self.app, ex)
        }));
        let cleanup = self.cleanup(&mut execution);
        let disposed = catch_unwind(AssertUnwindSafe(|| drop(execution)));
        let (result, state) = match (result, cleanup, disposed) {
            (Ok(result), Ok(Some(state)), Ok(()))
                if state.number() == number && state.user() == &user =>
            {
                (result, Some(state))
            }
            _ => (Err(ProcessError::Internal), None),
        };
        let completion = self.complete(handle, &user, number, state, reuse);
        if result.as_ref().err() == Some(&ProcessError::Internal) {
            return result;
        }
        completion?;
        result
    }

    pub fn service_call_fresh(
        &self,
        context: &NativeContext,
        credentials: Option<Credentials<'_>>,
        request: &crate::service::RequestData,
    ) -> Result<crate::service::ResponseData, crate::service::ServiceError> {
        self.services.service(&request.path)?;
        let handle = self.open_session(context, credentials)?;
        let result = self.service_call(context, handle, credentials, Reuse::Keep, request);
        let closed = self.close_session(context, handle, credentials);
        match result {
            Ok(response) => {
                closed?;
                Ok(response)
            }
            Err(error) => Err(error),
        }
    }

    pub fn service_call(
        &self,
        context: &NativeContext,
        handle: SessionHandle,
        credentials: Option<Credentials<'_>>,
        reuse: Reuse,
        request: &crate::service::RequestData,
    ) -> Result<crate::service::ResponseData, crate::service::ServiceError> {
        self.services.service(&request.path)?;
        self.admit(context, credentials)?;
        let route = self.services.route(&request.method, &request.path)?;
        self.call(
            context,
            handle,
            credentials,
            HttpMethod {
                service: route.service.id,
                template: &route.template.name,
                method: &route.method.name,
            },
            reuse,
            |app, ex| {
                let input = crate::service::objects::request(
                    ex,
                    request,
                    &request.base_url,
                    &route.relative_url,
                    &route.params,
                )?;
                let instance =
                    app.module_instance(ex, appobj::ModuleTarget::HttpService(route.service.id))?;
                let result = ex.call(
                    &instance,
                    &route.method.handler,
                    &mut [bsl_vm::Arg::Val(input)],
                )?;
                crate::service::objects::response(ex, &result)
            },
        )
        .map_err(Into::into)
    }

    fn cleanup(
        &self,
        execution: &mut Option<Execution>,
    ) -> Result<Option<SessionState>, ProcessError> {
        catch_unwind(AssertUnwindSafe(|| {
            let ex = execution.as_mut()?;
            ex.host_data_ref()?;
            Some(self.app.end_call(ex).0)
        }))
        .map_err(|_| ProcessError::Internal)
    }

    fn complete(
        &self,
        handle: SessionHandle,
        user: &SessionUser,
        number: u32,
        state: Option<SessionState>,
        reuse: Reuse,
    ) -> Result<(), ProcessError> {
        let mut sessions = self.lock();
        if !sessions.accepting {
            return Err(ProcessError::Stopped);
        }
        let slot = sessions.slots.get_mut(&handle).ok_or(ProcessError::Gone)?;
        if slot.number != number || &slot.user != user || slot.state.is_some() {
            return Err(ProcessError::Internal);
        }
        if reuse == Reuse::Keep && state.is_some() {
            slot.state = state;
            slot.last_used = Instant::now();
        } else {
            let removed = sessions.slots.remove(&handle);
            drop(sessions);
            drop(removed);
        }
        Ok(())
    }

    pub fn close_session(
        &self,
        context: &NativeContext,
        handle: SessionHandle,
        credentials: Option<Credentials<'_>>,
    ) -> Result<(), ProcessError> {
        let user = self.admit(context, credentials)?;
        let mut sessions = self.lock();
        let slot = sessions.slots.get(&handle).ok_or(ProcessError::Gone)?;
        if slot.user != user {
            return Err(ProcessError::Unauthorized);
        }
        let removed = sessions.slots.remove(&handle);
        drop(sessions);
        drop(removed);
        Ok(())
    }

    pub fn expire_idle(&self) -> usize {
        let mut sessions = self.lock();
        let expired: Vec<_> = sessions
            .slots
            .iter()
            .filter(|(_, s)| s.state.is_some() && s.last_used.elapsed() >= self.config.idle_timeout)
            .map(|(h, _)| *h)
            .collect();
        let removed: Vec<_> = expired
            .iter()
            .filter_map(|h| sessions.slots.remove(h))
            .collect();
        drop(sessions);
        drop(removed);
        expired.len()
    }

    pub fn stop(&self) {
        let mut sessions = self.lock();
        sessions.accepting = false;
        let removed = std::mem::take(&mut sessions.slots);
        drop(sessions);
        drop(removed);
    }
}

struct SessionEvents<'a> {
    process: &'a RuntimeProcess,
}
impl SessionHooks for SessionEvents<'_> {
    fn zones_set(&mut self, state: &SessionState) -> Result<(), SessionError> {
        if state.user().no_users {
            return Ok(());
        }
        for event in ["_$Session$_.Start", "_$Session$_.Authentication"] {
            self.process.app.event_sink().write(evlog::Event {
                time: self.process.config.env.now_utc(),
                level: evlog::Level::Information,
                status: evlog::TxStatus::None,
                txn: None,
                user: Some(evlog::UserRef {
                    uuid: state.user().uuid,
                    name: state.user().name.clone(),
                }),
                computer: state.computer().into(),
                app: "HTTPServiceConnection".into(),
                connection: state.number(),
                session: state.number(),
                event: event.into(),
                comment: String::new(),
                metadata: evlog::Meta::None,
                data: if event == "_$Session$_.Authentication" {
                    evlog::data::authentication(&state.user().name, "АутентификацияОС")
                } else {
                    evlog::data::undefined()
                },
                data_presentation: String::new(),
                server: self.process.config.computer.clone(),
                main_port: 0,
                aux_port: 0,
                separators: Vec::new(),
            });
        }
        Ok(())
    }
}
