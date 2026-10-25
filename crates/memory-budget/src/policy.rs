pub mod history;
pub mod numeric;
use crate::discovery::{Discovery, ExternalBound, Visibility};
use std::fmt;
pub const MIB: u64 = 1024 * 1024;
pub const GIB: u64 = 1024 * MIB;
pub const CONTROL_RESERVE: u64 = 4 * MIB;
#[derive(Clone, Debug, Default)]
pub struct RawSettings {
    pub memory_limit: Option<String>,
    pub cache_limit: Option<String>,
    pub query_memory_limit: Option<String>,
    pub result_memory_limit: Option<String>,
    pub response_limit: Option<String>,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SettingSource {
    Cli,
    Env,
    #[default]
    Default,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SettingSources {
    pub memory: SettingSource,
    pub cache: SettingSource,
    pub query: SettingSource,
    pub result: SettingSource,
    pub response: SettingSource,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BudgetMode {
    Auto,
    Explicit,
    Fallback,
}
#[derive(Clone, Debug)]
pub struct ResolvedPolicy {
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
    pub bounds: Vec<ExternalBound>,
    pub visibility: Visibility,
    pub warnings: Vec<String>,
    pub pressure: bool,
    pub membership: String,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigError(pub String);
impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for ConfigError {}
pub fn parse_bytes(text: &str, allow_zero: bool) -> Result<u64, ConfigError> {
    numeric::parse_bytes(text, allow_zero).map_err(|error| ConfigError(error.to_string()))
}
fn selected<'a>(
    cli: &'a Option<String>,
    env: &'a Option<String>,
) -> (Option<&'a str>, SettingSource) {
    if let Some(s) = cli {
        (Some(s), SettingSource::Cli)
    } else if let Some(s) = env {
        (Some(s), SettingSource::Env)
    } else {
        (None, SettingSource::Default)
    }
}
fn ceiling(c: u64) -> u64 {
    ((u128::from(c) * 7) / 10) as u64
}
fn default_cap(b: u64, percent: u64, max: u64) -> u64 {
    (((u128::from(b) * u128::from(percent)) / 100) as u64).min(max) / MIB * MIB
}
fn caps(b: u64) -> [u64; 5] {
    [
        default_cap(b, 20, 256 * MIB),
        default_cap(b, 40, GIB),
        default_cap(b, 15, 256 * MIB),
        default_cap(b, 10, 64 * MIB),
        default_cap(b, 5, 32 * MIB),
    ]
}
pub fn resolve(
    cli: &RawSettings,
    env: &RawSettings,
    deployment: Option<&str>,
    discovery: &Discovery,
) -> Result<ResolvedPolicy, ConfigError> {
    let mut bounds = discovery.bounds.clone();
    if let Some(s) = deployment {
        bounds.push(ExternalBound {
            source: "YELLOWCODE_CONTAINER_MEMORY_BYTES".into(),
            bytes: parse_bytes(s, false)?,
        })
    }
    let mut envelope = bounds.iter().map(|b| b.bytes).min();
    if let (Some(c), Some(ram)) = (envelope, discovery.physical_memory) {
        envelope = Some(c.min(ram));
        bounds.push(ExternalBound {
            source: "physical-memory sanity bound".into(),
            bytes: ram,
        });
    }
    let (raw, memory) = selected(&cli.memory_limit, &env.memory_limit);
    let explicit = raw.filter(|s| s.trim_matches(|c: char| c.is_ascii_whitespace()) != "auto");
    let (budget, mode) = if let Some(raw) = explicit {
        (parse_bytes(raw, false)?, BudgetMode::Explicit)
    } else if let Some(c) = envelope {
        (ceiling(c) / MIB * MIB, BudgetMode::Auto)
    } else {
        (GIB, BudgetMode::Fallback)
    };
    if budget < 256 * MIB {
        return Err(ConfigError(format!(
            "Memory budget {budget} B is below minimum {} B",
            256 * MIB
        )));
    }
    if let Some(c) = envelope {
        let safe = ceiling(c);
        if budget > safe {
            return Err(ConfigError(format!(
                "Memory budget {budget} B exceeds policy ceiling {safe} B"
            )));
        }
    }
    let admission = budget
        .checked_sub(CONTROL_RESERVE)
        .ok_or_else(|| ConfigError("Control reserve exceeds budget".into()))?;
    let defaults = caps(budget);
    let fields = [
        (&cli.cache_limit, &env.cache_limit),
        (&cli.query_memory_limit, &env.query_memory_limit),
        (&cli.result_memory_limit, &env.result_memory_limit),
        (&cli.response_limit, &env.response_limit),
    ];
    let mut values = [0; 4];
    let mut sources = [SettingSource::Default; 4];
    for (i, (cli, env)) in fields.into_iter().enumerate() {
        let (raw, source) = selected(cli, env);
        sources[i] = source;
        values[i] = match raw {
            Some(s) => parse_bytes(s, i == 0)?,
            None => defaults[i],
        };
        if values[i] > admission {
            return Err(ConfigError(format!(
                "Component cap {} B exceeds admission {admission} B",
                values[i]
            )));
        }
        if i == 3 && values[i] < 1024 {
            return Err(ConfigError("Response limit must be at least 1 KiB".into()));
        }
    }
    let mut warnings = discovery.warnings.clone();
    if discovery.visibility != Visibility::Visible {
        warnings.push("External envelope discovery is partial or unknown; configure an explicit deployment envelope".into())
    }
    if mode == BudgetMode::Fallback {
        warnings.push("No finite external envelope; fixed 1 GiB fallback".into())
    }
    if discovery.soft_limit.is_some_and(|high| high < budget) {
        warnings
            .push("Visible memory.high is below managed budget (soft pressure threshold)".into())
    }
    Ok(ResolvedPolicy {
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
        bounds,
        visibility: discovery.visibility,
        warnings,
        pressure: false,
        membership: discovery.membership.clone(),
    })
}
pub struct PolicyRefresh {
    startup: ResolvedPolicy,
    sticky: Vec<ExternalBound>,
    membership: String,
}
impl PolicyRefresh {
    pub fn new(startup: ResolvedPolicy) -> Self {
        Self {
            sticky: startup.bounds.clone(),
            membership: startup.membership.clone(),
            startup,
        }
    }
    pub fn refresh(&mut self, d: &Discovery) -> ResolvedPolicy {
        let moved = !d.membership.is_empty()
            && d.membership != self.membership
            && d.visibility == Visibility::Visible
            && d.membership_confirmed;
        if moved {
            self.sticky
                .retain(|b| b.source == "YELLOWCODE_CONTAINER_MEMORY_BYTES");
            self.membership = d.membership.clone();
        }
        if d.membership_confirmed {
            self.sticky
                .retain(|b| !d.confirmed_unlimited.contains(&b.source));
        }
        for b in &d.bounds {
            if let Some(old) = self.sticky.iter_mut().find(|old| old.source == b.source) {
                old.bytes = if d.membership_confirmed {
                    b.bytes
                } else {
                    old.bytes.min(b.bytes)
                };
            } else {
                self.sticky.push(b.clone());
            }
        }
        if let Some(ram) = d.physical_memory {
            if let Some(old) = self
                .sticky
                .iter_mut()
                .find(|b| b.source == "physical-memory sanity bound")
            {
                old.bytes = if d.membership_confirmed {
                    ram
                } else {
                    old.bytes.min(ram)
                };
            } else if !self.sticky.is_empty() {
                self.sticky.push(ExternalBound {
                    source: "physical-memory sanity bound".into(),
                    bytes: ram,
                });
            }
        }
        let mut c = self.sticky.iter().map(|b| b.bytes).min();
        if let (Some(bound), Some(ram)) = (c, d.physical_memory) {
            c = Some(bound.min(ram));
        }
        let mut p = self.startup.clone();
        p.envelope = c;
        p.bounds = self.sticky.clone();
        p.visibility = d.visibility;
        p.warnings = d.warnings.clone();
        let missing = self.sticky.iter().any(|b| {
            b.source != "YELLOWCODE_CONTAINER_MEMORY_BYTES"
                && if b.source == "physical-memory sanity bound" {
                    d.physical_memory.is_none()
                } else {
                    !d.bounds.iter().any(|new| new.source == b.source)
                }
        });
        if missing {
            p.visibility = Visibility::Partial;
            p.warnings.push(
                "Previously observed memory bounds could not be refreshed; retaining safe ceilings"
                    .into(),
            );
        }

        let target = numeric::legacy_target(&self.startup, c);
        p.budget = target.budget;
        p.admission = target.admission;
        p.pressure = target.pressure;
        p.cache = target.cache;
        p.query = target.query;
        p.result = target.result;
        p.response = target.response;
        p.row = target.row;
        p
    }
}
