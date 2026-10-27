use memory_budget::control::storage::{ControlReserve, Observation, SourceKinds};
use memory_budget::discovery::Visibility;
use memory_budget::policy::numeric::{self, Field, Policy, RawRefs, Startup};
use memory_budget::policy::{BudgetMode, ConfigError, SettingSource};
use memory_budget::{BudgetError, BudgetedWriter, Category, MemoryBudget, MemoryLimit, Snapshot};
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[derive(Default)]
#[cfg_attr(feature = "clap", derive(clap::Args))]
#[cfg_attr(feature = "clap", group(id = "MemorySettings"))]
pub struct Settings {
    /// Managed memory budget: auto or integer bytes with B/KiB/MiB/GiB suffix. Without a container
    /// limit the fixed fallback is 1GiB; UNF-class configurations need --memory-limit >= 1792MiB,
    /// ERP-class about 3GiB.
    #[cfg_attr(feature = "clap", arg(long, allow_hyphen_values = true))]
    memory_limit: Option<String>,
    /// Clean cache category cap; 0 disables caching.
    #[cfg_attr(feature = "clap", arg(long, allow_hyphen_values = true))]
    cache_limit: Option<String>,
    /// Execution, compilation, batch and temporary-table cap.
    #[cfg_attr(feature = "clap", arg(long, allow_hyphen_values = true))]
    query_memory_limit: Option<String>,
    /// Retained canonical result cap shared by all statements.
    #[cfg_attr(feature = "clap", arg(long, allow_hyphen_values = true))]
    result_memory_limit: Option<String>,
    /// Complete serialized output cap, including escaping and schema.
    #[cfg_attr(feature = "clap", arg(long, allow_hyphen_values = true))]
    response_limit: Option<String>,
    /// Only the budget B is a setting (`convert` DT to 1CD): the other variables are not read.
    #[cfg_attr(feature = "clap", arg(skip))]
    memory_only: bool,
}

pub struct Context {
    startup: Startup,
    budget: MemoryBudget,
    request: std::sync::OnceLock<MemoryBudget>,
    pub explicit: bool,
    sources: SourceKinds,
    soft_limit: Option<u64>,
    pressure: AtomicBool,
    response: AtomicU64,
}

const DEPLOYMENT: &str = "YELLOWCODE_CONTAINER_MEMORY_BYTES";

fn environment(name: &str, selected: bool) -> Result<Option<String>, ConfigError> {
    if !selected {
        return Ok(None);
    }
    match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(ConfigError(format!(
            "{name} must contain an ASCII memory size"
        ))),
    }
}

fn setting_name(field: Field) -> &'static str {
    match field {
        Field::Deployment => DEPLOYMENT,
        Field::Memory => "--memory-limit/YELLOWCODE_MEMORY_LIMIT",
        Field::Cache => "--cache-limit/YELLOWCODE_CACHE_LIMIT",
        Field::Query => "--query-memory-limit/YELLOWCODE_QUERY_MEMORY_LIMIT",
        Field::Result => "--result-memory-limit/YELLOWCODE_RESULT_MEMORY_LIMIT",
        Field::Response => "--response-limit/YELLOWCODE_RESPONSE_LIMIT",
    }
}
fn config_error(error: numeric::Error) -> ConfigError {
    match error {
        numeric::Error::Size { field, .. } | numeric::Error::Component { field, .. } => {
            ConfigError(format!("{}: {error}", setting_name(field)))
        }
        _ => ConfigError(error.to_string()),
    }
}

pub struct Raw {
    cli: [Option<String>; 5],
    env: [Option<String>; 5],
    deployment: Option<String>,
}
impl Raw {
    fn refs(values: &[Option<String>; 5]) -> RawRefs<'_> {
        RawRefs {
            memory_limit: values[0].as_deref(),
            cache_limit: values[1].as_deref(),
            query_memory_limit: values[2].as_deref(),
            result_memory_limit: values[3].as_deref(),
            response_limit: values[4].as_deref(),
        }
    }
    fn deployment(&self) -> Result<Option<u64>, ConfigError> {
        self.deployment
            .as_deref()
            .map(|text| {
                numeric::parse_bytes(text, false).map_err(|cause| {
                    config_error(numeric::Error::Size {
                        field: Field::Deployment,
                        cause,
                    })
                })
            })
            .transpose()
    }
}

impl Settings {
    pub fn memory_only(memory_limit: Option<String>) -> Settings {
        Settings {
            memory_limit,
            memory_only: true,
            ..Settings::default()
        }
    }
    fn raw(&self) -> Result<Raw, ConfigError> {
        let cli = [
            self.memory_limit.clone(),
            self.cache_limit.clone(),
            self.query_memory_limit.clone(),
            self.result_memory_limit.clone(),
            self.response_limit.clone(),
        ];
        let names = [
            "YELLOWCODE_MEMORY_LIMIT",
            "YELLOWCODE_CACHE_LIMIT",
            "YELLOWCODE_QUERY_MEMORY_LIMIT",
            "YELLOWCODE_RESULT_MEMORY_LIMIT",
            "YELLOWCODE_RESPONSE_LIMIT",
        ];
        let mut env = [None, None, None, None, None];
        for ((value, name), cli) in env.iter_mut().zip(names).zip(&cli) {
            let selected =
                cli.is_none() && (!self.memory_only || name == "YELLOWCODE_MEMORY_LIMIT");
            *value = environment(name, selected)?;
        }
        Ok(Raw {
            cli,
            env,
            deployment: environment(DEPLOYMENT, true)?,
        })
    }
    pub fn resolve(&self) -> crate::Result<(Context, ControlReserve)> {
        let raw = self.raw()?;
        let deployment = raw.deployment()?;
        let mut reserve = ControlReserve::try_new()?;
        let observation = super::control::observe(&mut reserve, deployment)
            .map_err(|_| ConfigError(format!("{DEPLOYMENT} must be positive")))?;
        Ok((Context::new(&raw, &observation)?, reserve))
    }
}

impl Context {
    pub fn new(raw: &Raw, observation: &Observation) -> crate::Result<Self> {
        let startup = numeric::resolve(
            Raw::refs(&raw.cli),
            Raw::refs(&raw.env),
            raw.deployment.as_deref(),
            observation.reduced,
            observation.visibility,
            observation.soft_limit,
        )
        .map_err(config_error)?;
        let policy = startup.policy();
        let budget = MemoryBudget::try_new(policy.target().limits)?;
        let sources = policy.sources;
        let explicit = [
            sources.memory,
            sources.cache,
            sources.query,
            sources.result,
            sources.response,
        ]
        .iter()
        .any(|s| *s != SettingSource::Default);
        Ok(Self {
            startup,
            budget,
            request: std::sync::OnceLock::new(),
            explicit,
            sources: observation.sources,
            soft_limit: observation.soft_limit,
            pressure: AtomicBool::new(policy.pressure),
            response: AtomicU64::new(policy.response),
        })
    }
    pub fn deployment(&self) -> Option<u64> {
        self.startup.deployment()
    }
    pub fn cache_bytes(&self) -> crate::Result<usize> {
        let startup = self.startup.policy().cache;
        let current = self
            .budget
            .snapshot()
            .categories
            .get(Category::Cache as usize)
            .map_or(startup, |c| c.limit);
        Ok(usize::try_from(startup.min(current))?)
    }
    pub fn ledger(&self) -> &MemoryBudget {
        self.request.get().unwrap_or(&self.budget)
    }
    pub fn snapshot(&self) -> Snapshot {
        self.budget.snapshot()
    }
    pub fn restore_budget(&self) -> (u64, &'static str, MemoryBudget) {
        let p = self.startup.policy();
        let mode = match p.mode {
            BudgetMode::Auto => "auto",
            BudgetMode::Explicit => "explicit",
            BudgetMode::Fallback => "fallback",
        };
        (p.budget, mode, self.budget.clone())
    }
    pub fn open(&self, path: &std::path::Path) -> crate::Result<query_console::Console> {
        let console = query_console::Console::open_with_memory(
            path,
            self.budget.clone(),
            self.cache_bytes()?,
        )?;
        if let Some(handle) = console
            .cache_evictor()
            .memory_budget()
            .filter(|handle| handle.same_instance(&self.budget))
        {
            self.request.get_or_init(|| handle.clone());
        }
        Ok(console)
    }
    pub fn response_writer(&self) -> Result<BudgetedWriter, Refusal> {
        BudgetedWriter::new(self.ledger(), self.response_limit()).map_err(Refusal::budget)
    }
    pub fn summary_wanted(&self) -> bool {
        let w = self.startup.policy().warnings;
        self.explicit || w.discovery_incomplete || w.fixed_fallback || w.soft_below_budget
    }
    #[cfg(any(test, feature = "testing"))]
    #[doc(hidden)]
    pub fn set_response_limit_for_test(&mut self, limit: u64) {
        self.response.store(limit, Ordering::Release);
    }
    pub fn response_limit(&self) -> u64 {
        self.response.load(Ordering::Acquire)
    }
    pub fn pressured(&self) -> bool {
        self.pressure.load(Ordering::Acquire)
    }
    pub fn apply(&self, observation: &Observation) -> Policy {
        let policy = self.startup.refresh(
            observation.reduced,
            observation.visibility,
            observation.soft_limit,
        );
        self.budget.set_limits(policy.target().limits);
        self.response.store(policy.response, Ordering::Release);
        self.pressure.store(policy.pressure, Ordering::Release);
        policy
    }
    pub fn admission(&self) -> Option<Refusal> {
        self.pressured().then_some(Refusal::PRESSURE)
    }
    pub fn summary(&self, err: &mut dyn Write) -> std::io::Result<()> {
        let p = self.startup.policy();
        let mode = match p.mode {
            BudgetMode::Auto => "auto",
            BudgetMode::Explicit => "explicit",
            BudgetMode::Fallback => "fallback",
        };
        let visibility = match p.visibility {
            Visibility::Visible => "visible",
            Visibility::Partial => "partial",
            Visibility::Unknown => "unknown",
        };
        write!(
            err,
            "yellowcode: memory mode={mode} B={} A={} E={} cache={} query={} result={} response={} row={} envelope=",
            p.budget, p.admission, p.control_reserve, p.cache, p.query, p.result, p.response, p.row
        )?;
        match p.envelope {
            Some(bytes) => write!(err, "{bytes}")?,
            None => err.write_all(b"none")?,
        }
        write!(err, " visibility={visibility} sources=")?;
        let kinds = [
            (self.sources.deployment, "deployment"),
            (self.sources.cgroup_v2, "cgroup_v2"),
            (self.sources.cgroup_v1, "cgroup_v1"),
            (self.sources.physical, "physical"),
        ];
        let mut any = false;
        for (_, name) in kinds.iter().filter(|(present, _)| *present) {
            if any {
                err.write_all(b",")?;
            }
            err.write_all(name.as_bytes())?;
            any = true;
        }
        if !any {
            err.write_all(b"none")?;
        }
        let source = |s: SettingSource| match s {
            SettingSource::Cli => "cli",
            SettingSource::Env => "env",
            SettingSource::Default => "default",
        };
        writeln!(
            err,
            " policy=70% settings=memory:{},cache:{},query:{},result:{},response:{}",
            source(p.sources.memory),
            source(p.sources.cache),
            source(p.sources.query),
            source(p.sources.result),
            source(p.sources.response)
        )?;
        if p.warnings.discovery_incomplete {
            writeln!(
                err,
                "yellowcode: memory warning: external envelope discovery is partial or unknown; set {DEPLOYMENT} or an explicit --memory-limit"
            )?;
        }
        if p.warnings.fixed_fallback {
            writeln!(
                err,
                "yellowcode: memory warning: no finite external envelope; fixed 1GiB fallback (UNF-class bases need --memory-limit >= 1792MiB, ERP-class about 3GiB)"
            )?;
        }
        if p.warnings.soft_below_budget
            && let Some(high) = self.soft_limit
        {
            writeln!(
                err,
                "yellowcode: memory warning: memory.high {high} B is below the budget; it is a soft throttle, not a hard limit"
            )?;
        }
        Ok(())
    }
}

pub fn is_config_error(error: &(dyn std::error::Error + 'static)) -> bool {
    error.downcast_ref::<ConfigError>().is_some()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Refusal {
    pub status: u16,
    pub text: &'static str,
}
impl Refusal {
    pub const PRESSURE: Self = Self {
        status: 503,
        text: "MEMORY_PRESSURE",
    };
    pub const ALLOCATION: Self = Self {
        status: 503,
        text: "MEMORY_ALLOCATION_FAILED",
    };
    pub const BUSY: Self = Self {
        status: 503,
        text: "query service is busy",
    };
    pub const INPUT: Self = Self {
        status: 422,
        text: "MEMORY_LIMIT: input",
    };
    pub fn limit(limit: MemoryLimit) -> Self {
        Self {
            status: 422,
            text: limit.public_text(),
        }
    }
    pub fn budget(error: BudgetError) -> Self {
        match error {
            BudgetError::AllocationFailed => Self::ALLOCATION,
            BudgetError::Limit(limit) => Self::limit(limit),
            BudgetError::Overflow => Self {
                status: 422,
                text: "MEMORY_LIMIT: response",
            },
        }
    }
    pub fn run(refusal: &query_console::RunRefusal) -> Self {
        match refusal {
            query_console::RunRefusal::Result(limit) => Self::limit(*limit),
            query_console::RunRefusal::Allocation => Self::ALLOCATION,
        }
    }
    pub fn reason(self) -> &'static str {
        reason(self.text)
    }
}

pub fn reason(text: &str) -> &'static str {
    match text {
        "MEMORY_PRESSURE" => "memory_pressure",
        "MEMORY_ALLOCATION_FAILED" => "allocation_failed",
        "MEMORY_LIMIT: global" => "memory_global",
        "MEMORY_LIMIT: row" => "memory_row",
        "MEMORY_LIMIT: result" => "memory_result",
        "MEMORY_LIMIT: input" => "memory_input",
        "MEMORY_LIMIT: response" => "memory_response",
        "MEMORY_LIMIT: execution" => "memory_execution",
        "MEMORY_LIMIT: cache" => "memory_cache",
        "query service is busy" => "busy",
        _ => "http_error",
    }
}

pub fn report_reason(report: &query_console::Report, timed_out: bool) -> &'static str {
    let first = report
        .error
        .as_ref()
        .map(|e| e.chain.first().map_or("", String::as_str));
    outcome_reason(first, report.cancelled, timed_out)
}
fn outcome_reason(error: Option<&str>, cancelled: bool, timed_out: bool) -> &'static str {
    if timed_out {
        return "timeout";
    }
    if cancelled {
        return "cancelled";
    }
    match error.map(reason) {
        None => "ok",
        Some("http_error") => "query_error",
        Some(memory) => memory,
    }
}

pub struct ErrorBody {
    bytes: [u8; 512],
    len: usize,
}
impl Write for ErrorBody {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let end = self
            .len
            .checked_add(bytes.len())
            .filter(|n| *n <= self.bytes.len())
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::FileTooLarge))?;
        self.bytes
            .get_mut(self.len..end)
            .ok_or(std::io::ErrorKind::InvalidInput)?
            .copy_from_slice(bytes);
        self.len = end;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl ErrorBody {
    pub fn new(message: &str) -> Self {
        #[derive(serde::Serialize)]
        struct Envelope<'a> {
            error: &'a str,
        }
        let mut body = Self::empty();
        let message = &message[..message.floor_char_boundary(message.len().min(160))];
        if serde_json::to_writer(&mut body, &Envelope { error: message }).is_err() {
            body.len = 0;
            let _ = body.write_all(b"{\"error\":\"diagnostic too large\"}");
        }
        body
    }
    pub fn empty() -> Self {
        Self {
            bytes: [0; 512],
            len: 0,
        }
    }
    pub fn as_bytes(&self) -> &[u8] {
        self.bytes.get(..self.len).unwrap_or(&[])
    }
}
