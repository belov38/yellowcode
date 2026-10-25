use crate::error::{Error, Result};
use memory_budget::{BudgetError, Shared};
#[derive(Debug, Clone)]
pub(crate) enum PageNumbers {
    Legacy(Vec<u32>),
    Budgeted(Shared<Vec<u32>>),
}
impl PageNumbers {
    pub fn push(&mut self, value: u32) -> Result<()> {
        match self {
            Self::Legacy(values) => {
                values.push(value);
                Ok(())
            }
            Self::Budgeted(_) => Err(Error::Memory(BudgetError::Limit(
                memory_budget::MemoryLimit {
                    category: memory_budget::Category::Metadata,
                    requested: 4,
                    reason: memory_budget::LimitReason::InvalidSize,
                },
            ))),
        }
    }
}
impl From<Vec<u32>> for PageNumbers {
    fn from(values: Vec<u32>) -> Self {
        Self::Legacy(values)
    }
}
impl std::ops::Deref for PageNumbers {
    type Target = [u32];
    fn deref(&self) -> &[u32] {
        match self {
            Self::Legacy(values) => values,
            Self::Budgeted(values) => values.as_slice(),
        }
    }
}
impl AsRef<[u32]> for PageNumbers {
    fn as_ref(&self) -> &[u32] {
        self
    }
}
impl<T: AsRef<[u32]>> PartialEq<T> for PageNumbers {
    fn eq(&self, other: &T) -> bool {
        self.as_ref() == other.as_ref()
    }
}
impl Eq for PageNumbers {}
impl PartialEq<PageNumbers> for Vec<u32> {
    fn eq(&self, other: &PageNumbers) -> bool {
        self.as_slice() == other.as_ref()
    }
}
impl<'a> IntoIterator for &'a PageNumbers {
    type Item = &'a u32;
    type IntoIter = std::slice::Iter<'a, u32>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
