use super::{ExtensionObject, ObjectReadError};
use crate::extension_reader::{self as reader, ExtensionEntry, ExtensionReadError};
use crate::modules::{is_plain_module_text, kind_of_file};
use crate::{EntrySource, ModuleKind};
use brace::Guid;
use mdschema::static_schema;
use memory_budget::{Category, Charged, MemoryBudget};
use std::io::Write;
use v8cont::{ManagedContainer, ManagedReadError};

const ENTRY_NAME_CAPACITY: usize = 40;

#[derive(Debug, thiserror::Error)]
#[error("extension module {header_id}.{suffix}: {source}")]
pub struct ModuleReadError {
    pub header_id: Guid,
    pub suffix: u8,
    #[source]
    pub source: ModuleReadFailure,
}

#[derive(Debug, thiserror::Error)]
pub enum ModuleReadFailure {
    #[error(transparent)]
    Entry(#[from] ExtensionReadError),
    #[error(transparent)]
    Container(#[from] ManagedReadError),
    #[error("expected exactly 'info' and 'text' in the nested container")]
    Parts,
    #[error("the module keeps no source text (nested 'info' and 'image')")]
    NoSource,
    #[error("invalid module entry name")]
    Name,
}

pub struct ExtensionModule<'a> {
    header_id: Guid,
    suffix: u8,
    kind: ModuleKind,
    name: &'static str,
    size: u64,
    source: &'a dyn EntrySource,
    budget: &'a MemoryBudget,
}

#[derive(Debug)]
pub struct ExtensionModuleText<'a> {
    bytes: Charged<Vec<u8>>,
    _budget: &'a MemoryBudget,
}

impl AsRef<[u8]> for ExtensionModuleText<'_> {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

impl ExtensionObject<'_> {
    pub fn modules(
        &self,
    ) -> Result<impl Iterator<Item = Result<ExtensionModule<'_>, ModuleReadError>>, ObjectReadError>
    {
        let header_id = self.loaded.header_id()?;
        let slots = self
            .loaded
            .reference
            .kind
            .and_then(|kind| {
                static_schema()
                    .table("modules.json")?
                    .get("modules")?
                    .get(kind)
            })
            .and_then(|slots| slots.as_object())
            .unwrap_or_default();
        Ok(slots.iter().filter_map(move |(suffix, value)| {
            let suffix = suffix.parse::<u8>().ok()?;
            let file = value.as_str()?;
            let kind = kind_of_file(file)?;
            let name = file.strip_suffix(".bsl")?;
            let module = ExtensionModule {
                header_id,
                suffix,
                kind,
                name,
                size: 0,
                source: self.source,
                budget: self.local,
            };
            module.locate().transpose()
        }))
    }
}

impl<'a> ExtensionModule<'a> {
    pub fn kind(&self) -> ModuleKind {
        self.kind
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    fn locate(self) -> Result<Option<Self>, ModuleReadError> {
        let mut buffer = [0; ENTRY_NAME_CAPACITY];
        let size = self
            .entry_name(&mut buffer)
            .and_then(|name| reader::size(self.source, name, self.entry()).map_err(Into::into))
            .map_err(|error| self.wrap(error))?;
        Ok(size.map(|size| Self { size, ..self }))
    }

    pub fn read(&self) -> Result<ExtensionModuleText<'a>, ModuleReadError> {
        let mut buffer = [0; ENTRY_NAME_CAPACITY];
        let bytes = self
            .read_bytes(&mut buffer)
            .map_err(|error| self.wrap(error))?;
        Ok(ExtensionModuleText {
            bytes,
            _budget: self.budget,
        })
    }

    fn entry_name<'b>(
        &self,
        buffer: &'b mut [u8; ENTRY_NAME_CAPACITY],
    ) -> Result<&'b str, ModuleReadFailure> {
        let mut dest = buffer.as_mut_slice();
        write!(dest, "{}.{}", self.header_id, self.suffix).map_err(|_| ModuleReadFailure::Name)?;
        let len = ENTRY_NAME_CAPACITY - dest.len();
        let bytes = buffer.get(..len).ok_or(ModuleReadFailure::Name)?;
        std::str::from_utf8(bytes).map_err(|_| ModuleReadFailure::Name)
    }

    fn entry(&self) -> ExtensionEntry {
        ExtensionEntry::Module(self.header_id)
    }

    fn wrap(&self, source: ModuleReadFailure) -> ModuleReadError {
        ModuleReadError {
            header_id: self.header_id,
            suffix: self.suffix,
            source,
        }
    }

    fn read_bytes(
        &self,
        buffer: &mut [u8; ENTRY_NAME_CAPACITY],
    ) -> Result<Charged<Vec<u8>>, ModuleReadFailure> {
        let raw = reader::read_present(
            self.source,
            self.entry_name(buffer)?,
            self.size,
            self.entry(),
            self.budget,
        )?;
        if is_plain_module_text(&raw) {
            return Ok(raw);
        }

        let container =
            ManagedContainer::open(raw.as_slice(), self.budget, Category::ReaderWorking)?;
        let [_, _] = container.entries() else {
            return Err(ModuleReadFailure::Parts);
        };
        let info = container.find("info").ok_or(ModuleReadFailure::Parts)?;
        let Some(text) = container.find("text") else {
            return Err(match container.find("image") {
                Some(_) => ModuleReadFailure::NoSource,
                None => ModuleReadFailure::Parts,
            });
        };
        drop(container.read_data(info, raw.len() as u64)?);
        container
            .read_data(text, raw.len() as u64)
            .map_err(Into::into)
    }
}
