use super::collation::{CollationError, ManagedCollation};
use bsl_prim::Number;
use ibschema::values::type_code;
use memory_budget::{BudgetError, BudgetedVec, Category, MemoryBudget};
use query_ir::managed_value::ValueRef;
use std::cmp::Ordering;
use std::fmt::{self, Write};
use std::hash::{Hash, Hasher};

#[derive(Debug)]
pub struct ManagedKey(BudgetedVec<u8>);
impl ManagedKey {
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }
}
impl PartialEq for ManagedKey {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}
impl Eq for ManagedKey {}
impl PartialOrd for ManagedKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for ManagedKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.as_slice().cmp(other.as_slice())
    }
}
impl Hash for ManagedKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_slice().hash(state);
    }
}

pub fn try_key_of(
    value: ValueRef<'_>,
    collator: &ManagedCollation,
    budget: &MemoryBudget,
) -> Result<ManagedKey, CollationError> {
    collator.check_budget(budget)?;
    if let ValueRef::String(text) = value {
        let mut key = collator.try_sorting_key(text, budget)?;
        let len = key.len();
        key.try_push(0)?;
        key.as_mut_slice().copy_within(0..len, 1);
        *key.as_mut_slice()
            .first_mut()
            .ok_or(BudgetError::Overflow)? = type_code::STRING;
        return Ok(ManagedKey(key));
    }
    if let ValueRef::Number(number) = value {
        return number_key(number, budget)
            .map(ManagedKey)
            .map_err(Into::into);
    }
    if let ValueRef::Type(ty) = value {
        let mut count = Count(0);
        write!(count, "{ty:?}").map_err(|_| BudgetError::Overflow)?;
        let capacity = count.0.checked_add(1).ok_or(BudgetError::Overflow)?;
        let mut key = BudgetedVec::try_with_capacity(budget, Category::Execution, capacity)?;
        key.try_push(9)?;
        let mut output = KeyWriter {
            key: &mut key,
            error: None,
        };
        let result = write!(output, "{ty:?}");
        output.finish(result)?;
        return Ok(ManagedKey(key));
    }
    let (kind, bytes): (u8, usize) = match value {
        ValueRef::Null => (0, 0),
        ValueRef::Undefined => (type_code::UNDEFINED, 0),
        ValueRef::Bool(_) => (type_code::BOOL, 1),
        ValueRef::Date(_) => (type_code::DATE, 8),
        ValueRef::Uuid(_) => (type_code::UUID, 16),
        ValueRef::Ref { .. } => (type_code::REF, 20),
        ValueRef::Binary(bytes) => (11, bytes.len()),
        ValueRef::Storage(bytes) => (12, bytes.len()),
        ValueRef::SysEnum(ty, index) => (
            10,
            std::mem::size_of_val(&ty.0) + std::mem::size_of_val(&index),
        ),
        ValueRef::Nested(_) => (13, 0),
        ValueRef::String(_) | ValueRef::Number(_) | ValueRef::Type(_) => {
            return Err(BudgetError::Overflow.into());
        }
    };
    let capacity = bytes.checked_add(1).ok_or(BudgetError::Overflow)?;
    let mut key = BudgetedVec::try_with_capacity(budget, Category::Execution, capacity)?;
    key.try_push(kind)?;
    match value {
        ValueRef::Bool(value) => key.try_push(u8::from(value))?,
        ValueRef::Date(value) => {
            key.try_extend_from_slice(&((value.ticks() as u64) ^ (1 << 63)).to_be_bytes())?
        }
        ValueRef::Uuid(bytes) => key.try_extend_from_slice(bytes)?,
        ValueRef::Binary(bytes) | ValueRef::Storage(bytes) => key.try_extend_from_slice(bytes)?,
        ValueRef::Ref { table, id } => {
            key.try_extend_from_slice(&table.to_be_bytes())?;
            key.try_extend_from_slice(id)?;
        }
        ValueRef::SysEnum(ty, index) => {
            key.try_extend_from_slice(&ty.0.to_be_bytes())?;
            key.try_extend_from_slice(&index.to_be_bytes())?;
        }
        _ => {}
    }
    Ok(ManagedKey(key))
}

struct Count(usize);
impl Write for Count {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.0 = self.0.checked_add(text.len()).ok_or(fmt::Error)?;
        Ok(())
    }
}
struct KeyWriter<'a> {
    key: &'a mut BudgetedVec<u8>,
    error: Option<BudgetError>,
}
impl KeyWriter<'_> {
    fn finish(&self, result: fmt::Result) -> Result<(), BudgetError> {
        result.map_err(|_| self.error.unwrap_or(BudgetError::Overflow))
    }
}
impl Write for KeyWriter<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.key
            .try_extend_from_slice(text.as_bytes())
            .map_err(|error| {
                self.error = Some(error);
                fmt::Error
            })
    }
}

#[derive(Default)]
struct NumberShape {
    fraction: bool,
    started: bool,
    integer_digits: usize,
    fractional_zeroes: usize,
    digits: usize,
}
impl Write for NumberShape {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for byte in text.bytes() {
            match byte {
                b'-' => continue,
                b'.' => {
                    self.fraction = true;
                    continue;
                }
                _ => {}
            }
            self.started |= byte != b'0';
            if self.started {
                self.digits = self.digits.checked_add(1).ok_or(fmt::Error)?;
                if !self.fraction {
                    self.integer_digits = self.integer_digits.checked_add(1).ok_or(fmt::Error)?;
                }
            } else if self.fraction {
                self.fractional_zeroes = self.fractional_zeroes.checked_add(1).ok_or(fmt::Error)?;
            }
        }
        Ok(())
    }
}
struct NumberDigits<'a> {
    output: KeyWriter<'a>,
    started: bool,
    negative: bool,
}
impl Write for NumberDigits<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for byte in text.bytes() {
            if matches!(byte, b'-' | b'.') {
                continue;
            }
            self.started |= byte != b'0';
            if self.started {
                let digit = byte - b'0' + 1;
                self.output
                    .key
                    .try_push(if self.negative { !digit } else { digit })
                    .map_err(|error| {
                        self.output.error = Some(error);
                        fmt::Error
                    })?;
            }
        }
        Ok(())
    }
}
fn number_key(number: &Number, budget: &MemoryBudget) -> Result<BudgetedVec<u8>, BudgetError> {
    let mut shape = NumberShape::default();
    number
        .write_xml(&mut shape)
        .map_err(|_| BudgetError::Overflow)?;
    let capacity = if number.is_zero() {
        2
    } else {
        shape.digits.checked_add(7).ok_or(BudgetError::Overflow)?
    };
    let mut key = BudgetedVec::try_with_capacity(budget, Category::Execution, capacity)?;
    key.try_push(type_code::NUMBER)?;
    if number.is_zero() {
        key.try_push(2)?;
        return Ok(key);
    }
    let negative = number.is_negative();
    key.try_push(if negative { 1 } else { 3 })?;
    let exponent = if shape.integer_digits > 0 {
        i32::try_from(shape.integer_digits).unwrap_or(i32::MAX)
    } else {
        i32::try_from(shape.fractional_zeroes).map_or(i32::MIN, |zeroes| -zeroes)
    };
    for byte in ((exponent as u32) ^ 0x8000_0000).to_be_bytes() {
        key.try_push(if negative { !byte } else { byte })?;
    }
    let mut output = NumberDigits {
        output: KeyWriter {
            key: &mut key,
            error: None,
        },
        started: false,
        negative,
    };
    let result = number.write_xml(&mut output);
    output.output.finish(result)?;
    key.try_push(if negative { 255 } else { 0 })?;
    Ok(key)
}
