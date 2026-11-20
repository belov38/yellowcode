use crate::{Error, Result};
use memory_budget::{
    BudgetError, BudgetedVec, Category, Charged, LimitReason, MemoryBudget, MemoryLimit,
};
use onecd::{BlobVal, ReadTxn, RowId, TableRead, Value};

fn bad(what: &'static str) -> Error {
    Error::InconsistentStatic(what)
}
fn check_ledger(txn: &ReadTxn, budget: &MemoryBudget) -> Result<()> {
    if !txn
        .memory_budget()
        .is_some_and(|ledger| ledger.same_instance(budget))
    {
        return Err(Error::UnsupportedStatic(
            "managed metadata reads require the transaction's memory ledger",
        ));
    }
    Ok(())
}
fn value_limit(bytes: usize, limit: u64) -> Result<()> {
    let requested = bytes
        .checked_add(std::mem::size_of::<BudgetedVec<u8>>())
        .ok_or(BudgetError::Overflow)?;
    let requested = u64::try_from(requested).map_err(|_| BudgetError::Overflow)?;
    if requested > limit {
        return Err(BudgetError::Limit(MemoryLimit {
            category: Category::Metadata,
            requested,
            reason: LimitReason::Category,
        })
        .into());
    }
    Ok(())
}
pub(crate) fn column(table: &TableRead<'_>, name: &str) -> Result<usize> {
    table
        .descr()
        .field(name)
        .map(|(i, _)| i)
        .ok_or_else(|| bad("missing service metadata column"))
}
pub(crate) fn partno(value: Option<&Value>) -> Result<u64> {
    let Some(Value::Numeric(number)) = value else {
        return Err(bad("PARTNO is not numeric"));
    };
    if number.scale() != 0 || (number.is_negative() && number.digits().iter().any(|&d| d != 0)) {
        return Err(bad("invalid PARTNO"));
    }
    number.digits().iter().try_fold(0u64, |acc, &digit| {
        acc.checked_mul(10)
            .and_then(|acc| acc.checked_add(u64::from(digit)))
            .ok_or_else(|| bad("PARTNO overflow"))
    })
}
fn data_len(value: Option<&Value>) -> Result<usize> {
    match value {
        Some(
            Value::Binary(bytes)
            | Value::Image(BlobVal::Data(bytes))
            | Value::Text(BlobVal::Data(bytes)),
        ) => Ok(bytes.len()),
        Some(Value::Image(BlobVal::Ref(reference)) | Value::Text(BlobVal::Ref(reference))) => {
            usize::try_from(reference.len).map_err(|_| BudgetError::Overflow.into())
        }
        _ => Err(bad("service metadata value is not binary")),
    }
}
pub(crate) fn folder_data_len(value: Option<&Value>) -> Result<usize> {
    match value {
        Some(Value::Image(_) | Value::Binary(_)) => data_len(value),
        _ => Err(bad("BINARYDATA is not an image or binary value")),
    }
}
fn schema_data_len(value: Option<&Value>) -> Result<usize> {
    match value {
        Some(Value::Image(_) | Value::Text(_)) => data_len(value),
        _ => Err(bad("SERIALIZEDDATA is not an image or text value")),
    }
}
fn names_equal(first: &str, second: &str) -> bool {
    first
        .chars()
        .flat_map(char::to_lowercase)
        .eq(second.chars().flat_map(char::to_lowercase))
}
pub(crate) fn append(
    table: &TableRead<'_>,
    value: Option<&Value>,
    output: &mut BudgetedVec<u8>,
    budget: &MemoryBudget,
    limit: u64,
) -> Result<()> {
    match value {
        Some(
            Value::Binary(bytes)
            | Value::Image(BlobVal::Data(bytes))
            | Value::Text(BlobVal::Data(bytes)),
        ) => output.try_extend_from_slice(bytes)?,
        Some(Value::Image(BlobVal::Ref(reference)) | Value::Text(BlobVal::Ref(reference))) => {
            let bytes = table.blob_bytes_managed(*reference, budget, Category::Metadata, limit)?;
            output.try_extend_from_slice(&bytes)?;
        }
        _ => return Err(bad("service metadata value is not binary")),
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct Part {
    number: u64,
    slot: RowId,
    bytes: usize,
}

pub fn folder_file_data_in_budgeted(
    txn: &ReadTxn,
    table: &str,
    name: &str,
    budget: &MemoryBudget,
    limit: u64,
) -> Result<Option<Charged<Vec<u8>>>> {
    check_ledger(txn, budget)?;
    let table = txn
        .find_table(table)?
        .ok_or_else(|| bad("missing service metadata table"))?;
    let name_column = column(&table, "FILENAME")?;
    let data_column = column(&table, "BINARYDATA")?;
    let part_column = table.descr().field("PARTNO").map(|(i, _)| i);
    let mut parts = BudgetedVec::<Part>::new(budget, Category::Metadata)?;
    let mut first_name: Option<BudgetedVec<u8>> = None;
    let mut total = 0usize;
    for slot in 1..table.num_slots() {
        let Some(row) = table.get_managed(slot, budget, Category::Metadata, limit)? else {
            continue;
        };
        let Some(Value::Char(found) | Value::VarChar(found)) = row.get(name_column) else {
            return Err(bad("FILENAME is not text"));
        };
        if !names_equal(found, name) {
            continue;
        }
        if let Some(first) = &first_name {
            if first.as_slice() != found.as_bytes() {
                return Err(bad("service file names differ only in case"));
            }
        } else {
            let mut first =
                BudgetedVec::try_with_capacity(budget, Category::Metadata, found.len())?;
            first.try_extend_from_slice(found.as_bytes())?;
            first_name = Some(first);
        }
        let number = part_column.map_or(Ok(0), |i| partno(row.get(i)))?;
        let bytes = folder_data_len(row.get(data_column))?;
        total = total.checked_add(bytes).ok_or(BudgetError::Overflow)?;
        value_limit(total, limit)?;
        parts.try_push(Part {
            number,
            slot,
            bytes,
        })?;
    }
    if parts.is_empty() {
        return Ok(None);
    }
    parts
        .as_mut_slice()
        .sort_unstable_by_key(|part| part.number);
    if parts
        .windows(2)
        .any(|pair| pair.first().map(|p| p.number) == pair.get(1).map(|p| p.number))
    {
        return Err(bad("repeated service metadata PARTNO"));
    }
    drop(first_name);
    let mut output = BudgetedVec::try_with_capacity(budget, Category::Metadata, total)?;
    for part in parts.as_slice() {
        let row = table
            .get_managed(part.slot, budget, Category::Metadata, limit)?
            .ok_or_else(|| bad("service metadata part vanished"))?;
        if folder_data_len(row.get(data_column))? != part.bytes {
            return Err(bad("service metadata part length changed"));
        }
        append(&table, row.get(data_column), &mut output, budget, limit)?;
    }
    Ok(Some(output.into_charged_vec()))
}

pub fn dbnames_text_in_budgeted(
    txn: &ReadTxn,
    budget: &MemoryBudget,
    limit: u64,
) -> Result<Charged<Vec<u8>>> {
    let stored = folder_file_data_in_budgeted(txn, "PARAMS", "DBNames", budget, limit)?
        .ok_or_else(|| bad("PARAMS has no DBNames"))?;
    Ok(zraw::inflate_budgeted(
        &stored,
        limit,
        budget,
        Category::Metadata,
    )?)
}

pub fn dbschema_text_in_budgeted(
    txn: &ReadTxn,
    budget: &MemoryBudget,
    limit: u64,
) -> Result<Charged<Vec<u8>>> {
    dbschema_text_in_budgeted_checked(txn, budget, limit, &|| Ok(()))
}

pub(crate) fn dbschema_text_in_budgeted_checked(
    txn: &ReadTxn,
    budget: &MemoryBudget,
    limit: u64,
    before_slot: &dyn Fn() -> Result<()>,
) -> Result<Charged<Vec<u8>>> {
    check_ledger(txn, budget)?;
    let table = txn
        .find_table("DBSCHEMA")?
        .ok_or_else(|| bad("missing service metadata table"))?;
    let data_column = column(&table, "SERIALIZEDDATA")?;
    let mut only = None;
    for slot in 1..table.num_slots() {
        before_slot()?;
        if let Some(row) = table.get_managed(slot, budget, Category::Metadata, limit)? {
            if only.is_some() {
                return Err(bad("DBSCHEMA has more than one row"));
            }
            only = Some(row);
        }
    }
    let row = only.ok_or_else(|| bad("DBSCHEMA has no rows"))?;
    let total = schema_data_len(row.get(data_column))?;
    value_limit(total, limit)?;
    let mut output = BudgetedVec::try_with_capacity(budget, Category::Metadata, total)?;
    append(&table, row.get(data_column), &mut output, budget, limit)?;
    Ok(output.into_charged_vec())
}

pub(crate) fn schema_storage_main_in_budgeted(
    txn: &ReadTxn,
    budget: &MemoryBudget,
    limit: u64,
) -> Result<Option<(Charged<Vec<u8>>, bool)>> {
    schema_storage_main_in_budgeted_checked(txn, budget, limit, &|| Ok(()))
}

pub(crate) fn schema_storage_main_in_budgeted_checked(
    txn: &ReadTxn,
    budget: &MemoryBudget,
    limit: u64,
    before_slot: &dyn Fn() -> Result<()>,
) -> Result<Option<(Charged<Vec<u8>>, bool)>> {
    let Some((main, extension)) = storage_value_in_budgeted(txn, budget, limit, before_slot, 0)?
    else {
        return Ok(None);
    };
    let main = main.ok_or_else(|| bad("SCHEMASTORAGE: missing main SCHEMAID0"))?;
    Ok(Some((main, extension)))
}

pub(crate) fn schema_storage_extension_in_budgeted(
    txn: &ReadTxn,
    budget: &MemoryBudget,
    limit: u64,
) -> Result<Option<Charged<Vec<u8>>>> {
    Ok(storage_value_in_budgeted(txn, budget, limit, &|| Ok(()), 1)?.and_then(|(value, _)| value))
}

type StorageValue = (Option<Charged<Vec<u8>>>, bool);

fn storage_value_in_budgeted(
    txn: &ReadTxn,
    budget: &MemoryBudget,
    limit: u64,
    before_slot: &dyn Fn() -> Result<()>,
    want: u32,
) -> Result<Option<StorageValue>> {
    check_ledger(txn, budget)?;
    let Some(table) = txn.find_table("SCHEMASTORAGE")? else {
        return Ok(None);
    };
    let columns = super::StorageColumns::new(table.descr())?;
    let mut shape = super::StorageRows::default();
    let mut wanted = None;
    for slot in 1..table.num_slots() {
        before_slot()?;
        if let Some(row) = table.get_managed(slot, budget, Category::Metadata, limit)? {
            if !shape.admit() {
                break;
            }
            let id = shape.observe(&columns, &row, limit)?;
            if id == want && wanted.is_none() {
                wanted = Some(row);
            }
        }
    }
    let extension = shape.finish(&columns)?;
    let Some(row) = wanted else {
        return Ok(Some((None, extension)));
    };
    let total = super::storage_len(row.get(columns.current), "CURRENTSCHEMA", limit)?;
    value_limit(total, limit)?;
    let mut output = BudgetedVec::try_with_capacity(budget, Category::Metadata, total)?;
    append(&table, row.get(columns.current), &mut output, budget, limit)?;
    Ok(Some((Some(output.into_charged_vec()), extension)))
}

pub fn has_rows_in_budgeted(txn: &ReadTxn, table: &str, budget: &MemoryBudget) -> Result<bool> {
    check_ledger(txn, budget)?;
    let Some(table) = txn.find_table(table)? else {
        return Ok(false);
    };
    for slot in 1..table.num_slots() {
        if table
            .get_managed(slot, budget, Category::Metadata, u64::MAX)?
            .is_some()
        {
            return Ok(true);
        }
    }
    Ok(false)
}
