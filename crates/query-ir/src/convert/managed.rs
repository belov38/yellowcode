use crate::managed_value::{ManagedValue, NumberValueError, ScalarValue};
use ibschema::field_view::{FieldNumber, FieldView};
use memory_budget::{BudgetError, MemoryBudget};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldConversionError {
    Memory(BudgetError),
    InvalidNumber,
}
impl From<BudgetError> for FieldConversionError {
    fn from(error: BudgetError) -> Self {
        Self::Memory(error)
    }
}
impl std::fmt::Display for FieldConversionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Memory(error) => error.fmt(f),
            Self::InvalidNumber => f.write_str("stored number cannot be represented"),
        }
    }
}
impl std::error::Error for FieldConversionError {}
fn number(
    negative: bool,
    digits: &[u8],
    scale: u32,
    budget: &MemoryBudget,
) -> Result<ManagedValue, FieldConversionError> {
    ManagedValue::try_number_from_decimal_digits(negative, digits, scale, budget)
        .map_err(|error| match error {
            NumberValueError::Budget(error) => FieldConversionError::Memory(error),
            NumberValueError::DivByZero => FieldConversionError::InvalidNumber,
        })?
        .ok_or(FieldConversionError::InvalidNumber)
}
pub fn try_field_to_q(
    value: FieldView<'_>,
    budget: &MemoryBudget,
) -> Result<ManagedValue, FieldConversionError> {
    let scalar = match value {
        FieldView::Null => ScalarValue::Null,
        FieldView::Undefined => ScalarValue::Undefined,
        FieldView::Bool(value) => ScalarValue::Bool(value),
        FieldView::Date(value) => ScalarValue::Date(super::date_to_q(&value)),
        FieldView::Uuid(value) => ScalarValue::Uuid(*value.as_bytes()),
        FieldView::Ref { table, id } => ScalarValue::Ref { table, id: id.0 },
        FieldView::String(value) => return Ok(ManagedValue::try_string(value, budget)?),
        FieldView::Binary(value) => return Ok(ManagedValue::try_binary(value, budget)?),
        FieldView::Storage(value) => return Ok(ManagedValue::try_storage(value, budget)?),
        FieldView::Number(FieldNumber::Decimal(value)) => {
            return number(value.is_negative(), value.digits(), value.scale(), budget);
        }
        FieldView::Number(FieldNumber::Unsigned(mut value)) => {
            let mut digits = [0; 10];
            let mut start = digits.len();
            loop {
                start -= 1;
                if let Some(digit) = digits.get_mut(start) {
                    *digit = (value % 10) as u8;
                }
                value /= 10;
                if value == 0 {
                    break;
                }
            }
            return number(false, digits.get(start..).unwrap_or(&[]), 0, budget);
        }
        FieldView::RowVersion(words) => {
            let mut bytes = [0; 16];
            for (slot, word) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(words) {
                *slot = word.to_be_bytes();
            }
            return Ok(ManagedValue::try_binary(&bytes, budget)?);
        }
    };
    Ok(ManagedValue::scalar(scalar))
}

pub fn try_row_to_q(
    table: &ibschema::storage::TableInfo,
    values: &[FieldView<'_>],
    budget: &MemoryBudget,
) -> Result<crate::managed_value::ManagedRow, FieldConversionError> {
    let mut output = memory_budget::BudgetedVec::try_with_capacity(
        budget,
        memory_budget::Category::Execution,
        values.len(),
    )?;
    for (index, value) in values.iter().copied().enumerate() {
        output.try_push(try_table_field_to_q(table, index, value, budget)?)?;
    }
    Ok(output)
}
pub fn try_table_field_to_q(
    table: &ibschema::storage::TableInfo,
    index: usize,
    value: FieldView<'_>,
    budget: &MemoryBudget,
) -> Result<ManagedValue, FieldConversionError> {
    use ibschema::{storage::FieldId, values::RefId};
    match value {
        FieldView::Uuid(uuid) => {
            let little = u16::try_from(index)
                .ok()
                .and_then(|index| super::binary16(table, FieldId(index)));
            match little {
                Some(true) => Ok(ManagedValue::try_binary(&uuid.to_bytes_le(), budget)?),
                Some(false) => Ok(ManagedValue::try_binary(&RefId::from_uuid(uuid).0, budget)?),
                None => try_field_to_q(value, budget),
            }
        }
        FieldView::RowVersion(words) => Ok(data_version(words, budget)?),
        _ => try_field_to_q(value, budget),
    }
}

fn data_version(words: [u32; 4], budget: &MemoryBudget) -> Result<ManagedValue, BudgetError> {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut bytes = [0; 8];
    let [_, _, first_word, second_word] = words;
    let (first, second) = bytes.split_at_mut(4);
    first.copy_from_slice(&first_word.to_be_bytes());
    second.copy_from_slice(&second_word.to_be_bytes());
    let mut output = [u16::from(b'='); 12];
    for (input, target) in bytes.chunks(3).zip(output.as_chunks_mut::<4>().0) {
        let number = input
            .iter()
            .enumerate()
            .fold(0u32, |number, (index, byte)| {
                number | u32::from(*byte) << (16 - 8 * index)
            });
        for (index, target) in target.iter_mut().enumerate().take(input.len() + 1) {
            *target = u16::from(
                ABC.get(((number >> (18 - 6 * index)) & 63) as usize)
                    .copied()
                    .unwrap_or(b'A'),
            );
        }
    }
    ManagedValue::try_utf16(&output, budget)
}
