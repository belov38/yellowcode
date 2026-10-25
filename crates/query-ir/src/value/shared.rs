use memory_budget::{MemoryBudget, Shared};
use std::sync::Arc;

pub struct QShared<T>(Backing<T>);
enum Backing<T> {
    Legacy(Arc<T>),
    Managed(Shared<T>),
}
impl<T> QShared<T> {
    pub(crate) fn managed_on(&self, budget: &MemoryBudget) -> Option<Self> {
        match &self.0 {
            Backing::Managed(value) if value.reservation().budget().same_instance(budget) => {
                Some(Self(Backing::Managed(value.clone())))
            }
            _ => None,
        }
    }
    pub(crate) fn from_managed(value: Shared<T>) -> Self {
        Self(Backing::Managed(value))
    }
}
impl<T> Clone for QShared<T> {
    fn clone(&self) -> Self {
        Self(match &self.0 {
            Backing::Legacy(value) => Backing::Legacy(value.clone()),
            Backing::Managed(value) => Backing::Managed(value.clone()),
        })
    }
}
impl<T> std::ops::Deref for QShared<T> {
    type Target = T;
    fn deref(&self) -> &T {
        match &self.0 {
            Backing::Legacy(value) => value,
            Backing::Managed(value) => value,
        }
    }
}
impl<T> AsRef<T> for QShared<T> {
    fn as_ref(&self) -> &T {
        self
    }
}
impl<T: std::fmt::Debug> std::fmt::Debug for QShared<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&**self, f)
    }
}
impl<T: PartialEq> PartialEq for QShared<T> {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}
impl<T: Eq> Eq for QShared<T> {}
impl<T: std::hash::Hash> std::hash::Hash for QShared<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::hash::Hash::hash(&**self, state);
    }
}
impl<T> From<Arc<T>> for QShared<T> {
    fn from(value: Arc<T>) -> Self {
        Self(Backing::Legacy(value))
    }
}
