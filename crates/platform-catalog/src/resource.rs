use crate::messages::{CATEGORIES_TSV, CategoryTable, MESSAGES_TSV, Messages};
use crate::{CATALOG_BIN, Catalog, ManagedSelectError, PlatformError, PlatformVersion};
use memory_budget::{BudgetError, Category, Lock, MemoryBudget, Reservation};
use std::{borrow::Cow, io::Read, path::Path, sync::OnceLock};

pub(crate) struct Resource {
    catalog_bin: Cow<'static, [u8]>,
    messages_tsv: Cow<'static, str>,
    categories_tsv: Cow<'static, str>,
    catalog: OnceLock<Catalog>,
    messages: OnceLock<Messages>,
    categories: OnceLock<CategoryTable>,
    input_owners: Lock<[Option<Reservation>; 3]>,
    catalog_owner: Lock<Option<Reservation>>,
    messages_owner: Lock<Option<Reservation>>,
    categories_owner: Lock<Option<Reservation>>,
}
pub(crate) static EMBEDDED: Resource = Resource {
    catalog_bin: Cow::Borrowed(CATALOG_BIN),
    messages_tsv: Cow::Borrowed(MESSAGES_TSV),
    categories_tsv: Cow::Borrowed(CATEGORIES_TSV),
    catalog: OnceLock::new(),
    messages: OnceLock::new(),
    categories: OnceLock::new(),
    input_owners: Lock::new([None, None, None]),
    catalog_owner: Lock::new(None),
    messages_owner: Lock::new(None),
    categories_owner: Lock::new(None),
};
struct Selected {
    installed: Option<Resource>,
}
impl Selected {
    fn embedded() -> Self {
        Self { installed: None }
    }
    fn installed(resource: Resource) -> Self {
        Self {
            installed: Some(resource),
        }
    }
    fn resource(&self) -> &Resource {
        self.installed.as_ref().unwrap_or(&EMBEDDED)
    }
}
struct Selection {
    lock: Lock<()>,
    active: OnceLock<Selected>,
}
impl Selection {
    const fn new() -> Self {
        Self {
            lock: Lock::new(()),
            active: OnceLock::new(),
        }
    }
    fn choose<E>(
        &self,
        build: &str,
        create: impl FnOnce() -> Result<Selected, E>,
        prepare: impl FnOnce(&Resource) -> Result<(), E>,
        other: impl FnOnce(&str) -> E,
        before_publish: impl FnOnce(),
    ) -> Result<(), E> {
        let _guard = self.lock.lock();
        if let Some(selected) = self.active.get() {
            let resource = selected.resource();
            if resource.build() != build {
                return Err(other(resource.build()));
            }
            return prepare(resource);
        }
        let candidate = create()?;
        prepare(candidate.resource())?;
        before_publish();
        let _ = self.active.set(candidate);
        Ok(())
    }
}
static SELECTION: Selection = Selection::new();

#[allow(
    clippy::panic,
    reason = "embedded generated resources; guarded by freshness and catalog tests"
)]
impl Resource {
    fn managed_catalog(&'static self) -> (&'static Catalog, MemoryBudget) {
        (self.catalog(), self.catalog_root())
    }
    fn catalog_root(&self) -> MemoryBudget {
        let owner = self.catalog_owner.lock();
        match owner.as_ref() {
            Some(owner) => owner.budget().root_budget(),
            None => panic!("managed preparation must install the catalog owner"),
        }
    }
    pub(crate) fn build(&self) -> &str {
        match crate::codec::split_header(&self.catalog_bin) {
            Ok((build, _)) => build,
            Err(e) => panic!("embedded data/catalog.bin is broken: {e}"),
        }
    }
    pub(crate) fn catalog(&'static self) -> &'static Catalog {
        let _guard = SELECTION.lock.lock();
        self.catalog
            .get_or_init(|| match Catalog::decode(&self.catalog_bin) {
                Ok(v) => v,
                Err(e) => panic!("embedded data/catalog.bin is broken: {e}"),
            })
    }
    pub(crate) fn messages(&'static self) -> &'static Messages {
        let _guard = SELECTION.lock.lock();
        self.messages
            .get_or_init(|| match Messages::parse_tsv(&self.messages_tsv) {
                Ok(v) => v,
                Err(e) => panic!("embedded data/messages.tsv is broken: {e}"),
            })
    }
    pub(crate) fn categories(&'static self) -> &'static CategoryTable {
        let _guard = SELECTION.lock.lock();
        self.categories
            .get_or_init(|| match CategoryTable::parse_tsv(&self.categories_tsv) {
                Ok(v) => v,
                Err(e) => panic!("embedded data/categories.tsv is broken: {e}"),
            })
    }
}
impl Resource {
    fn ensure(&self, root: &MemoryBudget) -> Result<(), ManagedSelectError> {
        self.ensure_after_catalog(root, || {})
    }
    fn ensure_after_catalog(
        &self,
        root: &MemoryBudget,
        after_catalog: impl FnOnce(),
    ) -> Result<(), ManagedSelectError> {
        {
            let mut owners = self.input_owners.lock();
            let capacities = [
                match &self.catalog_bin {
                    Cow::Borrowed(_) => 0,
                    Cow::Owned(v) => v.capacity(),
                },
                match &self.messages_tsv {
                    Cow::Borrowed(_) => 0,
                    Cow::Owned(v) => v.capacity(),
                },
                match &self.categories_tsv {
                    Cow::Borrowed(_) => 0,
                    Cow::Owned(v) => v.capacity(),
                },
            ];
            for (slot, bytes) in owners.iter_mut().zip(capacities) {
                if bytes != 0 && slot.is_none() {
                    *slot = Some(root.try_reserve(Category::Input, bytes as u64)?);
                }
            }
        }
        let mut owner = self.catalog_owner.lock();
        if owner.is_none() {
            if let Some(value) = self.catalog.get() {
                let bytes = crate::codec::copy_bytes(value.data())?
                    .checked_add(crate::index::Indexes::bytes(value.data())?)
                    .and_then(|n| n.checked_add(value.platform().len() as u64))
                    .and_then(|n| n.checked_add(std::mem::size_of::<Catalog>() as u64))
                    .ok_or(BudgetError::Overflow)?;
                *owner = Some(root.try_reserve(Category::Metadata, bytes)?);
            } else {
                let crate::AdmittedCatalog { value, _charge } =
                    Catalog::decode_with_memory(&self.catalog_bin, root)?;
                *owner = Some(_charge);
                let _ = self.catalog.set(value);
            }
        }
        drop(owner);
        after_catalog();
        let mut owner = self.messages_owner.lock();
        if owner.is_none() {
            if let Some(value) = self.messages.get() {
                *owner = Some(root.try_reserve(Category::Metadata, value.backing_bytes()?)?);
            } else {
                let admitted = crate::messages::managed_messages(&self.messages_tsv, root)?;
                *owner = Some(admitted.charge);
                let _ = self.messages.set(admitted.value);
            }
        }
        drop(owner);
        let mut owner = self.categories_owner.lock();
        if owner.is_none() {
            if let Some(value) = self.categories.get() {
                *owner = Some(root.try_reserve(Category::Metadata, value.backing_bytes()?)?);
            } else {
                let admitted = crate::messages::managed_categories(&self.categories_tsv, root)?;
                *owner = Some(admitted.charge);
                let _ = self.categories.set(admitted.value);
            }
        }
        Ok(())
    }
}
pub(crate) fn loaded_messages() -> Option<&'static Messages> {
    SELECTION.active.get()?.resource().messages.get()
}
pub(crate) fn active() -> &'static Resource {
    let _guard = SELECTION.lock.lock();
    SELECTION.active.get_or_init(Selected::embedded).resource()
}
pub(crate) fn select(build: &PlatformVersion) -> Result<(), PlatformError> {
    SELECTION.choose(
        build.as_str(),
        || {
            if !crate::is_platform_build(build.as_str()) {
                return Err(PlatformError::Unknown(build.clone()));
            }
            if EMBEDDED.build() == build.as_str() {
                return Ok(Selected::embedded());
            }
            let Some(dir) = crate::installed::find(build)? else {
                return Err(PlatformError::Unknown(build.clone()));
            };
            Ok(Selected::installed(load(build, &dir)?))
        },
        |_| Ok(()),
        |active| PlatformError::OtherSelected {
            active: PlatformVersion::new(active),
            requested: build.clone(),
        },
        || {},
    )
}
pub(crate) fn select_from_roots_with_memory(
    build: &str,
    roots: crate::DiscoveryRoots<'_>,
    root: &MemoryBudget,
) -> Result<(), ManagedSelectError> {
    SELECTION.choose(
        build,
        || {
            if !crate::is_platform_build(build) {
                return Err(ManagedSelectError::unknown(build, root));
            }
            if EMBEDDED.build() == build {
                return Ok(Selected::embedded());
            }
            let Some(dir) = find_managed(build, roots, root)? else {
                return Err(ManagedSelectError::unknown(build, root));
            };
            Ok(Selected::installed(load_managed(build, &dir.value, root)?))
        },
        |resource| resource.ensure(root),
        |active| ManagedSelectError::other(active, build, root),
        || {},
    )
}
pub(crate) fn prepare_with_memory(root: &MemoryBudget) -> Result<(), ManagedSelectError> {
    let _guard = SELECTION.lock.lock();
    if let Some(selected) = SELECTION.active.get() {
        return selected.resource().ensure(root);
    }
    EMBEDDED.ensure(root)?;
    let _ = SELECTION.active.set(Selected::embedded());
    Ok(())
}
pub(crate) fn catalog_with_memory(
    root: &MemoryBudget,
) -> Result<(&'static Catalog, MemoryBudget), ManagedSelectError> {
    prepare_with_memory(root)?;
    Ok(active().managed_catalog())
}

fn find_managed(
    build: &str,
    inputs: crate::DiscoveryRoots<'_>,
    root: &MemoryBudget,
) -> Result<Option<crate::native::PaidPath>, ManagedSelectError> {
    use crate::native::PaidPath;
    let mut roots = [None, None];
    if let Some(override_root) = inputs.override_root {
        if override_root.as_os_str().is_empty() {
            return Err(ManagedSelectError::broken(
                build,
                Path::new(""),
                "YELLOWCODE_PLATFORM_RESOURCES must name a platform resource directory",
                root,
            ));
        }
        return probe_managed(build, override_root, root);
    }
    if let Some(parent) = inputs.executable.parent() {
        let resources = PaidPath::join(parent, "resources", root)?;
        roots[0] = Some(PaidPath::join(&resources.value, "platform", root)?);
        if cfg!(target_os = "macos") && parent.file_name().is_some_and(|p| p == "MacOS") {
            if let Some(contents) = parent
                .parent()
                .filter(|p| p.file_name().is_some_and(|n| n == "Contents"))
            {
                let resources = PaidPath::join(contents, "Resources", root)?;
                let resources = PaidPath::join(&resources.value, "resources", root)?;
                roots[1] = Some(PaidPath::join(&resources.value, "platform", root)?);
            }
        } else if cfg!(target_os = "linux")
            && let Some(prefix) = parent.parent()
        {
            let lib = PaidPath::join(prefix, "lib", root)?;
            let product = PaidPath::join(&lib.value, "Yellowcode", root)?;
            let resources = PaidPath::join(&product.value, "resources", root)?;
            roots[1] = Some(PaidPath::join(&resources.value, "platform", root)?);
        }
    }
    for path in roots.into_iter().flatten() {
        if let Some(dir) = probe_managed(build, &path.value, root)? {
            return Ok(Some(dir));
        }
    }
    Ok(None)
}
fn probe_managed(
    build: &str,
    path: &Path,
    root: &MemoryBudget,
) -> Result<Option<crate::native::PaidPath>, ManagedSelectError> {
    let dir = crate::native::PaidPath::join(path, build, root)?;
    match crate::native::metadata(&dir.value, root) {
        Ok(_) => Ok(Some(dir)),
        Err(crate::native::Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
fn read_managed(
    path: &Path,
    root: &MemoryBudget,
) -> Result<crate::owned::Admitted<Vec<u8>>, ManagedSelectError> {
    let mut file = crate::native::open(path, root)?;
    let size = usize::try_from(file.metadata()?.len()).map_err(|_| BudgetError::Overflow)?;
    let charge = root.try_reserve(Category::Input, size as u64)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(size)
        .map_err(|_| BudgetError::AllocationFailed)?;
    bytes.resize(size, 0);
    file.read_exact(&mut bytes)?;
    let mut extra = [0u8; 1];
    if file.read(&mut extra)? != 0 {
        return Err(std::io::Error::from(std::io::ErrorKind::InvalidData).into());
    }
    Ok(crate::owned::Admitted {
        value: bytes,
        charge,
    })
}
fn text_managed(
    build: &str,
    path: &Path,
    root: &MemoryBudget,
) -> Result<crate::owned::Admitted<String>, ManagedSelectError> {
    let bytes = read_managed(path, root)?;
    if std::str::from_utf8(&bytes.value).is_err() {
        return Err(ManagedSelectError::broken(
            build,
            path,
            "table is not UTF-8",
            root,
        ));
    }
    let text = match String::from_utf8(bytes.value) {
        Ok(text) => text,
        Err(_) => return Err(std::io::Error::from(std::io::ErrorKind::InvalidData).into()),
    };
    let text = crate::owned::Admitted {
        value: text,
        charge: bytes.charge,
    };
    if text
        .value
        .lines()
        .next()
        .and_then(|s| s.strip_prefix("# platform "))
        != Some(build)
    {
        return Err(ManagedSelectError::broken(
            build,
            path,
            "missing or mismatched platform build",
            root,
        ));
    }
    Ok(text)
}
fn load_managed(
    build: &str,
    dir: &Path,
    root: &MemoryBudget,
) -> Result<Resource, ManagedSelectError> {
    use crate::native::PaidPath;
    let catalog_path = PaidPath::join(dir, "catalog.bin", root)?;
    let bytes = broken_io(
        build,
        &catalog_path.value,
        root,
        read_managed(&catalog_path.value, root),
    )?;
    let catalog = Catalog::decode_with_memory(&bytes.value, root)?;
    if catalog.platform() != build {
        return Err(ManagedSelectError::broken(
            build,
            &catalog_path.value,
            "catalog declares a different platform build",
            root,
        ));
    }
    let messages_path = PaidPath::join(dir, "messages.tsv", root)?;
    let messages_tsv = broken_io(
        build,
        &messages_path.value,
        root,
        text_managed(build, &messages_path.value, root),
    )?;
    let messages = crate::messages::managed_messages(&messages_tsv.value, root)?;
    let categories_path = PaidPath::join(dir, "categories.tsv", root)?;
    let categories_tsv = broken_io(
        build,
        &categories_path.value,
        root,
        text_managed(build, &categories_path.value, root),
    )?;
    let categories = crate::messages::managed_categories(&categories_tsv.value, root)?;
    Ok(Resource {
        catalog_bin: Cow::Owned(bytes.value),
        messages_tsv: Cow::Owned(messages_tsv.value),
        categories_tsv: Cow::Owned(categories_tsv.value),
        catalog: OnceLock::from(catalog.value),
        messages: OnceLock::from(messages.value),
        categories: OnceLock::from(categories.value),
        input_owners: Lock::new([
            Some(bytes.charge),
            Some(messages_tsv.charge),
            Some(categories_tsv.charge),
        ]),
        catalog_owner: Lock::new(Some(catalog._charge)),
        messages_owner: Lock::new(Some(messages.charge)),
        categories_owner: Lock::new(Some(categories.charge)),
    })
}
fn broken_io<T>(
    build: &str,
    path: &Path,
    root: &MemoryBudget,
    result: Result<T, ManagedSelectError>,
) -> Result<T, ManagedSelectError> {
    result.map_err(|error| match error {
        ManagedSelectError::Io(io) => {
            match ManagedSelectError::broken(build, path, "the file cannot be read", root) {
                ManagedSelectError::Platform(platform) => {
                    match memory_budget::BudgetedBox::try_new(
                        root,
                        Category::Metadata,
                        (platform, io),
                    ) {
                        Ok(pair) => ManagedSelectError::Unreadable(pair),
                        Err(error) => ManagedSelectError::Memory(error),
                    }
                }
                other => other,
            }
        }
        other => other,
    })
}

fn load(build: &PlatformVersion, dir: &Path) -> Result<Resource, PlatformError> {
    let catalog_path = dir.join("catalog.bin");
    let bytes = std::fs::read(&catalog_path)
        .map_err(|e| crate::installed::broken(build, &catalog_path, e))?;
    let catalog =
        Catalog::decode(&bytes).map_err(|e| crate::installed::broken(build, &catalog_path, e))?;
    if catalog.platform() != build.as_str() {
        return Err(crate::installed::broken(
            build,
            &catalog_path,
            "catalog declares a different platform build",
        ));
    }
    let messages_path = dir.join("messages.tsv");
    let messages_tsv = read_table(build, &messages_path)?;
    let messages = Messages::parse_tsv(&messages_tsv)
        .map_err(|e| crate::installed::broken(build, &messages_path, e))?;
    let categories_path = dir.join("categories.tsv");
    let categories_tsv = read_table(build, &categories_path)?;
    let categories = CategoryTable::parse_tsv(&categories_tsv)
        .map_err(|e| crate::installed::broken(build, &categories_path, e))?;
    Ok(Resource {
        catalog_bin: Cow::Owned(bytes),
        messages_tsv: Cow::Owned(messages_tsv),
        categories_tsv: Cow::Owned(categories_tsv),
        catalog: OnceLock::from(catalog),
        messages: OnceLock::from(messages),
        categories: OnceLock::from(categories),
        input_owners: Lock::new([None, None, None]),
        catalog_owner: Lock::new(None),
        messages_owner: Lock::new(None),
        categories_owner: Lock::new(None),
    })
}
fn read_table(build: &PlatformVersion, path: &Path) -> Result<String, PlatformError> {
    let text =
        std::fs::read_to_string(path).map_err(|e| crate::installed::broken(build, path, e))?;
    if text
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("# platform "))
        != Some(build.as_str())
    {
        return Err(crate::installed::broken(
            build,
            path,
            "missing or mismatched platform build",
        ));
    }
    Ok(text)
}
