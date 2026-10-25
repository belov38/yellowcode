use super::{
    BudgetMode, CONTROL_RESERVE, GIB, MIB, RawSettings, ResolvedPolicy, SettingSource,
    SettingSources, caps, ceiling, history::Reduced,
};
use crate::{Category, Limits, control::Target, discovery::Visibility};
use std::fmt;

#[derive(Clone, Copy, Debug, Default)]
pub struct RawRefs<'a> {
    pub memory_limit: Option<&'a str>,
    pub cache_limit: Option<&'a str>,
    pub query_memory_limit: Option<&'a str>,
    pub result_memory_limit: Option<&'a str>,
    pub response_limit: Option<&'a str>,
}
impl<'a> From<&'a RawSettings> for RawRefs<'a> {
    fn from(raw: &'a RawSettings) -> Self {
        Self {
            memory_limit: raw.memory_limit.as_deref(),
            cache_limit: raw.cache_limit.as_deref(),
            query_memory_limit: raw.query_memory_limit.as_deref(),
            result_memory_limit: raw.result_memory_limit.as_deref(),
            response_limit: raw.response_limit.as_deref(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SizeError {
    ExpectedInteger,
    InvalidSuffix,
    Overflow,
    MustBePositive,
}
impl fmt::Display for SizeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ExpectedInteger => "Expected unsigned integer bytes with B/KiB/MiB/GiB suffix",
            Self::InvalidSuffix => "Invalid memory size suffix",
            Self::Overflow => "Memory size overflow",
            Self::MustBePositive => "Memory size must be positive",
        })
    }
}
impl std::error::Error for SizeError {}

pub fn parse_bytes(text: &str, allow_zero: bool) -> Result<u64, SizeError> {
    let text = text.trim_matches(|c: char| c.is_ascii_whitespace());
    let end = text.bytes().take_while(u8::is_ascii_digit).count();
    if end == 0 {
        return Err(SizeError::ExpectedInteger);
    }
    let (digits, suffix) = text.split_at(end);
    let multiplier = match suffix {
        "" | "B" => 1,
        "KiB" => 1024,
        "MiB" => MIB,
        "GiB" => GIB,
        _ => return Err(SizeError::InvalidSuffix),
    };
    let integer = digits.parse::<u64>().map_err(|_| SizeError::Overflow)?;
    let value = integer.checked_mul(multiplier).ok_or(SizeError::Overflow)?;
    if value == 0 && !allow_zero {
        return Err(SizeError::MustBePositive);
    }
    Ok(value)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Field {
    Deployment,
    Memory,
    Cache,
    Query,
    Result,
    Response,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Size {
        field: Field,
        cause: SizeError,
    },
    BelowMinimum {
        budget: u64,
    },
    AboveCeiling {
        budget: u64,
        ceiling: u64,
    },
    ControlReserve,
    Component {
        field: Field,
        cap: u64,
        admission: u64,
    },
    ResponseMinimum,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Size { cause, .. } => cause.fmt(f),
            Self::BelowMinimum { budget } => write!(
                f,
                "Memory budget {budget} B is below minimum {} B",
                256 * MIB
            ),
            Self::AboveCeiling { budget, ceiling } => write!(
                f,
                "Memory budget {budget} B exceeds policy ceiling {ceiling} B"
            ),
            Self::ControlReserve => f.write_str("Control reserve exceeds budget"),
            Self::Component { cap, admission, .. } => {
                write!(f, "Component cap {cap} B exceeds admission {admission} B")
            }
            Self::ResponseMinimum => f.write_str("Response limit must be at least 1 KiB"),
        }
    }
}
impl std::error::Error for Error {}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Warnings {
    pub discovery_incomplete: bool,
    pub fixed_fallback: bool,
    pub soft_below_budget: bool,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Policy {
    pub budget: u64,
    pub admission: u64,
    pub control_reserve: u64,
    pub cache: u64,
    pub query: u64,
    pub result: u64,
    pub response: u64,
    pub row: u64,
    pub sources: SettingSources,
    pub mode: BudgetMode,
    pub envelope: Option<u64>,
    pub visibility: Visibility,
    pub pressure: bool,
    pub warnings: Warnings,
}
impl Policy {
    pub fn target(self) -> Target {
        Target {
            budget: self.budget,
            limits: Limits::new(self.admission)
                .with_category(Category::Cache, self.cache)
                .with_category(Category::Execution, self.query)
                .with_category(Category::Result, self.result)
                .with_category(Category::Row, self.row),
            row_limit: self.row,
            response_limit: self.response,
            visibility: self.visibility,
            pressure: self.pressure,
        }
    }
    fn diagnostics(&mut self, reduced: Reduced, visibility: Visibility, soft: Option<u64>) {
        self.visibility = if reduced.partial {
            Visibility::Partial
        } else {
            visibility
        };
        self.warnings = Warnings {
            discovery_incomplete: self.visibility != Visibility::Visible,
            fixed_fallback: self.mode == BudgetMode::Fallback,
            soft_below_budget: soft.is_some_and(|high| high < self.budget),
        };
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Startup {
    policy: Policy,
    deployment: Option<u64>,
}
impl Startup {
    pub fn policy(&self) -> Policy {
        self.policy
    }
    pub fn deployment(&self) -> Option<u64> {
        self.deployment
    }
    pub fn refresh(&self, reduced: Reduced, visibility: Visibility, soft: Option<u64>) -> Policy {
        let mut policy = runtime_target(self.policy, envelope(reduced, self.deployment));
        policy.diagnostics(reduced, visibility, soft);
        policy
    }
}
fn selected<'a>(cli: Option<&'a str>, env: Option<&'a str>) -> (Option<&'a str>, SettingSource) {
    if cli.is_some() {
        (cli, SettingSource::Cli)
    } else if env.is_some() {
        (env, SettingSource::Env)
    } else {
        (None, SettingSource::Default)
    }
}
fn size(text: &str, field: Field) -> Result<u64, Error> {
    parse_bytes(text, field == Field::Cache).map_err(|cause| Error::Size { field, cause })
}
fn envelope(reduced: Reduced, deployment: Option<u64>) -> Option<u64> {
    let external = match (reduced.external_ceiling, deployment) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    external.map(|bound| reduced.physical_ceiling.map_or(bound, |ram| bound.min(ram)))
}
pub fn resolve(
    cli: RawRefs<'_>,
    env: RawRefs<'_>,
    deployment: Option<&str>,
    reduced: Reduced,
    visibility: Visibility,
    soft: Option<u64>,
) -> Result<Startup, Error> {
    let deployment = deployment
        .map(|text| size(text, Field::Deployment))
        .transpose()?;
    let envelope = envelope(reduced, deployment);
    let (raw, memory) = selected(cli.memory_limit, env.memory_limit);
    let explicit = raw.filter(|s| s.trim_matches(|c: char| c.is_ascii_whitespace()) != "auto");
    let (budget, mode) = if let Some(raw) = explicit {
        (size(raw, Field::Memory)?, BudgetMode::Explicit)
    } else if let Some(c) = envelope {
        (ceiling(c) / MIB * MIB, BudgetMode::Auto)
    } else {
        (GIB, BudgetMode::Fallback)
    };
    if budget < 256 * MIB {
        return Err(Error::BelowMinimum { budget });
    }
    if let Some(c) = envelope {
        let ceiling = ceiling(c);
        if budget > ceiling {
            return Err(Error::AboveCeiling { budget, ceiling });
        }
    }
    let admission = budget
        .checked_sub(CONTROL_RESERVE)
        .ok_or(Error::ControlReserve)?;
    let defaults = caps(budget);
    let fields = [
        (cli.cache_limit, env.cache_limit, Field::Cache),
        (cli.query_memory_limit, env.query_memory_limit, Field::Query),
        (
            cli.result_memory_limit,
            env.result_memory_limit,
            Field::Result,
        ),
        (cli.response_limit, env.response_limit, Field::Response),
    ];
    let mut values = [0; 4];
    let mut sources = [SettingSource::Default; 4];
    for (i, (cli, env, field)) in fields.into_iter().enumerate() {
        let (raw, source) = selected(cli, env);
        sources[i] = source;
        values[i] = match raw {
            Some(raw) => size(raw, field)?,
            None => defaults[i],
        };
        if values[i] > admission {
            return Err(Error::Component {
                field,
                cap: values[i],
                admission,
            });
        }
        if field == Field::Response && values[i] < 1024 {
            return Err(Error::ResponseMinimum);
        }
    }
    let mut policy = Policy {
        budget,
        admission,
        control_reserve: CONTROL_RESERVE,
        cache: values[0],
        query: values[1],
        result: values[2],
        response: values[3],
        row: defaults[4],
        sources: SettingSources {
            memory,
            cache: sources[0],
            query: sources[1],
            result: sources[2],
            response: sources[3],
        },
        mode,
        envelope,
        visibility,
        pressure: false,
        warnings: Warnings::default(),
    };
    policy.diagnostics(reduced, visibility, soft);
    Ok(Startup { policy, deployment })
}

fn runtime_target(startup: Policy, envelope: Option<u64>) -> Policy {
    let mut p = startup;
    p.envelope = envelope;
    if let Some(c) = envelope {
        let safe = ceiling(c);
        let safe = if p.mode == BudgetMode::Explicit {
            safe
        } else {
            safe / MIB * MIB
        };
        p.budget = p.budget.min(safe);
    }
    p.admission = p.budget.saturating_sub(CONTROL_RESERVE);
    p.pressure = p.budget < 256 * MIB;
    let defaults = caps(p.budget);
    let cap = |source, value: u64, default| {
        if source == SettingSource::Default {
            default
        } else {
            value.min(p.admission)
        }
    };
    p.cache = cap(p.sources.cache, p.cache, defaults[0]);
    p.query = cap(p.sources.query, p.query, defaults[1]);
    p.result = cap(p.sources.result, p.result, defaults[2]);
    p.response = cap(p.sources.response, p.response, defaults[3]);
    p.row = defaults[4];
    p
}
pub(super) fn legacy_target(p: &ResolvedPolicy, envelope: Option<u64>) -> Policy {
    runtime_target(
        Policy {
            budget: p.budget,
            admission: p.admission,
            control_reserve: p.control_reserve,
            cache: p.cache,
            query: p.query,
            result: p.result,
            response: p.response,
            row: p.row,
            sources: p.sources,
            mode: p.mode,
            envelope: p.envelope,
            visibility: p.visibility,
            pressure: p.pressure,
            warnings: Warnings::default(),
        },
        envelope,
    )
}
