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
#[error("cannot read SQL configuration compatibility: {message}")]
pub struct Error {
    message: String,
    limit: bool,
}

impl Error {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            limit: false,
        }
    }

    fn budget(error: impl Into<BudgetError>) -> Self {
        let error = error.into();
        Self {
            limit: matches!(error, BudgetError::Limit(_)),
            message: error.to_string(),
        }
    }

    fn managed(error: &(dyn std::error::Error + 'static)) -> Self {
        let mut limit = false;
        let mut current = Some(error);
        while let Some(at) = current {
            limit |= at
                .downcast_ref::<BudgetError>()
                .is_some_and(|e| matches!(e, BudgetError::Limit(_)))
                || at.downcast_ref::<memory_budget::MemoryLimit>().is_some()
                || at.downcast_ref::<Error>().is_some_and(|e| e.limit);
            current = at.source();
        }
        Self {
            message: error.to_string(),
            limit,
        }
    }
}

pub fn compatibility_staged(
    spool: &mut crate::sql_config_spool::Spool,
    budget_bytes: u64,
) -> Result<u32, Error> {
    compatibility_from_source(&StagedSource::new(spool, budget_bytes)?)
}

pub fn inspect_staged_separation_facts(
    spool: &mut crate::sql_config_spool::Spool,
    budget_bytes: u64,
) -> Result<SeparationFacts, Error> {
    separation_facts_from_source(&StagedSource::new(spool, budget_bytes)?)
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
                u64::try_from(row.name.len()).map_err(|_| Error::new("name size overflow"))?;
            sum.checked_add(len)
                .ok_or_else(|| Error::new("name size overflow"))
        })?;
        let names = budget
            .try_reserve(Category::Metadata, names_bytes)
            .map_err(Error::budget)?;
        let mut entries = BudgetedVec::try_with_capacity(&budget, Category::Metadata, rows.len())
            .map_err(Error::budget)?;
        for row in rows {
            let mut name = String::new();
            name.try_reserve_exact(row.name.len())
                .map_err(|_| Error::new("name allocation failed"))?;
            let extra = name.capacity().saturating_sub(row.name.len());
            if extra != 0 {
                names
                    .try_grow(u64::try_from(extra).map_err(|_| Error::new("name size overflow"))?)
                    .map_err(Error::budget)?;
            }
            name.push_str(&row.name);
            entries
                .try_push(EntryMeta {
                    name,
                    ctime: Default::default(),
                    mtime: Default::default(),
                    attrs: 0,
                })
                .map_err(Error::budget)?;
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
            .map_err(|_| Error::new("staged entry already borrowed"))?;
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
            .ok_or_else(|| Error::new("missing staged entry"))?
            .stored_size;
        let size = usize::try_from(size).map_err(|_| Error::new("staged entry size overflow"))?;
        if size > limit {
            return Err(Error::new("stored entry exceeds limit"));
        }
        let mut data = BudgetedVec::try_with_capacity(&self.budget, Category::Metadata, size)
            .map_err(Error::budget)?;
        data.try_resize(size, 0).map_err(Error::budget)?;
        spool
            .reader(index)
            .map_err(|e| Error::new(e.to_string()))?
            .read_exact(data.as_mut_slice())
            .map_err(|e| Error::new(e.to_string()))?;
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
            .ok_or_else(|| Error::new("missing payload"))?;
        if bytes.len() > limit {
            return Err(Error::new("stored entry exceeds limit"));
        }
        let mut copy =
            BudgetedVec::try_with_capacity(&self.budget, Category::Metadata, bytes.len())
                .map_err(Error::budget)?;
        copy.try_extend_from_slice(bytes).map_err(Error::budget)?;
        Ok(Some(copy.into_charged_vec()))
    }
}

fn source(config: &SqlConfig, budget_bytes: u64) -> Result<ConfigSource<'_>, Error> {
    if config.rows.len() != config.payloads.len() {
        return Err(Error::new("payload count mismatch"));
    }
    config_cf::write_config(
        &config.rows,
        config.year_offset,
        &mut v8cont::VecData(&config.payloads),
        &mut std::io::sink(),
    )
    .map_err(|e| Error::new(e.to_string()))?;
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
    compatibility_from_source(&source)
}

fn compatibility_from_source<S: ManagedEntrySource<Error = Error>>(
    source: &S,
) -> Result<u32, Error> {
    let entries = ManagedEntries::new(source).map_err(|e| Error::new(e.to_string()))?;
    let index = ManagedIndex::build(&entries).map_err(|e| Error::new(e.to_string()))?;
    if index
        .objs
        .iter()
        .any(|(_, (kind, _))| *kind == mdconf::MdKind::CommonAttribute)
    {
        return Err(Error::new(
            "common attributes require a verified users layout",
        ));
    }
    let settings =
        ManagedSettings::read(&entries, &index).map_err(|e| Error::new(e.to_string()))?;
    let value = settings.compatibility_mode.0;
    if value <= 80214 {
        return Err(Error::new("F2 compatibility is unsupported"));
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
    let entries = ManagedEntries::new(source).map_err(|e| Error::new(e.to_string()))?;
    let index = ManagedIndex::build(&entries).map_err(|e| Error::new(e.to_string()))?;
    let settings =
        ManagedSettings::read(&entries, &index).map_err(|e| Error::new(e.to_string()))?;
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
        ManagedPredefined::build(&entries, &index).map_err(|e| Error::new(e.to_string()))?;
    let objects = ManagedObjects::shells(&index)
        .map_err(|e| Error::new(e.to_string()))?
        .decode(&entries, &index, &predefined, false)
        .map_err(|e| Error::new(e.to_string()))?;
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
                .ok_or_else(|| Error::new("missing separation property"))?;
            if values.next().is_some() || prop.codec != mdconf::Codec::Enum {
                return Err(Error::new("invalid separation property"));
            }
            match &prop.value {
                mdconf::Value::Text(value) if value == "Separate" => Ok(true),
                mdconf::Value::Text(value) if value == "DontUse" => Ok(false),
                _ => Err(Error::new("unknown separation value")),
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
        return Err(Error::new("incomplete common attributes"));
    }
    Ok(facts)
}

pub fn users_layout(config: &SqlConfig, budget_bytes: u64) -> Result<UsersLayout, Error> {
    let source = source(config, budget_bytes)?;
    users_layout_from_source(&source)
}

fn users_layout_from_source<S: ManagedEntrySource<Error = Error>>(
    source: &S,
) -> Result<UsersLayout, Error> {
    let entries = ManagedEntries::new(source).map_err(|e| Error::new(e.to_string()))?;
    let index = ManagedIndex::build(&entries).map_err(|e| Error::new(e.to_string()))?;
    let settings =
        ManagedSettings::read(&entries, &index).map_err(|e| Error::new(e.to_string()))?;
    if settings.compatibility_mode.0 <= 80214 {
        return Err(Error::new("F2 compatibility is unsupported"));
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
        return Err(Error::new("unverified number of common attributes"));
    }
    if settings.compatibility_mode.0 != 80501 {
        return Err(Error::new("unverified separated users compatibility"));
    }
    let predefined =
        ManagedPredefined::build(&entries, &index).map_err(|e| Error::new(e.to_string()))?;
    let objects = ManagedObjects::shells(&index)
        .map_err(|e| Error::new(e.to_string()))?
        .decode(&entries, &index, &predefined, false)
        .map_err(|e| Error::new(e.to_string()))?;
    let mut attrs = objects
        .objects
        .as_slice()
        .iter()
        .filter(|o| o.kind == mdconf::MdKind::CommonAttribute);
    let attr = attrs
        .next()
        .ok_or_else(|| Error::new("missing common attribute"))?;
    if attrs.next().is_some() {
        return Err(Error::new("ambiguous common attributes"));
    }
    let separated = |name: &str| -> Result<bool, Error> {
        let mut values = attr.props.iter().filter(|p| p.name == name);
        let prop = values
            .next()
            .ok_or_else(|| Error::new("missing separation property"))?;
        if values.next().is_some() || prop.codec != mdconf::Codec::Enum {
            return Err(Error::new("invalid separation property"));
        }
        match &prop.value {
            mdconf::Value::Text(value) if value == "Separate" => Ok(true),
            mdconf::Value::Text(value) if value == "DontUse" => Ok(false),
            _ => Err(Error::new("unknown separation value")),
        }
    };
    if !separated("DataSeparation")? {
        return Err(Error::new("unverified data separation"));
    }
    let users = separated("UsersSeparation")?;
    let authentication = separated("AuthenticationSeparation")?;
    Ok(if users || authentication {
        UsersLayout::UsersSpr11
    } else {
        UsersLayout::Users8
    })
}

pub const SEPARATED_COMPATIBILITY: std::ops::RangeInclusive<u32> = 80314..=80501;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DtProfile {
    pub layout: UsersLayout,
    pub compatibility: u32,
    pub peak_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileRefusal {
    OldCompatibility { found: u32 },
    UnverifiedCompatibility { found: u32 },
    LockMode { found: mdconf::DataLockControlMode },
    NoUsersSeparator { attributes: usize },
    SeveralUsersSeparators { separators: usize },
    SeparatorWithoutData,
    PartialSeparatorCompatibility { found: u32 },
    UnreadableAttribute { listed: usize, read: usize },
    MemoryLimit { budget_bytes: u64 },
}

impl ProfileRefusal {
    pub fn code(&self) -> &'static str {
        match self {
            Self::OldCompatibility { .. } => "compatibility_old",
            Self::UnverifiedCompatibility { .. } => "compatibility_unverified",
            Self::LockMode { .. } => "lock_mode",
            Self::NoUsersSeparator { .. } => "no_users_separator",
            Self::SeveralUsersSeparators { .. } => "several_users_separators",
            Self::SeparatorWithoutData => "separator_without_data",
            Self::PartialSeparatorCompatibility { .. } => "partial_separator_compatibility",
            Self::UnreadableAttribute { .. } => "unreadable_attribute",
            Self::MemoryLimit { .. } => "memory_limit",
        }
    }
}

pub const PROFILE_REFUSAL_CODES: &[&str] = &[
    "compatibility_old",
    "compatibility_unverified",
    "lock_mode",
    "no_users_separator",
    "several_users_separators",
    "separator_without_data",
    "partial_separator_compatibility",
    "unreadable_attribute",
    "memory_limit",
];

pub const PROFILE_REFUSAL_START: &str = "DT profile of the configuration is refused [";

fn version(code: u32) -> String {
    format!("{}.{}.{}", code / 10000, code / 100 % 100, code % 100)
}

impl std::fmt::Display for ProfileRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{PROFILE_REFUSAL_START}{}]: ", self.code())?;
        match self {
            Self::OldCompatibility { found } => write!(
                f,
                "режим совместимости конфигурации {} (8.2.14 или ниже); DT таких баз платформа пишет в другом формате, команда его не пишет",
                version(*found)
            ),
            Self::UnverifiedCompatibility { found } => write!(
                f,
                "в конфигурации есть общие реквизиты, а её режим совместимости {} вне проверенного диапазона {}–{}; DT для такой базы пока не поддерживается",
                version(*found),
                version(*SEPARATED_COMPATIBILITY.start()),
                version(*SEPARATED_COMPATIBILITY.end())
            ),
            Self::LockMode { found } => write!(
                f,
                "режим управления блокировкой данных конфигурации — «{}»; DT проверен только для режима «Управляемый»",
                match found {
                    mdconf::DataLockControlMode::Automatic => "Автоматический",
                    mdconf::DataLockControlMode::AutomaticAndManaged =>
                        "Автоматический и управляемый",
                    mdconf::DataLockControlMode::Managed => "Управляемый",
                }
            ),
            Self::NoUsersSeparator { attributes } => write!(
                f,
                "в конфигурации общих реквизитов: {attributes}, и ни один не разделяет пользователей или аутентификацию; какую таблицу пользователей платформа пишет в DT такой базы, не проверено"
            ),
            Self::SeveralUsersSeparators { separators } => write!(
                f,
                "пользователей или аутентификацию разделяют несколько общих реквизитов ({separators}); проверен только случай одного такого реквизита"
            ),
            Self::SeparatorWithoutData => f.write_str(
                "общий реквизит разделяет пользователей или аутентификацию, но не разделяет данные; такая конфигурация не проверена",
            ),
            Self::PartialSeparatorCompatibility { found } => write!(
                f,
                "общий реквизит разделяет только пользователей или только аутентификацию, а режим совместимости {}; такой случай проверен только при 8.5.1",
                version(*found)
            ),
            Self::UnreadableAttribute { listed, read } => write!(
                f,
                "конфигурация перечисляет общих реквизитов: {listed}, а прочитано: {read}; запись реквизита в CONFIG отсутствует или не читается"
            ),
            Self::MemoryLimit { budget_bytes } => write!(
                f,
                "проверке профиля конфигурации не хватило {} МиБ памяти",
                budget_bytes / (1024 * 1024)
            ),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    #[error("{0}")]
    Refused(ProfileRefusal),
    #[error("{0}")]
    Read(#[from] Error),
}

pub const PROFILE_BUDGET_BYTES: u64 = 512 * 1024 * 1024;

pub fn dt_profile_staged(
    spool: &mut crate::sql_config_spool::Spool,
    budget_bytes: u64,
) -> Result<DtProfile, ProfileError> {
    let refuse_memory = |error: Error| profile_error(error, budget_bytes);
    let source = StagedSource::new(spool, budget_bytes).map_err(refuse_memory)?;
    let budget = source.budget.clone();
    let read = (|| {
        let reservation = budget
            .try_reserve(Category::Metadata, 0)
            .map_err(Error::budget)?;
        let source = memory_budget::Shared::try_new(source, reservation).map_err(Error::budget)?;
        let context =
            mdconf::startup::ManagedViewerContext::build(source).map_err(|e| Error::managed(&e))?;
        let settings = context.settings();
        let attributes = context
            .index()
            .order
            .iter()
            .filter(|id| {
                context
                    .index()
                    .objs
                    .get(id)
                    .is_some_and(|(kind, _)| *kind == mdconf::MdKind::CommonAttribute)
            })
            .copied()
            .collect::<Vec<_>>();
        let listed = listed_attributes(&context)?;
        if listed.iter().any(|id| !attributes.contains(id)) {
            return Ok(Err(ProfileRefusal::UnreadableAttribute {
                listed: listed.len(),
                read: attributes.len(),
            }));
        }
        let mut separations = Vec::new();
        for id in &attributes {
            let selected = context
                .read_object(*id)
                .map_err(|e| Error::managed(&e))?
                .ok_or_else(|| Error::new("missing common attribute"))?;
            separations.push(attribute_separation(&selected.object.props)?);
        }
        Ok(Ok((
            settings.compatibility_mode.0,
            settings.data_lock_control_mode,
            separations,
        )))
    })();
    let (compatibility, lock, separations) = read
        .map_err(refuse_memory)?
        .map_err(ProfileError::Refused)?;
    let layout = classify(compatibility, lock, &separations).map_err(ProfileError::Refused)?;
    Ok(DtProfile {
        layout,
        compatibility,
        peak_bytes: budget.snapshot().peak,
    })
}

const COMMON_ATTRIBUTE_CLASS: &str = "15794563-ccec-41f6-a83c-ec5f7b9a5bc1";

fn listed_attributes<S: ManagedEntrySource<Error = Error>>(
    context: &mdconf::startup::ManagedViewerContext<S>,
) -> Result<Vec<brace::Guid>, Error> {
    let entries = context.entries().map_err(|e| Error::managed(&e))?;
    let data = entries
        .doc(&context.index().cfg_id.to_string())
        .map_err(|e| Error::managed(&e))?
        .ok_or_else(|| Error::new("missing configuration document"))?;
    let mdconf::EntryData::Brace(doc) = &*data else {
        return Err(Error::new("configuration document is not brace"));
    };
    let text = |node: &brace::Node| match node {
        brace::Node::Atom(text) | brace::Node::Str(text) => Some(text.trim().to_owned()),
        brace::Node::List(_) => None,
    };
    let mut listed = Vec::new();
    let mut stack = vec![&doc.root];
    while let Some(node) = stack.pop() {
        let brace::Node::List(items) = node else {
            continue;
        };
        let class = items.first().and_then(text);
        let count = items
            .get(1)
            .and_then(text)
            .and_then(|count| count.parse::<usize>().ok());
        if let (Some(class), Some(count)) = (class, count)
            && class == COMMON_ATTRIBUTE_CLASS
        {
            for item in items.iter().skip(2).take(count) {
                listed.push(
                    text(item)
                        .and_then(|id| brace::Guid::parse(&id))
                        .ok_or_else(|| Error::new("invalid common attribute id"))?,
                );
            }
            continue;
        }
        stack.extend(items.iter().rev());
    }
    listed.sort_unstable();
    listed.dedup();
    Ok(listed)
}

fn profile_error(error: Error, budget_bytes: u64) -> ProfileError {
    if error.limit {
        ProfileError::Refused(ProfileRefusal::MemoryLimit { budget_bytes })
    } else {
        ProfileError::Read(error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Separation {
    pub data: bool,
    pub users: bool,
    pub authentication: bool,
}

fn attribute_separation(props: &[mdconf::Prop]) -> Result<Separation, Error> {
    let separated = |name: &str| -> Result<bool, Error> {
        let mut values = props.iter().filter(|p| p.name == name);
        let prop = values
            .next()
            .ok_or_else(|| Error::new("missing separation property"))?;
        if values.next().is_some() || prop.codec != mdconf::Codec::Enum {
            return Err(Error::new("invalid separation property"));
        }
        match &prop.value {
            mdconf::Value::Text(value) if value == "Separate" => Ok(true),
            mdconf::Value::Text(value) if value == "DontUse" => Ok(false),
            _ => Err(Error::new("unknown separation value")),
        }
    };
    Ok(Separation {
        data: separated("DataSeparation")?,
        users: separated("UsersSeparation")?,
        authentication: separated("AuthenticationSeparation")?,
    })
}

pub fn classify(
    compatibility: u32,
    lock: mdconf::DataLockControlMode,
    attributes: &[Separation],
) -> Result<UsersLayout, ProfileRefusal> {
    if compatibility <= 80214 {
        return Err(ProfileRefusal::OldCompatibility {
            found: compatibility,
        });
    }
    if lock != mdconf::DataLockControlMode::Managed {
        return Err(ProfileRefusal::LockMode { found: lock });
    }
    if attributes.is_empty() {
        return Ok(UsersLayout::Users8);
    }
    if !SEPARATED_COMPATIBILITY.contains(&compatibility) {
        return Err(ProfileRefusal::UnverifiedCompatibility {
            found: compatibility,
        });
    }
    let mut separators = attributes.iter().filter(|a| a.users || a.authentication);
    let Some(separator) = separators.next() else {
        return Err(ProfileRefusal::NoUsersSeparator {
            attributes: attributes.len(),
        });
    };
    let more = separators.count();
    if more != 0 {
        return Err(ProfileRefusal::SeveralUsersSeparators {
            separators: more + 1,
        });
    }
    if !separator.data {
        return Err(ProfileRefusal::SeparatorWithoutData);
    }
    if !(separator.users && separator.authentication) && compatibility != 80501 {
        return Err(ProfileRefusal::PartialSeparatorCompatibility {
            found: compatibility,
        });
    }
    Ok(UsersLayout::UsersSpr11)
}
