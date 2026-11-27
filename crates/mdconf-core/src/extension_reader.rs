use crate::configinfo::{
    ConfigInfo, ConfigInfoError, RetainedConfigInfo, check_manifest_input, forecast_with_depth,
};
use crate::extension_configuration::{ConfigurationError, ExtensionConfiguration};
use crate::{EntrySource, StoredBytes};
use brace::{Guid, Node};
use memory_budget::{BudgetError, Category, Charged, Limits, MemoryBudget, Reservation};

const STORED_LIMIT: usize = 1 << 20;
const INPUT_LIMIT: u64 = 4 << 20;
pub(crate) const WORKING_LIMIT: u64 = 64 << 20;
const DEPTH_LIMIT: u64 = 128;
const CATEGORY: Category = Category::ReaderWorking;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionEntry {
    Manifest,
    Root,
    Object(Guid),
    Module(Guid),
}

#[derive(Debug, thiserror::Error)]
pub enum ExtensionReadError {
    #[error("extension {entry:?}: {source}")]
    Source {
        entry: ExtensionEntry,
        #[source]
        source: crate::Error,
    },
    #[error("extension root entry {0} is missing")]
    MissingRoot(Guid),
    #[error("extension {0:?} disappeared during read")]
    Disappeared(ExtensionEntry),
    #[error("extension {0:?} stored input exceeds 1 MiB")]
    StoredTooLarge(ExtensionEntry),
    #[error("extension {0:?} stored size changed during read")]
    StoredSizeChanged(ExtensionEntry),
    #[error("extension {entry:?}: {source}")]
    Inflate {
        entry: ExtensionEntry,
        #[source]
        source: zraw::ManagedInflateError,
    },
    #[error("extension manifest: {0}")]
    Manifest(#[source] ConfigInfoError),
    #[error("extension root: {0}")]
    Brace(#[source] brace::ManagedParseError),
    #[error("extension root: {0}")]
    Configuration(#[from] ConfigurationError),
    #[error("extension {0:?} exceeds 128 list levels (including manifest wrapper)")]
    DepthLimit(ExtensionEntry),
    #[error("extension: {0}")]
    Memory(#[from] BudgetError),
}
type Result<T> = std::result::Result<T, ExtensionReadError>;

pub struct ExtensionRoot {
    manifest: RetainedConfigInfo,
    root: Charged<Node>,
    _caller: Reservation,
}

impl ExtensionRoot {
    pub fn objects<'a>(
        &'a self,
        source: &'a dyn EntrySource,
    ) -> std::result::Result<
        crate::extension_objects::ExtensionObjects<'a>,
        crate::extension_objects::ObjectReadError,
    > {
        crate::extension_objects::ExtensionObjects::new(self, source, &self._caller)
    }

    pub fn read(source: &dyn EntrySource, budget: &MemoryBudget) -> Result<Option<Self>> {
        Self::read_profile(source, budget, |_| Ok(()))
    }

    pub fn read_installed(
        source: &dyn EntrySource,
        budget: &MemoryBudget,
        prepare: impl FnOnce(&ConfigInfo) -> Result<()>,
    ) -> Result<Option<Self>> {
        Self::read_profile(source, budget, prepare)
    }

    fn read_profile(
        source: &dyn EntrySource,
        budget: &MemoryBudget,
        prepare: impl FnOnce(&ConfigInfo) -> Result<()>,
    ) -> Result<Option<Self>> {
        let Some(manifest_size) = size(source, "configinfo", ExtensionEntry::Manifest)? else {
            return Ok(None);
        };
        let caller = budget
            .try_reserve(CATEGORY, WORKING_LIMIT)
            .map_err(BudgetError::from)?;
        let local = MemoryBudget::try_new(Limits::new(WORKING_LIMIT))?;
        let manifest = {
            let raw = read_present(
                source,
                "configinfo",
                manifest_size,
                ExtensionEntry::Manifest,
                &local,
            )?;
            check_manifest_input(&raw).map_err(ExtensionReadError::Manifest)?;
            admit(&raw, &local, ExtensionEntry::Manifest, 1)?;
            ConfigInfo::parse_retained(&raw, &local).map_err(ExtensionReadError::Manifest)?
        };
        prepare(&manifest.value)?;
        let name = root_name(manifest.value.root_id);
        let name = std::str::from_utf8(&name).map_err(|_| BudgetError::Overflow)?;
        let size = size(source, name, ExtensionEntry::Root)?
            .ok_or(ExtensionReadError::MissingRoot(manifest.value.root_id))?;
        let root = {
            let raw = read_present(source, name, size, ExtensionEntry::Root, &local)?;
            admit(&raw, &local, ExtensionEntry::Root, 0)?;
            brace::parse_bytes_budgeted(&raw, &local, CATEGORY)
                .map_err(ExtensionReadError::Brace)?
        };
        ExtensionConfiguration::from_node(&root)?;
        let unused = WORKING_LIMIT
            .checked_sub(local.current_bytes())
            .ok_or(BudgetError::Overflow)?;
        caller.shrink(unused).map_err(BudgetError::from)?;
        Ok(Some(Self {
            manifest,
            root,
            _caller: caller,
        }))
    }

    pub fn manifest(&self) -> &ConfigInfo {
        &self.manifest.value
    }

    pub fn installed_classes(&self) -> crate::extension_objects::ExtensionClasses<'_> {
        let parts = self
            .root
            .as_list()
            .map_or(0, |root| root.len().saturating_sub(4));
        crate::extension_objects::ExtensionClasses::from_root(&self.root, parts)
    }

    pub fn installed_objects<'a>(
        &'a self,
        source: &'a dyn EntrySource,
    ) -> std::result::Result<
        crate::extension_objects::InstalledObjects<'a>,
        crate::extension_objects::ObjectReadError,
    > {
        crate::extension_objects::InstalledObjects::new(self, source, &self._caller)
    }

    pub fn configuration(
        &self,
    ) -> std::result::Result<ExtensionConfiguration<'_>, ConfigurationError> {
        ExtensionConfiguration::from_node(&self.root)
    }
}

pub(crate) fn size(
    source: &dyn EntrySource,
    name: &str,
    entry: ExtensionEntry,
) -> Result<Option<u64>> {
    source
        .stored_len(name)
        .map_err(|source| ExtensionReadError::Source { entry, source })
}

pub(crate) fn read_present(
    source: &dyn EntrySource,
    name: &str,
    size: u64,
    entry: ExtensionEntry,
    budget: &MemoryBudget,
) -> Result<Charged<Vec<u8>>> {
    if size > STORED_LIMIT as u64 {
        return Err(ExtensionReadError::StoredTooLarge(entry));
    }
    let _hold = budget
        .try_reserve(CATEGORY, STORED_LIMIT as u64)
        .map_err(BudgetError::from)?;
    let stored: StoredBytes = source
        .stored_owned_bounded(name, STORED_LIMIT)
        .map_err(|source| ExtensionReadError::Source { entry, source })?
        .ok_or(ExtensionReadError::Disappeared(entry))?;
    if stored.capacity() > STORED_LIMIT {
        return Err(ExtensionReadError::StoredTooLarge(entry));
    }
    if stored.len() as u64 != size {
        return Err(ExtensionReadError::StoredSizeChanged(entry));
    }
    zraw::inflate_budgeted(&stored, INPUT_LIMIT, budget, CATEGORY)
        .map_err(|source| ExtensionReadError::Inflate { entry, source })
}

pub(crate) fn admit(
    input: &[u8],
    budget: &MemoryBudget,
    entry: ExtensionEntry,
    wrapper: u64,
) -> Result<()> {
    let (bytes, depth) = forecast_with_depth(input)?;
    if depth.checked_add(wrapper).ok_or(BudgetError::Overflow)? > DEPTH_LIMIT {
        return Err(ExtensionReadError::DepthLimit(entry));
    }
    drop(
        budget
            .try_reserve(CATEGORY, bytes)
            .map_err(BudgetError::from)?,
    );
    Ok(())
}

pub(crate) fn root_name(id: Guid) -> [u8; 36] {
    let mut out = [b'-'; 36];
    let mut digits = id.as_bytes().iter().flat_map(|byte| [byte >> 4, byte & 15]);
    for (offset, slot) in out.iter_mut().enumerate() {
        if !matches!(offset, 8 | 13 | 18 | 23)
            && let Some(digit) = digits.next()
        {
            *slot = if digit < 10 {
                b'0' + digit
            } else {
                b'a' + digit - 10
            };
        }
    }
    out
}
