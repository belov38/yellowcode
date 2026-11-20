use super::{Error, Inventory, Item, ROW_BYTES, Result};
use brace::binary::{Atom, AtomValue, BinNode};
use memory_budget::{BudgetedText, Category, MemoryBudget};

fn atom(node: &BinNode) -> Result<&Atom> {
    match node {
        BinNode::Atom(a) => Ok(a),
        _ => Err(Error::Invalid("inventory scalar type")),
    }
}
fn number(node: &BinNode) -> Result<i64> {
    atom(node)?
        .as_int()
        .and_then(|n| i64::try_from(n).ok())
        .ok_or(Error::Invalid("inventory integer type"))
}
fn bounded_number(node: &BinNode, max: i64) -> Result<i64> {
    let value = number(node)?;
    if !(-max..=max).contains(&value) {
        return Err(Error::Invalid("inventory integer range"));
    }
    Ok(value)
}
fn name(node: &BinNode, budget: &MemoryBudget) -> Result<BudgetedText> {
    let a = atom(node)?;
    let units = match a.value() {
        AtomValue::Wide(v) => v.len(),
        AtomValue::Narrow(v) => v.len(),
        AtomValue::Int(0) if a.tag() == 1 => 0,
        AtomValue::Int(n) if a.tag() == 0x0b && u8::try_from(*n).is_ok() => 1,
        _ => return Err(Error::Invalid("inventory name type")),
    };
    if units > 255 {
        return Err(Error::Limit("extension name"));
    }
    let mut out = BudgetedText::new(budget, Category::Metadata, 1020)?;
    let mut append = |ch: char| -> Result<()> {
        out.try_push_str(ch.encode_utf8(&mut [0; 4]))?;
        Ok(())
    };
    match a.value() {
        AtomValue::Wide(v) => {
            for ch in char::decode_utf16(v.iter().copied()) {
                append(ch.map_err(|_| Error::Invalid("inventory name UTF-16"))?)?;
            }
        }
        AtomValue::Narrow(v) => {
            for byte in v {
                append(char::from(*byte))?;
            }
        }
        AtomValue::Int(0) if a.tag() == 1 => {}
        AtomValue::Int(n) => append(char::from(
            u8::try_from(*n).map_err(|_| Error::Invalid("inventory name type"))?,
        ))?,
        _ => return Err(Error::Invalid("inventory name type")),
    }
    Ok(out)
}
fn omitted_binary(node: &BinNode) -> Result<()> {
    let BinNode::List(parts) = node else {
        return Err(Error::Invalid("omitted binary shape"));
    };
    let (zero, parts) = parts
        .split_first()
        .ok_or(Error::Invalid("omitted binary shape"))?;
    if number(zero)? != 0 {
        return Err(Error::Invalid("omitted binary marker"));
    }
    let mut length = 0u64;
    for chunk in parts {
        let a = atom(chunk)?;
        let bytes = match a.value() {
            AtomValue::Narrow(v) => v.len() as u64,
            AtomValue::Int(0) if a.tag() == 1 => 0,
            AtomValue::Int(n) if a.tag() == 0x0b && u8::try_from(*n).is_ok() => 1,
            _ => return Err(Error::Invalid("omitted binary chunk")),
        };
        length = length
            .checked_add(bytes)
            .ok_or(Error::Limit("omitted inventory value"))?;
        if length > ROW_BYTES {
            return Err(Error::Limit("omitted inventory value"));
        }
    }
    Ok(())
}
fn omitted_text(node: &BinNode) -> Result<()> {
    let a = atom(node)?;
    let units = match a.value() {
        AtomValue::Wide(v) => v.len() as u64,
        AtomValue::Narrow(v) => v.len() as u64,
        AtomValue::Int(0) if a.tag() == 1 => 0,
        AtomValue::Int(n) if a.tag() == 0x0b && u8::try_from(*n).is_ok() => 1,
        _ => return Err(Error::Invalid("omitted text shape")),
    };
    if units.checked_mul(2).is_none_or(|n| n > ROW_BYTES) {
        return Err(Error::Limit("omitted inventory value"));
    }
    Ok(())
}
impl Inventory {
    pub fn push_dt_row(&mut self, row: &[BinNode]) -> Result<()> {
        if self.len() >= super::MAX_ITEMS {
            return Err(Error::Limit("extension count"));
        }
        let [
            f0,
            id,
            f1,
            order,
            f2,
            raw_name,
            f3,
            updated,
            f4,
            purpose,
            f5,
            scope,
            f6,
            zipped,
            f7,
            master,
            f8,
            distributed,
        ] = row
        else {
            return Err(Error::Invalid("inventory row shape"));
        };
        for flag in [f0, f1, f2, f3, f4, f5, f6, f7, f8] {
            if number(flag)? != 1 {
                return Err(Error::Invalid("NULL in nonnullable inventory field"));
            }
        }
        let id = atom(id)?
            .as_guid()
            .ok_or(Error::Invalid("inventory ID type"))?;
        let ticks = atom(updated)?
            .as_int()
            .and_then(|n| u64::try_from(n).ok())
            .ok_or(Error::Invalid("inventory calendar type"))?;
        let updated = ibschema::values::datetime_from_ticks(ticks)
            .map_err(|_| Error::Invalid("inventory calendar value"))?;
        let distributed = match number(distributed)? {
            0 => false,
            1 => true,
            _ => return Err(Error::Invalid("inventory distributed flag")),
        };
        omitted_binary(zipped)?;
        omitted_text(master)?;
        let order = bounded_number(order, 999_999_999)?;
        let purpose_code = bounded_number(purpose, 99)?;
        let scope_code = bounded_number(scope, 99)?;
        let item = Item {
            id,
            name: name(raw_name, self.items.reservation().budget())?,
            order,
            purpose_code,
            scope_code,
            update_time: (updated.year != 0).then_some(updated),
            used_in_distributed_infobase: distributed,
        };
        self.push(item)
    }
}

pub fn dt_generation(
    row: &[BinNode],
    id: brace::Guid,
    budget: &MemoryBudget,
) -> Result<Option<Option<[u8; 20]>>> {
    if row
        .get(1)
        .and_then(|value| atom(value).ok())
        .and_then(Atom::as_guid)
        != Some(id)
    {
        return Ok(None);
    }
    let Some(BinNode::List(parts)) = row.get(13) else {
        return Err(Error::Invalid("extension locator type"));
    };
    let mut value = memory_budget::BudgetedVec::new(budget, Category::ReaderWorking)?;
    for part in parts.iter().skip(1) {
        let a = atom(part)?;
        let single;
        let bytes = match a.value() {
            AtomValue::Narrow(v) => v.as_slice(),
            AtomValue::Int(0) if a.tag() == 1 => &[],
            AtomValue::Int(n) if a.tag() == 0x0b => {
                single = [u8::try_from(*n).map_err(|_| Error::Invalid("generation chunk"))?];
                &single
            }
            _ => return Err(Error::Invalid("generation chunk")),
        };
        if value.len().saturating_add(bytes.len()) > super::GENERATION_BYTES {
            return Err(Error::Limit("extension generation value"));
        }
        value.try_extend_from_slice(bytes)?;
    }
    super::generation_key(value.as_slice(), budget).map(Some)
}
