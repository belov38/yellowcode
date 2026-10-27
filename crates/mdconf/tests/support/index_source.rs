use super::mdconf_api::{EntryMeta, EntrySource, ManagedEntrySource};
use memory_budget::{BudgetError, BudgetedVec, Category, Charged, Limits, MemoryBudget};

pub struct Source {
    pub refuse: std::cell::Cell<bool>,
    pub budget: MemoryBudget,
    pub metadata: Vec<EntryMeta>,
    stored: Vec<Vec<u8>>,
}
impl Source {
    #[allow(dead_code)]
    pub fn replace_stored(&mut self, name: &str, bytes: Vec<u8>) {
        let at = self
            .metadata
            .iter()
            .position(|meta| meta.name == name)
            .expect("existing test entry");
        self.stored[at] = bytes;
    }

    pub fn from_native(native: &dyn EntrySource) -> Self {
        let metadata = native.list().unwrap();
        let stored = metadata
            .iter()
            .map(|meta| native.stored(&meta.name).unwrap().unwrap())
            .collect();
        Self {
            refuse: std::cell::Cell::new(false),
            budget: MemoryBudget::new(Limits::new(256 << 20)),
            metadata,
            stored,
        }
    }
}
impl ManagedEntrySource for Source {
    type Error = BudgetError;
    fn list(&self) -> &[EntryMeta] {
        &self.metadata
    }
    fn memory_budget(&self) -> &MemoryBudget {
        &self.budget
    }
    fn stored(&self, name: &str) -> Result<Option<Charged<Vec<u8>>>, BudgetError> {
        if self.refuse.get() {
            return Err(BudgetError::AllocationFailed);
        }
        let Some(at) = self.metadata.iter().position(|meta| meta.name == name) else {
            return Ok(None);
        };
        let mut out = BudgetedVec::try_with_capacity(
            &self.budget,
            Category::Metadata,
            self.stored[at].len(),
        )?;
        out.try_extend_from_slice(&self.stored[at])?;
        Ok(Some(out.into_charged_vec()))
    }
    fn stored_bounded(
        &self,
        name: &str,
        limit: usize,
    ) -> Result<Option<Charged<Vec<u8>>>, BudgetError> {
        assert!(
            self.metadata
                .iter()
                .position(|meta| meta.name == name)
                .is_none_or(|at| self.stored[at].len() <= limit)
        );
        self.stored(name)
    }
}
