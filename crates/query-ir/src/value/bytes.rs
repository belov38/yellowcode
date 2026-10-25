use memory_budget::{BudgetError, BudgetedVec, Category, MemoryBudget, Shared};
use std::sync::Arc;

#[derive(Clone)]
pub struct QBytes(Backing);
#[derive(Clone)]
enum Backing {
    Legacy(Arc<[u8]>),
    Managed(Shared<Vec<u8>>),
}
impl QBytes {
    pub fn try_clone_budgeted(&self, budget: &MemoryBudget) -> Result<Self, BudgetError> {
        if let Backing::Managed(value) = &self.0
            && value.reservation().budget().same_instance(budget)
        {
            return Ok(Self(Backing::Managed(value.clone())));
        }
        let mut output = BudgetedVec::try_with_capacity(budget, Category::Execution, self.len())?;
        output.try_extend_from_slice(self)?;
        Ok(Self(Backing::Managed(output.try_into_shared()?)))
    }
}
impl std::ops::Deref for QBytes {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match &self.0 {
            Backing::Legacy(value) => value,
            Backing::Managed(value) => value.as_slice(),
        }
    }
}
impl AsRef<[u8]> for QBytes {
    fn as_ref(&self) -> &[u8] {
        self
    }
}
impl std::fmt::Debug for QBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&**self, f)
    }
}
impl PartialEq for QBytes {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}
impl Eq for QBytes {}
impl std::hash::Hash for QBytes {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::hash::Hash::hash(&**self, state);
    }
}
impl From<Arc<[u8]>> for QBytes {
    fn from(value: Arc<[u8]>) -> Self {
        Self(Backing::Legacy(value))
    }
}
impl From<Vec<u8>> for QBytes {
    fn from(value: Vec<u8>) -> Self {
        Self(Backing::Legacy(value.into()))
    }
}
impl From<&[u8]> for QBytes {
    fn from(value: &[u8]) -> Self {
        Self(Backing::Legacy(value.into()))
    }
}
impl<const N: usize> From<[u8; N]> for QBytes {
    fn from(value: [u8; N]) -> Self {
        Self(Backing::Legacy(value.into()))
    }
}
