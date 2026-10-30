use memory_budget::{Category, MemoryBudget, Reservation};
use std::sync::Arc;

#[derive(Debug)]
struct Storage {
    texts: Vec<String>,
    retained: Option<Reservation>,
}
#[derive(Clone, Debug)]
pub struct FailureChain(Arc<Storage>);
impl PartialEq for FailureChain {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}
impl Eq for FailureChain {}
impl std::ops::Deref for FailureChain {
    type Target = [String];
    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}
impl FailureChain {
    pub fn unbudgeted(texts: Vec<String>) -> Self {
        Self(Arc::new(Storage {
            texts,
            retained: None,
        }))
    }
    pub fn as_slice(&self) -> &[String] {
        &self.0.texts
    }
    pub fn retained_bytes(&self) -> Option<u64> {
        self.0.retained.as_ref().map(Reservation::bytes)
    }
    pub fn render(
        budget: Option<&MemoryBudget>,
        upper: u64,
        render: impl FnOnce() -> Vec<String>,
    ) -> Self {
        let retained = match budget
            .map(|b| b.try_reserve(Category::Result, upper.max(1024)))
            .transpose()
        {
            Ok(retained) => retained,
            Err(error) => return Self::unbudgeted(vec![error.public_text().into()]),
        };
        let texts = render();
        debug_assert!(
            retained.as_ref().is_none_or(|lease| {
                let used = texts.iter().fold(
                    128u64 + (texts.capacity() * std::mem::size_of::<String>()) as u64,
                    |bytes, text| bytes.saturating_add(text.capacity() as u64),
                );
                used <= lease.bytes()
            }),
            "diagnostic renderer exceeded its admitted bound"
        );
        Self(Arc::new(Storage { texts, retained }))
    }
}
