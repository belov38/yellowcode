use crate::messages::{CATEGORIES_TSV, CategoryTable, MESSAGES_TSV, Messages};
use crate::{CATALOG_BIN, Catalog, PlatformError, PlatformVersion};
use std::{
    borrow::Cow,
    collections::BTreeMap,
    path::Path,
    sync::{Mutex, OnceLock},
};

pub(crate) struct Resource {
    catalog_bin: Cow<'static, [u8]>,
    messages_tsv: Cow<'static, str>,
    categories_tsv: Cow<'static, str>,
    catalog: OnceLock<Catalog>,
    messages: OnceLock<Messages>,
    categories: OnceLock<CategoryTable>,
}

pub(crate) static EMBEDDED: Resource = Resource {
    catalog_bin: Cow::Borrowed(CATALOG_BIN),
    messages_tsv: Cow::Borrowed(MESSAGES_TSV),
    categories_tsv: Cow::Borrowed(CATEGORIES_TSV),
    catalog: OnceLock::new(),
    messages: OnceLock::new(),
    categories: OnceLock::new(),
};

static ACTIVE: OnceLock<&'static Resource> = OnceLock::new();

#[allow(
    clippy::panic,
    reason = "embedded data/; guarded by real_catalog, messages and freshness tests"
)]
impl Resource {
    pub(crate) fn build(&self) -> &str {
        match crate::codec::split_header(&self.catalog_bin) {
            Ok((build, _)) => build,
            Err(e) => panic!("embedded data/catalog.bin is broken: {e}"),
        }
    }

    pub(crate) fn catalog(&'static self) -> &'static Catalog {
        self.catalog
            .get_or_init(|| match Catalog::decode(&self.catalog_bin) {
                Ok(c) => c,
                Err(e) => panic!("embedded data/catalog.bin is broken: {e}"),
            })
    }

    pub(crate) fn messages(&'static self) -> &'static Messages {
        self.messages
            .get_or_init(|| match Messages::parse_tsv(&self.messages_tsv) {
                Ok(m) => m,
                Err(e) => panic!("embedded data/messages.tsv is broken: {e}"),
            })
    }

    pub(crate) fn categories(&'static self) -> &'static CategoryTable {
        self.categories
            .get_or_init(|| match CategoryTable::parse_tsv(&self.categories_tsv) {
                Ok(t) => t,
                Err(e) => panic!("embedded data/categories.tsv is broken: {e}"),
            })
    }
}

pub(crate) fn active() -> &'static Resource {
    ACTIVE.get_or_init(|| &EMBEDDED)
}

pub(crate) fn select(build: &PlatformVersion) -> Result<(), PlatformError> {
    if let Some(r) = ACTIVE.get() {
        return check(r, build);
    }
    let r = lookup(build)?.ok_or_else(|| PlatformError::Unknown(build.clone()))?;
    check(ACTIVE.get_or_init(|| r), build)
}

static FILES: Mutex<BTreeMap<String, &'static Resource>> = Mutex::new(BTreeMap::new());

fn lookup(build: &PlatformVersion) -> Result<Option<&'static Resource>, PlatformError> {
    if !crate::is_platform_build(build.as_str()) {
        return Ok(None);
    }
    if EMBEDDED.build() == build.as_str() {
        return Ok(Some(&EMBEDDED));
    }
    let mut files = FILES
        .lock()
        .map_err(|e| crate::installed::broken(build, Path::new(""), e))?;
    if let Some(resource) = files.get(build.as_str()) {
        return Ok(Some(*resource));
    }
    let Some(dir) = crate::installed::find(build)? else {
        return Ok(None);
    };
    let resource = load(build, &dir)?;
    let resource = Box::leak(Box::new(resource));
    files.insert(build.as_str().to_owned(), resource);
    Ok(Some(resource))
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
    })
}

fn read_table(build: &PlatformVersion, path: &Path) -> Result<String, PlatformError> {
    let text =
        std::fs::read_to_string(path).map_err(|e| crate::installed::broken(build, path, e))?;
    let declared = text
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("# platform "));
    if declared != Some(build.as_str()) {
        return Err(crate::installed::broken(
            build,
            path,
            "missing or mismatched platform header",
        ));
    }
    Ok(text)
}

fn check(active: &Resource, build: &PlatformVersion) -> Result<(), PlatformError> {
    if active.build() == build.as_str() {
        Ok(())
    } else {
        Err(PlatformError::OtherSelected {
            active: PlatformVersion::new(active.build()),
            requested: build.clone(),
        })
    }
}
