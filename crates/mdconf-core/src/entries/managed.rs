use super::EntryData;
use memory_budget::{BudgetError, BudgetedVec, Category, Charged, MemoryBudget};
use v8cont::{ManagedContainer, ManagedReadError};

type Result<T> = std::result::Result<T, BudgetError>;

fn copy(bytes: &[u8], budget: &MemoryBudget, category: Category) -> Result<Charged<Vec<u8>>> {
    let mut output = BudgetedVec::try_with_capacity(budget, category, bytes.len())?;
    output.try_extend_from_slice(bytes)?;
    Ok(output.into_charged_vec())
}

pub fn decode_stored_budgeted(
    stored: &[u8],
    budget: &MemoryBudget,
    category: Category,
    inflated_limit: u64,
) -> Result<Charged<EntryData>> {
    if stored.is_empty() {
        return Ok(copy(stored, budget, category)?.map(EntryData::Bytes));
    }
    let raw = match zraw::inflate_budgeted(stored, inflated_limit, budget, category) {
        Ok(raw) => raw,
        Err(zraw::ManagedInflateError::Memory(error)) => return Err(error),
        Err(_) => return Ok(copy(stored, budget, category)?.map(EntryData::Bytes)),
    };
    if v8cont::is_container(&raw) {
        if let Some(items) = read_nested(&raw, budget, category)? {
            return Ok(items.map(EntryData::Nested));
        }
        return Ok(raw.map(EntryData::Bytes));
    }
    let text = raw.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&raw);
    if text.first() == Some(&b'{') {
        if brace::is_json_entry(&raw) {
            return Ok(raw.map(EntryData::Json));
        }
        match brace::parse_doc_budgeted(&raw, budget, category) {
            Ok(doc) => return Ok(doc.map(EntryData::Brace)),
            Err(brace::ManagedParseError::Memory(error)) => return Err(error),
            Err(brace::ManagedParseError::Parse(_)) => {}
        }
    }
    Ok(raw.map(EntryData::Bytes))
}

type Nested = Vec<(String, Vec<u8>)>;
fn read_nested(
    raw: &[u8],
    budget: &MemoryBudget,
    category: Category,
) -> Result<Option<Charged<Nested>>> {
    let container = match ManagedContainer::open(raw, budget, category) {
        Ok(container) => container,
        Err(ManagedReadError::Memory(error)) => return Err(error),
        Err(_) => return Ok(None),
    };
    let mut output = BudgetedVec::try_with_capacity(budget, category, container.entries().len())?
        .into_charged_vec();
    for entry in container.entries() {
        let data = match container.read_data(entry, u64::MAX) {
            Ok(data) => data,
            Err(ManagedReadError::Memory(error)) => return Err(error),
            Err(_) => return Ok(None),
        };
        let name = copy(entry.name.as_bytes(), budget, category)?
            .try_map(String::from_utf8)
            .map_err(|_| BudgetError::Overflow)?;
        output = output
            .try_zip(name.try_zip(data)?)?
            .map(|(mut output, item)| {
                output.push(item);
                output
            });
    }
    Ok(Some(output))
}
