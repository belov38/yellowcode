use super::*;
use mdconf::{EntryMeta, EntrySource, Inflated, Stages};

pub(super) struct Facts {
    pub cfg: Configuration,
    pub roles: Vec<RoleChoice>,
    pub admin: Vec<brace::Guid>,
    pub separated: bool,
}

const ENTRY_LIMIT: u64 = 8 * 1_048_576;
const RIGHTS_LIMIT: u64 = 65_536;
const TOTAL_LIMIT: u64 = 64 * 1_048_576;
fn entry_limit(name: &str) -> u64 {
    if name.ends_with(".0") {
        RIGHTS_LIMIT
    } else {
        ENTRY_LIMIT
    }
}
fn refusal() -> memory_budget::MemoryLimit {
    memory_budget::MemoryLimit {
        category: memory_budget::Category::Metadata,
        requested: 0,
        reason: memory_budget::LimitReason::Category,
    }
}
struct Bounded<'a> {
    source: infobase::config::ConfigTxnSource<'a>,
    inflated: std::cell::Cell<u64>,
    memory: memory_budget::MemoryBudget,
}
impl<'a> Bounded<'a> {
    fn new(source: infobase::config::ConfigTxnSource<'a>) -> Self {
        Self {
            source,
            inflated: std::cell::Cell::new(ENTRY_LIMIT),
            memory: memory_budget::MemoryBudget::new(memory_budget::Limits::new(TOTAL_LIMIT)),
        }
    }
}
impl EntrySource for Bounded<'_> {
    fn list(&self) -> mdconf::Result<Vec<EntryMeta>> {
        self.source.list()
    }
    fn stored(&self, name: &str) -> mdconf::Result<Option<Vec<u8>>> {
        self.stored_bounded(name, ENTRY_LIMIT as usize)
    }
    fn stored_bounded(&self, name: &str, limit: usize) -> mdconf::Result<Option<Vec<u8>>> {
        self.source
            .stored_bounded(name, limit.min(entry_limit(name) as usize))
    }
    fn stored_inflated_bounded(&self, name: &str, limit: u64) -> mdconf::Result<Option<Inflated>> {
        use memory_budget::{BudgetedVec, Category};
        let limit = limit
            .min(entry_limit(name))
            .min(TOTAL_LIMIT.saturating_sub(self.inflated.get()));
        let Some(stored_len) = self.source.stored_len(name)? else {
            return Ok(None);
        };
        if stored_len > limit {
            return Err(mdconf::Error::Memory(refusal()));
        }
        let held = self
            .memory
            .try_reserve(Category::Metadata, stored_len)
            .map_err(mdconf::Error::Memory)?;
        let Some(stored) = self.source.stored_bounded(name, stored_len as usize)? else {
            return Ok(None);
        };
        let (bytes, was_deflated) =
            match zraw::inflate_budgeted(&stored, limit, &self.memory, Category::Metadata) {
                Ok(bytes) => (bytes, true),
                Err(
                    zraw::ManagedInflateError::TooLarge(_) | zraw::ManagedInflateError::Memory(_),
                ) => return Err(mdconf::Error::Memory(refusal())),
                Err(_) => {
                    let mut bytes = BudgetedVec::try_with_capacity(
                        &self.memory,
                        Category::Metadata,
                        stored.len(),
                    )
                    .map_err(|_| mdconf::Error::Memory(refusal()))?;
                    bytes
                        .try_extend_from_slice(&stored)
                        .map_err(|_| mdconf::Error::Memory(refusal()))?;
                    (bytes.into_charged_vec(), false)
                }
            };
        self.inflated.set(self.inflated.get() + bytes.len() as u64);
        drop(stored);
        drop(held);
        Ok(Some(Inflated {
            bytes,
            was_deflated,
        }))
    }
}
#[derive(Default)]
struct Decoded(std::sync::atomic::AtomicU64);
impl Decoded {
    fn admit_bytes(&self, bytes: u64) -> std::result::Result<(), mdconf::MeterRefusal> {
        use std::sync::atomic::Ordering;
        let mut current = self.0.load(Ordering::Relaxed);
        loop {
            let total = current
                .checked_add(bytes)
                .filter(|n| *n <= TOTAL_LIMIT)
                .ok_or_else(|| mdconf::MeterRefusal(refusal()))?;
            match self
                .0
                .compare_exchange_weak(current, total, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => return Ok(()),
                Err(actual) => current = actual,
            }
        }
    }
}
impl mdconf::LoadMeter for Decoded {
    fn admit(&self, entry: &str, bytes: u64) -> std::result::Result<(), mdconf::MeterRefusal> {
        if bytes > entry_limit(entry) {
            return Err(mdconf::MeterRefusal(refusal()));
        }
        Ok(())
    }
    fn hold(&self, entry: &str, bytes: u64) -> std::result::Result<(), mdconf::MeterRefusal> {
        if bytes > entry_limit(entry) {
            return Err(mdconf::MeterRefusal(refusal()));
        }
        self.admit_bytes(bytes)
    }
    fn admit_copy(&self, entry: &str, bytes: u64) -> std::result::Result<(), mdconf::MeterRefusal> {
        self.admit(entry, bytes)?;
        self.admit_bytes(bytes)
    }
}
impl Facts {
    pub fn read(tx: &onecd::ReadTxn) -> Result<Self, UserWriteError> {
        let source = infobase::config::ConfigTxnSource::read_bounded(tx, "CONFIG", 100_000, 4096)
            .map_err(|_| UserWriteError::UnsupportedLayout("CONFIG unavailable"))?;
        if source.is_empty() {
            return Err(UserWriteError::UnsupportedLayout(
                "CONFIG rights unavailable",
            ));
        }
        let cfg = Configuration::load(
            &Bounded::new(source),
            &LoadOptions {
                keep_source: false,
                profile: LoadProfile::Query,
                extra_stages: Stages::RIGHTS,
                meter: Some(std::sync::Arc::new(Decoded::default())),
                ..Default::default()
            },
        )
        .map_err(|_| UserWriteError::UnsupportedLayout("CONFIG rights or schema unresolved"))?;
        if cfg
            .diagnostics
            .misses
            .keys()
            .any(|k| k.starts_with("rights:"))
        {
            return Err(UserWriteError::UnsupportedLayout(
                "CONFIG rights unresolved",
            ));
        }
        let right = mdconf::parts::Right::from_name("Administration").ok_or(
            UserWriteError::UnsupportedLayout("Administration schema unavailable"),
        )?;
        let mut roles = Vec::new();
        let mut admin = Vec::new();
        let mut bytes = 0usize;
        for role in cfg.objects.iter().filter(|o| o.kind == MdKind::Role) {
            bytes = bytes
                .checked_add(role.name.len())
                .filter(|n| *n <= 1_048_576)
                .ok_or(UserWriteError::UnsupportedLayout("role name limit"))?;
            if roles.len() >= 4096 || role.name.len() > 4096 {
                return Err(UserWriteError::UnsupportedLayout("role limit"));
            }
            let rights = role
                .rights()
                .ok_or(UserWriteError::UnsupportedLayout("role rights unavailable"))?;
            let administrative = rights.value(&mdconf::parts::RightTarget::Configuration, right);
            if administrative {
                admin.push(role.id);
            }
            roles.push(RoleChoice {
                id: role.id.to_string(),
                name: role.name.clone(),
                administrative,
            });
        }
        let separated = cfg
            .objects
            .iter()
            .filter(|o| o.kind == MdKind::CommonAttribute)
            .any(|o| {
                o.props.iter().any(|p| {
                    matches!(
                        p.name.as_str(),
                        "UsersSeparation" | "AuthenticationSeparation"
                    ) && matches!(&p.value, mdconf::Value::Text(v) if v=="Separate")
                })
            });
        Ok(Self {
            cfg,
            roles,
            admin,
            separated,
        })
    }
    pub fn is_admin(&self, c: &Card) -> bool {
        c.data_separation.is_empty()
            && (c.authentication.standard || (c.authentication.os && !c.os_user.trim().is_empty()))
            && c.roles
                .iter()
                .any(|r| brace::Guid::parse(&r.id).is_some_and(|id| self.admin.contains(&id)))
    }
    pub fn roles(&self, roles: &[String]) -> Result<Vec<brace::Guid>, UserWriteError> {
        if roles.len() > 4096 {
            return Err(UserWriteError::InvalidInput("roles limit"));
        }
        let mut result = Vec::with_capacity(roles.len());
        for role in roles {
            let id = brace::Guid::parse(role).ok_or(UserWriteError::InvalidInput("role UUID"))?;
            if result.contains(&id) || !self.roles.iter().any(|r| r.id == id.to_string()) {
                return Err(UserWriteError::InvalidInput("unknown or duplicate role"));
            }
            result.push(id);
        }
        Ok(result)
    }
}
