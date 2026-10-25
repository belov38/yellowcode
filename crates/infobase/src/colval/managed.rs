use ibschema::{columns::ColType, field_view::ColumnView};
use memory_budget::{
    BudgetError, BudgetedText, Category, Charged, LimitReason, MemoryBudget, MemoryLimit,
};
use onecd::{BlobRef, BlobVal, Value};

#[derive(Debug)]
pub enum ColumnDecodeError {
    Memory(BudgetError),
    Storage(onecd::Error),
    Unsupported(&'static str),
    Mismatch,
}
impl From<BudgetError> for ColumnDecodeError {
    fn from(error: BudgetError) -> Self {
        Self::Memory(error)
    }
}
impl From<onecd::Error> for ColumnDecodeError {
    fn from(error: onecd::Error) -> Self {
        match error {
            onecd::Error::Memory(error) => Self::Memory(error),
            error => Self::Storage(error),
        }
    }
}
impl std::fmt::Display for ColumnDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Memory(error) => error.fmt(f),
            Self::Storage(error) => error.fmt(f),
            Self::Unsupported(message) => f.write_str(message),
            Self::Mismatch => f.write_str("physical value does not match column type"),
        }
    }
}
impl std::error::Error for ColumnDecodeError {}

pub enum ColumnOwner<'a> {
    Borrowed(ColumnView<'a>),
    Text(Charged<String>),
    Blob(Charged<Vec<u8>>),
}
impl ColumnOwner<'_> {
    pub fn view(&self) -> ColumnView<'_> {
        match self {
            Self::Borrowed(view) => *view,
            Self::Text(text) => ColumnView::Str(text.as_str()),
            Self::Blob(bytes) => ColumnView::Bytes(bytes.as_slice()),
        }
    }
}
fn check_bound(bytes: u64, limit: u64) -> Result<(), BudgetError> {
    if bytes > limit {
        return Err(BudgetError::Limit(MemoryLimit {
            category: Category::Row,
            requested: bytes,
            reason: LimitReason::Category,
        }));
    }
    Ok(())
}
fn invalid_reader() -> BudgetError {
    BudgetError::Limit(MemoryLimit {
        category: Category::Execution,
        requested: 0,
        reason: LimitReason::InvalidSize,
    })
}
fn read_blob(
    reference: BlobRef,
    budget: &MemoryBudget,
    limit: u64,
    reader: &mut impl FnMut(BlobRef, &MemoryBudget, u64) -> onecd::Result<Charged<Vec<u8>>>,
) -> Result<Charged<Vec<u8>>, ColumnDecodeError> {
    let declared = u64::from(reference.len);
    let descriptor = std::mem::size_of::<Charged<Vec<u8>>>() as u64;
    check_bound(
        declared
            .checked_add(descriptor)
            .ok_or(BudgetError::Overflow)?,
        limit,
    )?;
    let output = reader(reference, budget, limit)?;
    let charge = output.reservation();
    let required = (output.capacity() as u64)
        .checked_add(descriptor)
        .ok_or(BudgetError::Overflow)?;
    if !budget.same_instance(charge.budget())
        || charge.category() != Category::Execution
        || output.len() as u64 != declared
        || charge.bytes() < required
    {
        return Err(invalid_reader().into());
    }
    check_bound(charge.bytes(), limit)?;
    Ok(output)
}
fn text(
    bytes: &[u8],
    raw_charge: u64,
    budget: &MemoryBudget,
    limit: u64,
) -> Result<ColumnOwner<'static>, ColumnDecodeError> {
    if !bytes.len().is_multiple_of(2) {
        return Err(ColumnDecodeError::Unsupported("odd length of an NT value"));
    }
    let chars = || {
        char::decode_utf16(
            bytes
                .as_chunks::<2>()
                .0
                .iter()
                .copied()
                .map(u16::from_le_bytes),
        )
    };
    let mut length = 0usize;
    for value in chars() {
        let value =
            value.map_err(|_| ColumnDecodeError::Unsupported("lone surrogate in NT value"))?;
        length = length
            .checked_add(value.len_utf8())
            .ok_or(BudgetError::Overflow)?;
    }
    let live = raw_charge
        .checked_add(std::mem::size_of::<BudgetedText>() as u64)
        .and_then(|v| v.checked_add(length as u64))
        .ok_or(BudgetError::Overflow)?;
    check_bound(live, limit)?;
    let mut output = BudgetedText::try_with_capacity(budget, Category::Execution, limit, length)?;
    for value in chars() {
        output.try_push(
            value.map_err(|_| ColumnDecodeError::Unsupported("lone surrogate in NT value"))?,
        )?;
    }
    Ok(ColumnOwner::Text(output.into_string()?))
}
pub fn decode_column<'a>(
    value: &'a Value,
    ty: ColType,
    budget: &MemoryBudget,
    max_live_bytes: u64,
    reader: &mut impl FnMut(BlobRef, &MemoryBudget, u64) -> onecd::Result<Charged<Vec<u8>>>,
) -> Result<ColumnOwner<'a>, ColumnDecodeError> {
    if ty == ColType::RV || matches!(value, Value::RowVersion(_)) {
        return Err(ColumnDecodeError::Unsupported("row version column value"));
    }
    let view = match (value, ty) {
        (Value::Null, _) => ColumnView::Null,
        (Value::Char(s), ColType::NC(_)) | (Value::VarChar(s), ColType::NVC(_)) => {
            ColumnView::Str(s)
        }
        (Value::Numeric(n), ColType::N(..)) => ColumnView::Number(n),
        (Value::Bool(b), ColType::L) => ColumnView::Bool(*b),
        (Value::DateTime(d), ColType::DT) => ColumnView::DateTime(*d),
        (Value::Binary(bytes), ColType::B(_) | ColType::VB(_)) => ColumnView::Bytes(bytes),
        (Value::Image(BlobVal::Data(bytes)), ColType::I) => ColumnView::Bytes(bytes),
        (Value::Image(BlobVal::Ref(reference)), ColType::I) => {
            return Ok(ColumnOwner::Blob(read_blob(
                *reference,
                budget,
                max_live_bytes,
                reader,
            )?));
        }
        (Value::NText(BlobVal::Data(bytes)), ColType::NT) => {
            return text(bytes, bytes.len() as u64, budget, max_live_bytes);
        }
        (Value::NText(BlobVal::Ref(reference)), ColType::NT) => {
            let bytes = read_blob(*reference, budget, max_live_bytes, reader)?;
            return text(&bytes, bytes.reservation().bytes(), budget, max_live_bytes);
        }
        _ => return Err(ColumnDecodeError::Mismatch),
    };
    Ok(ColumnOwner::Borrowed(view))
}
