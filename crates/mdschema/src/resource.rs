use crate::{Schema, SchemaError, StaticSchema};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ConfigFormat {
    pub version: u32,
    pub revision: u32,
}

impl std::fmt::Display for ConfigFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.version, self.revision)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ResourceError {
    #[error("unsupported configuration format {0}: no schema resource installed")]
    UnsupportedFormat(ConfigFormat),
    #[error("unsupported schema resource revision {0}")]
    UnsupportedRevision(u8),
    #[error("schema resource: {0}")]
    Broken(&'static str),
    #[error(
        "schema resource has no embedded codec capability; a budgeted static representation is required"
    )]
    StaticUnavailable,
}

impl From<ResourceError> for SchemaError {
    fn from(error: ResourceError) -> Self {
        Self(error.to_string())
    }
}

pub struct SchemaResource {
    bytes: &'static [u8],
    build: &'static str,
    formats: &'static [u8],
    tables: &'static [u8],
    count: u16,
    pub(crate) schema: crate::managed::Cell<Schema>,
    pub(crate) learned: crate::managed::Cell<crate::FactMap<String, crate::KindSpec>>,
    #[cfg(any(test, feature = "testing"))]
    pub(crate) learned_legacy_builds: std::sync::atomic::AtomicUsize,
    pub(crate) binding: crate::managed::Binding,
    pub(crate) rights: crate::managed::Cell<crate::rights::RightsTables>,
    pub(crate) templates: crate::managed::Cell<crate::templates::TemplateTables>,
    pub(crate) events: OnceLock<Result<BTreeMap<String, String>, String>>,
    pub(crate) stdattrs: crate::managed::Cell<Vec<crate::forms::KindStdAttrs>>,
    pub(crate) forms: Mutex<HashMap<crate::forms::Key, Arc<crate::forms::FormTables>>>,
    pub(crate) upgrade: crate::managed::Cell<crate::forms_upgrade::UpgradeTables>,
}
impl std::fmt::Debug for SchemaResource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SchemaResource")
            .field("build", &self.build)
            .field("format_count", &(self.formats.len() / 8))
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy)]
struct Cursor(&'static [u8]);
impl Cursor {
    fn take(&mut self, len: usize) -> Result<&'static [u8], ResourceError> {
        let head = self
            .0
            .get(..len)
            .ok_or(ResourceError::Broken("truncated envelope"))?;
        self.0 = self
            .0
            .get(len..)
            .ok_or(ResourceError::Broken("truncated envelope"))?;
        Ok(head)
    }
    fn u16(&mut self) -> Result<u16, ResourceError> {
        let bytes = self
            .take(2)?
            .try_into()
            .map_err(|_| ResourceError::Broken("invalid u16"))?;
        Ok(u16::from_le_bytes(bytes))
    }
    fn u32(&mut self) -> Result<u32, ResourceError> {
        let bytes = self
            .take(4)?
            .try_into()
            .map_err(|_| ResourceError::Broken("invalid u32"))?;
        Ok(u32::from_le_bytes(bytes))
    }
    fn text(&mut self, len: usize) -> Result<&'static str, ResourceError> {
        std::str::from_utf8(self.take(len)?).map_err(|_| ResourceError::Broken("invalid UTF-8"))
    }
    fn record(&mut self) -> Result<(&'static str, &'static str), ResourceError> {
        let len = usize::from(self.u16()?);
        let name = self.text(len)?;
        let len =
            usize::try_from(self.u32()?).map_err(|_| ResourceError::Broken("table too large"))?;
        Ok((name, self.text(len)?))
    }
}

impl SchemaResource {
    pub fn decode(bytes: &'static [u8]) -> Result<Self, ResourceError> {
        let mut cursor = Cursor(bytes);
        if cursor.take(5)? != b"MDSCH" {
            return Err(ResourceError::Broken("bad magic"));
        }
        let revision = *cursor
            .take(1)?
            .first()
            .ok_or(ResourceError::Broken("missing revision"))?;
        if revision != 1 {
            return Err(ResourceError::UnsupportedRevision(revision));
        }
        let len = usize::from(cursor.u16()?);
        let build = cursor.text(len)?;
        if build.len() > 64
            || !build
                .split('.')
                .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err(ResourceError::Broken("invalid platform build"));
        }
        let nformats = usize::from(cursor.u16()?);
        if nformats == 0 {
            return Err(ResourceError::Broken("no configuration formats"));
        }
        let formats = cursor.take(nformats * 8)?;
        let mut previous = None;
        let mut check = Cursor(formats);
        for _ in 0..nformats {
            let current = ConfigFormat {
                version: check.u32()?,
                revision: check.u32()?,
            };
            if previous.is_some_and(|p| p >= current) {
                return Err(ResourceError::Broken("formats must be unique and sorted"));
            }
            previous = Some(current);
        }
        let count = cursor.u16()?;
        let tables = cursor.0;
        let mut previous = "";
        for _ in 0..count {
            let (name, _) = cursor.record()?;
            if name <= previous {
                return Err(ResourceError::Broken(
                    "table keys must be unique and sorted",
                ));
            }
            previous = name;
        }
        if !cursor.0.is_empty() {
            return Err(ResourceError::Broken("trailing envelope bytes"));
        }
        let resource = Self {
            bytes,
            build,
            formats,
            tables,
            count,
            schema: crate::managed::Cell::new(),
            learned: crate::managed::Cell::new(),
            #[cfg(any(test, feature = "testing"))]
            learned_legacy_builds: std::sync::atomic::AtomicUsize::new(0),
            binding: crate::managed::Binding::new(),
            rights: crate::managed::Cell::new(),
            templates: crate::managed::Cell::new(),
            events: OnceLock::new(),
            stdattrs: crate::managed::Cell::new(),
            forms: Mutex::new(HashMap::new()),
            upgrade: crate::managed::Cell::new(),
        };
        let required = crate::static_schema().tables();
        if usize::from(count) != required.len() + 2
            || required
                .iter()
                .any(|(name, _)| resource.table(name).is_none())
            || resource.table("forms/compat8312.json").is_none()
            || resource.table("forms/upgrade.json").is_none()
        {
            return Err(ResourceError::Broken("missing or unsupported table set"));
        }
        Ok(resource)
    }
    pub fn bytes(&self) -> &'static [u8] {
        self.bytes
    }
    pub fn build(&self) -> &'static str {
        self.build
    }
    pub fn formats(&self) -> impl Iterator<Item = ConfigFormat> + '_ {
        self.formats.as_chunks::<8>().0.iter().filter_map(|bytes| {
            let version = u32::from_le_bytes(bytes.get(..4)?.try_into().ok()?);
            let revision = u32::from_le_bytes(bytes.get(4..)?.try_into().ok()?);
            Some(ConfigFormat { version, revision })
        })
    }
    pub fn tables(&self) -> impl Iterator<Item = (&'static str, &'static str)> {
        let mut cursor = Cursor(self.tables);
        (0..self.count).filter_map(move |_| cursor.record().ok())
    }
    fn table(&self, name: &str) -> Option<&'static str> {
        self.tables()
            .find(|(key, _)| *key == name)
            .map(|(_, text)| text)
    }
    pub fn json(&self, name: &str) -> Result<&'static str, SchemaError> {
        self.table(name)
            .ok_or_else(|| SchemaError(format!("resource table missing: {name}")))
    }
    pub fn schema(&'static self) -> Result<&'static Schema, SchemaError> {
        if self.require_embedded().is_ok() {
            return self
                .embedded_schema()
                .map_err(|e| SchemaError(e.to_string()));
        }
        self.schema.legacy_plain(|| Schema::load(self))
    }
    pub fn require_embedded(&self) -> Result<(), ResourceError> {
        if std::ptr::eq(self, embedded_resource()?) {
            Ok(())
        } else {
            Err(ResourceError::StaticUnavailable)
        }
    }
    pub fn static_schema(&self) -> Result<&'static StaticSchema, ResourceError> {
        self.require_embedded()?;
        Ok(crate::static_schema())
    }
}

static EMBEDDED: OnceLock<Result<SchemaResource, ResourceError>> = OnceLock::new();
pub fn embedded_resource() -> Result<&'static SchemaResource, ResourceError> {
    EMBEDDED
        .get_or_init(|| SchemaResource::decode(include_bytes!("../data/schema.bin")))
        .as_ref()
        .map_err(|e| *e)
}
pub fn for_format(format: ConfigFormat) -> Result<&'static SchemaResource, ResourceError> {
    let resource = embedded_resource()?;
    if resource.formats().any(|candidate| candidate == format) {
        Ok(resource)
    } else {
        Err(ResourceError::UnsupportedFormat(format))
    }
}
