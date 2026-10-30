use mdconf::{
    EntryMeta, ManagedEntries, ManagedEntrySource,
    startup::{ManagedIndex, ManagedObjects, ManagedPredefined, ManagedSettings},
};
use memory_budget::{
    BudgetError, BudgetedVec, Category, Charged, Limits, MemoryBudget, Reservation,
};
use std::{cell::RefCell, io::Read};

use crate::{config_cf, sql_config::SqlConfig};

#[derive(Debug, thiserror::Error)]
#[error("cannot read SQL configuration compatibility: {0}")]
pub struct Error(String);

pub fn compatibility_staged(
    spool: &mut crate::sql_config_spool::Spool,
    budget_bytes: u64,
) -> Result<u32, Error> {
    compatibility_from_source(&StagedSource::new(spool, budget_bytes)?, false)
}

pub fn strict_compatibility_staged(
    spool: &mut crate::sql_config_spool::Spool,
    budget_bytes: u64,
) -> Result<u32, Error> {
    compatibility_from_source(&StagedSource::new(spool, budget_bytes)?, true)
}

pub fn inspect_staged_separation_facts(
    spool: &mut crate::sql_config_spool::Spool,
    budget_bytes: u64,
) -> Result<SeparationFacts, Error> {
    separation_facts_from_source(&StagedSource::new(spool, budget_bytes)?)
}

pub fn users_layout_staged(
    spool: &mut crate::sql_config_spool::Spool,
    budget_bytes: u64,
) -> Result<UsersLayout, Error> {
    users_layout_from_source(&StagedSource::new(spool, budget_bytes)?, true)
}

struct StagedSource<'a> {
    spool: RefCell<&'a mut crate::sql_config_spool::Spool>,
    entries: BudgetedVec<EntryMeta>,
    _names: Reservation,
    budget: MemoryBudget,
}

impl<'a> StagedSource<'a> {
    fn new(
        spool: &'a mut crate::sql_config_spool::Spool,
        budget_bytes: u64,
    ) -> Result<Self, Error> {
        let budget = MemoryBudget::new(Limits::new(budget_bytes));
        let rows = spool.rows();
        let names_bytes = rows.iter().try_fold(0_u64, |sum, row| {
            let len =
                u64::try_from(row.name.len()).map_err(|_| Error("name size overflow".into()))?;
            sum.checked_add(len)
                .ok_or_else(|| Error("name size overflow".into()))
        })?;
        let names = budget
            .try_reserve(Category::Metadata, names_bytes)
            .map_err(|e| Error(e.to_string()))?;
        let mut entries = BudgetedVec::try_with_capacity(&budget, Category::Metadata, rows.len())
            .map_err(|e| Error(e.to_string()))?;
        for row in rows {
            let mut name = String::new();
            name.try_reserve_exact(row.name.len())
                .map_err(|_| Error("name allocation failed".into()))?;
            let extra = name.capacity().saturating_sub(row.name.len());
            if extra != 0 {
                names
                    .try_grow(u64::try_from(extra).map_err(|_| Error("name size overflow".into()))?)
                    .map_err(|e| Error(e.to_string()))?;
            }
            name.push_str(&row.name);
            entries
                .try_push(EntryMeta {
                    name,
                    ctime: Default::default(),
                    mtime: Default::default(),
                    attrs: 0,
                })
                .map_err(|e| Error(e.to_string()))?;
        }
        Ok(Self {
            spool: RefCell::new(spool),
            entries,
            _names: names,
            budget,
        })
    }
}

impl ManagedEntrySource for StagedSource<'_> {
    type Error = Error;
    fn list(&self) -> &[EntryMeta] {
        self.entries.as_slice()
    }
    fn memory_budget(&self) -> &MemoryBudget {
        &self.budget
    }
    fn stored(&self, name: &str) -> Result<Option<Charged<Vec<u8>>>, Error> {
        self.stored_bounded(name, usize::MAX)
    }
    fn stored_bounded(&self, name: &str, limit: usize) -> Result<Option<Charged<Vec<u8>>>, Error> {
        let mut spool = self
            .spool
            .try_borrow_mut()
            .map_err(|_| Error("staged entry already borrowed".into()))?;
        let index = match spool
            .rows()
            .binary_search_by(|row| row.name.as_str().cmp(name))
        {
            Ok(index) => index,
            Err(_) => return Ok(None),
        };
        let size = spool
            .rows()
            .get(index)
            .ok_or_else(|| Error("missing staged entry".into()))?
            .stored_size;
        let size = usize::try_from(size).map_err(|_| Error("staged entry size overflow".into()))?;
        if size > limit {
            return Err(Error("stored entry exceeds limit".into()));
        }
        let mut data = BudgetedVec::try_with_capacity(&self.budget, Category::Metadata, size)
            .map_err(|e| Error(e.to_string()))?;
        data.try_resize(size, 0).map_err(|e| Error(e.to_string()))?;
        spool
            .reader(index)
            .map_err(|e| Error(e.to_string()))?
            .read_exact(data.as_mut_slice())
            .map_err(|e| Error(e.to_string()))?;
        Ok(Some(data.into_charged_vec()))
    }
}

struct ConfigSource<'a> {
    config: &'a SqlConfig,
    entries: Vec<EntryMeta>,
    budget: MemoryBudget,
}

impl ManagedEntrySource for ConfigSource<'_> {
    type Error = Error;

    fn list(&self) -> &[EntryMeta] {
        &self.entries
    }
    fn memory_budget(&self) -> &MemoryBudget {
        &self.budget
    }

    fn stored(&self, name: &str) -> Result<Option<Charged<Vec<u8>>>, Error> {
        self.stored_bounded(name, usize::MAX)
    }

    fn stored_bounded(&self, name: &str, limit: usize) -> Result<Option<Charged<Vec<u8>>>, Error> {
        let Some(index) = self.entries.iter().position(|entry| entry.name == name) else {
            return Ok(None);
        };
        let bytes = self
            .config
            .payloads
            .get(index)
            .ok_or_else(|| Error("missing payload".into()))?;
        if bytes.len() > limit {
            return Err(Error("stored entry exceeds limit".into()));
        }
        let mut copy =
            BudgetedVec::try_with_capacity(&self.budget, Category::Metadata, bytes.len())
                .map_err(|e: BudgetError| Error(e.to_string()))?;
        copy.try_extend_from_slice(bytes)
            .map_err(|e| Error(e.to_string()))?;
        Ok(Some(copy.into_charged_vec()))
    }
}

fn source(config: &SqlConfig, budget_bytes: u64) -> Result<ConfigSource<'_>, Error> {
    if config.rows.len() != config.payloads.len() {
        return Err(Error("payload count mismatch".into()));
    }
    config_cf::write_config(
        &config.rows,
        config.year_offset,
        &mut v8cont::VecData(&config.payloads),
        &mut std::io::sink(),
    )
    .map_err(|e| Error(e.to_string()))?;
    Ok(ConfigSource {
        config,
        entries: config
            .rows
            .iter()
            .map(|row| EntryMeta {
                name: row.name.clone(),
                ctime: Default::default(),
                mtime: Default::default(),
                attrs: 0,
            })
            .collect(),
        budget: MemoryBudget::new(Limits::new(budget_bytes)),
    })
}

pub fn compatibility(config: &SqlConfig, budget_bytes: u64) -> Result<u32, Error> {
    let source = source(config, budget_bytes)?;
    compatibility_from_source(&source, false)
}

fn compatibility_from_source<S: ManagedEntrySource<Error = Error>>(
    source: &S,
    require_managed_lock: bool,
) -> Result<u32, Error> {
    let entries = ManagedEntries::new(source).map_err(|e| Error(e.to_string()))?;
    let index = ManagedIndex::build(&entries).map_err(|e| Error(e.to_string()))?;
    if index
        .objs
        .iter()
        .any(|(_, (kind, _))| *kind == mdconf::MdKind::CommonAttribute)
    {
        return Err(Error(
            "common attributes require a verified users layout".into(),
        ));
    }
    let settings = ManagedSettings::read(&entries, &index).map_err(|e| Error(e.to_string()))?;
    if require_managed_lock
        && settings.data_lock_control_mode != mdconf::DataLockControlMode::Managed
    {
        return Err(Error("unverified ordinary users lock mode".into()));
    }
    let value = settings.compatibility_mode.0;
    if value <= 80214 {
        return Err(Error("F2 compatibility is unsupported".into()));
    }
    Ok(value)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsersLayout {
    Users8,
    UsersSpr11,
}

#[derive(Debug, PartialEq, Eq)]
pub struct SeparationFacts {
    pub compatibility: u32,
    pub data_lock_control_mode: mdconf::DataLockControlMode,
    pub common_attributes: usize,
    pub data_separated: usize,
    pub users_separated: usize,
    pub authentication_separated: usize,
    pub users_and_authentication_separated: usize,
}

pub fn inspect_separation_facts(
    config: &SqlConfig,
    budget_bytes: u64,
) -> Result<SeparationFacts, Error> {
    let source = source(config, budget_bytes)?;
    separation_facts_from_source(&source)
}

fn separation_facts_from_source<S: ManagedEntrySource<Error = Error>>(
    source: &S,
) -> Result<SeparationFacts, Error> {
    let entries = ManagedEntries::new(source).map_err(|e| Error(e.to_string()))?;
    let index = ManagedIndex::build(&entries).map_err(|e| Error(e.to_string()))?;
    let settings = ManagedSettings::read(&entries, &index).map_err(|e| Error(e.to_string()))?;
    let count = index
        .objs
        .iter()
        .filter(|(_, (kind, _))| *kind == mdconf::MdKind::CommonAttribute)
        .count();
    let mut facts = SeparationFacts {
        compatibility: settings.compatibility_mode.0,
        data_lock_control_mode: settings.data_lock_control_mode,
        common_attributes: count,
        data_separated: 0,
        users_separated: 0,
        authentication_separated: 0,
        users_and_authentication_separated: 0,
    };
    if count == 0 {
        return Ok(facts);
    }
    let predefined =
        ManagedPredefined::build(&entries, &index).map_err(|e| Error(e.to_string()))?;
    let objects = ManagedObjects::shells(&index)
        .map_err(|e| Error(e.to_string()))?
        .decode(&entries, &index, &predefined, false)
        .map_err(|e| Error(e.to_string()))?;
    let mut seen = 0;
    for attr in objects
        .objects
        .as_slice()
        .iter()
        .filter(|o| o.kind == mdconf::MdKind::CommonAttribute)
    {
        let separated = |name: &str| -> Result<bool, Error> {
            let mut values = attr.props.iter().filter(|p| p.name == name);
            let prop = values
                .next()
                .ok_or_else(|| Error("missing separation property".into()))?;
            if values.next().is_some() || prop.codec != mdconf::Codec::Enum {
                return Err(Error("invalid separation property".into()));
            }
            match &prop.value {
                mdconf::Value::Text(value) if value == "Separate" => Ok(true),
                mdconf::Value::Text(value) if value == "DontUse" => Ok(false),
                _ => Err(Error("unknown separation value".into())),
            }
        };
        let data = separated("DataSeparation")?;
        let users = separated("UsersSeparation")?;
        let auth = separated("AuthenticationSeparation")?;
        facts.data_separated += usize::from(data);
        facts.users_separated += usize::from(users);
        facts.authentication_separated += usize::from(auth);
        facts.users_and_authentication_separated += usize::from(users && auth);
        seen += 1;
    }
    if seen != count {
        return Err(Error("incomplete common attributes".into()));
    }
    Ok(facts)
}

pub fn users_layout(config: &SqlConfig, budget_bytes: u64) -> Result<UsersLayout, Error> {
    let source = source(config, budget_bytes)?;
    users_layout_from_source(&source, false)
}

fn users_layout_from_source<S: ManagedEntrySource<Error = Error>>(
    source: &S,
    require_managed_lock: bool,
) -> Result<UsersLayout, Error> {
    let entries = ManagedEntries::new(source).map_err(|e| Error(e.to_string()))?;
    let index = ManagedIndex::build(&entries).map_err(|e| Error(e.to_string()))?;
    let settings = ManagedSettings::read(&entries, &index).map_err(|e| Error(e.to_string()))?;
    if settings.compatibility_mode.0 <= 80214 {
        return Err(Error("F2 compatibility is unsupported".into()));
    }
    let count = index
        .objs
        .iter()
        .filter(|(_, (kind, _))| *kind == mdconf::MdKind::CommonAttribute)
        .count();
    if count == 0 {
        return Ok(UsersLayout::Users8);
    }
    if count != 1 {
        return Err(Error("unverified number of common attributes".into()));
    }
    if settings.compatibility_mode.0 != 80501 {
        return Err(Error("unverified separated users compatibility".into()));
    }
    if require_managed_lock
        && settings.data_lock_control_mode != mdconf::DataLockControlMode::Managed
    {
        return Err(Error("unverified separated users lock mode".into()));
    }
    let predefined =
        ManagedPredefined::build(&entries, &index).map_err(|e| Error(e.to_string()))?;
    let objects = ManagedObjects::shells(&index)
        .map_err(|e| Error(e.to_string()))?
        .decode(&entries, &index, &predefined, false)
        .map_err(|e| Error(e.to_string()))?;
    let mut attrs = objects
        .objects
        .as_slice()
        .iter()
        .filter(|o| o.kind == mdconf::MdKind::CommonAttribute);
    let attr = attrs
        .next()
        .ok_or_else(|| Error("missing common attribute".into()))?;
    if attrs.next().is_some() {
        return Err(Error("ambiguous common attributes".into()));
    }
    let separated = |name: &str| -> Result<bool, Error> {
        let mut values = attr.props.iter().filter(|p| p.name == name);
        let prop = values
            .next()
            .ok_or_else(|| Error("missing separation property".into()))?;
        if values.next().is_some() || prop.codec != mdconf::Codec::Enum {
            return Err(Error("invalid separation property".into()));
        }
        match &prop.value {
            mdconf::Value::Text(value) if value == "Separate" => Ok(true),
            mdconf::Value::Text(value) if value == "DontUse" => Ok(false),
            _ => Err(Error("unknown separation value".into())),
        }
    };
    if !separated("DataSeparation")? {
        return Err(Error("unverified data separation".into()));
    }
    let users = separated("UsersSeparation")?;
    let authentication = separated("AuthenticationSeparation")?;
    Ok(if users || authentication {
        UsersLayout::UsersSpr11
    } else {
        UsersLayout::Users8
    })
}
