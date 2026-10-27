use memory_budget::{BudgetedVec, Shared};

#[derive(Debug)]
pub enum ViewerBody {
    Static(&'static [u8]),
    Eager(Vec<u8>),
    Owned(BudgetedVec<u8>),
    Shared(Shared<Vec<u8>>),
}
impl Default for ViewerBody {
    fn default() -> Self {
        Self::Static(&[])
    }
}
impl AsRef<[u8]> for ViewerBody {
    fn as_ref(&self) -> &[u8] {
        match self {
            Self::Static(bytes) => bytes,
            Self::Eager(bytes) => bytes.as_slice(),
            Self::Owned(bytes) => bytes.as_slice(),
            Self::Shared(bytes) => bytes.as_slice(),
        }
    }
}

pub(crate) fn json<T: serde::Serialize + ?Sized>(
    budget: &memory_budget::MemoryBudget,
    wire_limit: u64,
    value: &T,
) -> Result<ViewerBody, memory_budget::BudgetError> {
    let mut out = memory_budget::BudgetedWriter::new(budget, wire_limit)?;
    let result = serde_json::to_writer(out.serialization_sink(), value);
    if let Some(error) = out.error() {
        return Err(*error);
    }
    result.map_err(|_| memory_budget::BudgetError::Overflow)?;
    Ok(ViewerBody::Owned(out.into_buffer()))
}
