use super::{
    ManagedFormModule, ManagedObjectsError,
    index::{GuidText, text},
};
use crate::{EntryData, ManagedEntries, ManagedEntrySource, Module, ModuleKind};
use brace::Guid;
use memory_budget::{BudgetError, BudgetedVec, Category, Charged, MemoryBudget};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModuleSlot {
    pub owner: Guid,
    pub suffix: &'static str,
    pub kind: ModuleKind,
    form: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModuleFacts {
    pub bytes: u64,
    pub lines: u64,
}

#[derive(Debug)]
pub struct ModuleRead {
    pub module: Option<Charged<Module>>,
    pub lenient: bool,
}
impl ModuleRead {
    pub fn facts(&self) -> Option<ModuleFacts> {
        self.module.as_ref().map(|module| {
            let body = module
                .source
                .strip_prefix(b"\xef\xbb\xbf")
                .unwrap_or(&module.source);
            let lines = if body.is_empty() {
                0
            } else {
                body.iter().filter(|&&b| b == b'\n').count() as u64
                    + u64::from(body.last() != Some(&b'\n'))
            };
            ModuleFacts {
                bytes: module.source.len() as u64,
                lines,
            }
        })
    }
    pub fn charged_bytes(&self) -> u64 {
        self.module
            .as_ref()
            .map_or(0, |module| module.reservation().bytes())
    }
    pub(super) fn separate(
        budget: &MemoryBudget,
        data: &EntryData,
        kind: ModuleKind,
    ) -> Result<Self, BudgetError> {
        let module = match crate::modules::separate_module_text(data) {
            Some(bytes) => {
                let mut source =
                    BudgetedVec::try_with_capacity(budget, Category::Metadata, bytes.len())?;
                source.try_extend_from_slice(bytes)?;
                Some(
                    source
                        .into_charged_vec()
                        .map(|source| Module { kind, source }),
                )
            }
            None => None,
        };
        Ok(Self {
            module,
            lenient: false,
        })
    }
}
impl ModuleSlot {
    pub const fn separate(owner: Guid, suffix: &'static str, kind: ModuleKind) -> Self {
        Self {
            owner,
            suffix,
            kind,
            form: false,
        }
    }
    pub const fn form(owner: Guid) -> Self {
        Self {
            owner,
            suffix: "0",
            kind: ModuleKind::FormModule,
            form: true,
        }
    }
    pub fn read<S: ManagedEntrySource + ?Sized>(
        self,
        entries: &ManagedEntries<'_, S>,
    ) -> Result<ModuleRead, ManagedObjectsError<S::Error>> {
        let guid = GuidText::new(self.owner);
        let name = text(entries.memory_budget(), &[guid.as_str()?, ".", self.suffix])?;
        let data = entries.get(name.as_str())?;
        entries.forget(name.as_str());
        let Some(data) = data else {
            return Ok(ModuleRead {
                module: None,
                lenient: false,
            });
        };
        if self.form {
            let value = ManagedFormModule::decode(entries.memory_budget(), &data)?;
            Ok(ModuleRead {
                module: value.module,
                lenient: value.lenient,
            })
        } else {
            Ok(ModuleRead::separate(
                entries.memory_budget(),
                &data,
                self.kind,
            )?)
        }
    }
}
