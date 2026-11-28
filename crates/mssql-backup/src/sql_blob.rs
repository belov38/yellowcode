use std::{
    collections::HashSet,
    io::{BufWriter, Read, Seek, SeekFrom, Write},
};

const MAX_BLOB_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageRef {
    pub key: [u8; 16],
    pub size: u64,
}

pub struct BlobPart {
    pub key: [u8; 16],
    pub offset: u64,
    pub number: u64,
    pub data: Vec<u8>,
}

pub struct StorageAccounting {
    keys: HashSet<[u8; 16]>,
    max_keys: usize,
}

pub fn read_storage_count(input: impl Read) -> Result<u64, Error> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Count {
        rows: u64,
    }
    let mut data = Vec::new();
    input
        .take(8193)
        .read_to_end(&mut data)
        .map_err(|_| Error("storage count I/O"))?;
    if data.len() > 8192 {
        return Err(Error("storage count output limit exceeded"));
    }
    data.retain(|&b| b != b'\r' && b != b'\n');
    let count: Count = serde_json::from_slice(&data).map_err(|_| Error("invalid storage count"))?;
    Ok(count.rows)
}

impl StorageAccounting {
    pub fn new(max_keys: usize) -> Self {
        Self {
            keys: HashSet::new(),
            max_keys,
        }
    }

    pub fn resolve(
        &mut self,
        reference: &StorageRef,
        parts: Vec<BlobPart>,
    ) -> Result<Vec<u8>, Error> {
        if !self.keys.contains(&reference.key) && self.keys.len() >= self.max_keys {
            return Err(Error("storage key limit exceeded"));
        }
        let data = resolve_single_part(reference, parts)?;
        self.keys.insert(reference.key);
        Ok(data)
    }

    pub fn finish(&self, source_rows: u64) -> Result<(), Error> {
        if u64::try_from(self.keys.len()).ok() != Some(source_rows) {
            return Err(Error("unaccounted binary storage rows"));
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("unsupported or inconsistent SQL binary storage: {0}")]
pub struct Error(&'static str);

pub fn parse_reference(value: &[u8]) -> Result<Option<StorageRef>, Error> {
    if value.len() != 64 || !value.starts_with(b"STORHDR\0") {
        return Ok(None);
    }
    if value.get(8..16) != Some(&[0_u8; 8])
        || value.get(40..56) != Some(&[0xff_u8; 16])
        || value.get(56..64) != Some(&[0_u8; 8])
    {
        return Err(Error("unverified header or external storage"));
    }
    let key = value
        .get(16..32)
        .ok_or(Error("missing key"))?
        .try_into()
        .map_err(|_| Error("invalid key"))?;
    let size_bytes: [u8; 8] = value
        .get(32..40)
        .ok_or(Error("missing size"))?
        .try_into()
        .map_err(|_| Error("invalid size"))?;
    let size = u64::from_le_bytes(size_bytes);
    if size > MAX_BLOB_BYTES {
        return Err(Error("payload limit exceeded"));
    }
    Ok(Some(StorageRef { key, size }))
}

pub fn resolve_single_part(reference: &StorageRef, parts: Vec<BlobPart>) -> Result<Vec<u8>, Error> {
    if parts.len() != 1 {
        return Err(Error("expected exactly one part"));
    }
    let part = parts.into_iter().next().ok_or(Error("missing part"))?;
    if part.key != reference.key
        || part.offset != 0
        || part.number != 0
        || u64::try_from(part.data.len()).ok() != Some(reference.size)
    {
        return Err(Error("key, part or payload size mismatch"));
    }
    Ok(part.data)
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct WirePart {
    key: String,
    offset: String,
    number: String,
    data: String,
}

pub fn read_blob_parts(input: impl Read, reference: &StorageRef) -> Result<Vec<BlobPart>, Error> {
    if reference.size > MAX_BLOB_BYTES {
        return Err(Error("payload limit exceeded"));
    }
    let max_row = reference
        .size
        .checked_mul(2)
        .and_then(|n| n.checked_add(4096))
        .and_then(|n| usize::try_from(n).ok())
        .ok_or(Error("row limit overflow"))?;
    let mut parts = Vec::new();
    crate::sql_rows::read_rows(input, max_row, |row| {
        if !parts.is_empty() {
            return Err("multiple storage parts".into());
        }
        parts.push(wire_part(row)?);
        Ok(())
    })
    .map_err(|_| Error("invalid SQL storage output"))?;
    if parts.len() != 1 {
        return Err(Error("expected exactly one part"));
    }
    Ok(parts)
}

fn wire_part(row: &[u8]) -> Result<BlobPart, String> {
    let wire: WirePart = serde_json::from_slice(row).map_err(|e| e.to_string())?;
    let key: [u8; 16] = crate::sql_json_row::binary(&wire.key)
        .map_err(|e| e.to_string())?
        .try_into()
        .map_err(|_| "invalid storage key")?;
    let number = |s: &str| {
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            return Err("invalid nonnegative storage number".to_string());
        }
        s.parse::<u64>()
            .map_err(|_| "storage number out of range".to_string())
    };
    Ok(BlobPart {
        key,
        offset: number(&wire.offset)?,
        number: number(&wire.number)?,
        data: crate::sql_json_row::binary(&wire.data).map_err(|e| e.to_string())?,
    })
}

#[derive(Debug, thiserror::Error)]
pub enum SpoolError {
    #[error(transparent)]
    Storage(#[from] Error),
    #[error("reading SQL binary storage: {0}")]
    Frame(#[from] crate::sql_rows::Error),
    #[error("binary storage staging file: {0}")]
    Io(#[from] std::io::Error),
    #[error("binary storage index: {0}")]
    Budget(#[from] memory_budget::BudgetError),
}

#[derive(Clone, Copy)]
struct Stored {
    key: [u8; 16],
    offset: u64,
    number: u64,
    at: u64,
    len: u64,
}

pub fn index_bytes(rows: u64) -> Option<u64> {
    let entries = rows.checked_mul(std::mem::size_of::<Stored>() as u64)?;
    entries.checked_add(rows.div_ceil(64).checked_mul(8)?)
}

#[derive(Debug)]
pub struct IndexRefusal {
    pub rows: u64,
    pub need: u64,
    pub budget: u64,
    pub fallback: bool,
}

impl IndexRefusal {
    pub fn limit_for(budget: u64) -> u64 {
        budget / 2
    }
    pub fn suggested_mib(&self) -> u64 {
        self.need.saturating_mul(2).div_ceil(1 << 20).max(1)
    }
}

impl std::fmt::Display for IndexRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mib = |bytes: u64| bytes.div_ceil(1 << 20);
        write!(
            f,
            "SQL binary storage memory limit: the storage table has {} rows, their index needs {} MiB, more than half of the memory budget of {} MiB",
            self.rows,
            mib(self.need),
            mib(self.budget)
        )?;
        if self.fallback {
            write!(
                f,
                "; the memory of this machine was not determined, so the budget is the fixed fallback: set --memory-limit {}MiB or more",
                self.suggested_mib()
            )
        } else {
            write!(
                f,
                "; repeat with --memory-limit {}MiB or more",
                self.suggested_mib()
            )
        }
    }
}

impl std::error::Error for IndexRefusal {}

pub struct Spool {
    file: std::fs::File,
    index: memory_budget::BudgetedVec<Stored>,
    resolved: memory_budget::BudgetedVec<u64>,
    rows: u64,
}

pub fn read_spool(
    input: impl Read,
    directory: &std::path::Path,
    expected: u64,
    budget: &memory_budget::MemoryBudget,
) -> Result<Spool, SpoolError> {
    let capacity = usize::try_from(expected).map_err(|_| Error("storage row count overflow"))?;
    let words =
        usize::try_from(expected.div_ceil(64)).map_err(|_| Error("storage row count overflow"))?;
    let category = memory_budget::Category::Metadata;
    let mut index = memory_budget::BudgetedVec::try_with_capacity(budget, category, capacity)?;
    let mut resolved = memory_budget::BudgetedVec::try_with_capacity(budget, category, words)?;
    resolved.try_resize(words, 0)?;
    let mut file = tempfile::tempfile_in(directory)?;
    let mut frames = crate::sql_rows::Frames::new(input);
    crate::sql_batch::begin(&mut frames, 1)?;
    let max_row = MAX_BLOB_BYTES
        .checked_mul(2)
        .and_then(|n| n.checked_add(4096))
        .and_then(|n| usize::try_from(n).ok())
        .ok_or(Error("row limit overflow"))?;
    let mut writer = BufWriter::new(&mut file);
    let mut at = 0_u64;
    let rows = frames.rows(max_row, |row| {
        if index.len() >= capacity {
            return Err(Error("more storage rows than the storage count").to_string());
        }
        let part = wire_part(row)?;
        let len = u64::try_from(part.data.len()).map_err(|_| "payload size overflow")?;
        writer
            .write_all(&part.data)
            .map_err(|e| format!("binary storage staging file: {e}"))?;
        index
            .try_push(Stored {
                key: part.key,
                offset: part.offset,
                number: part.number,
                at,
                len,
            })
            .map_err(|e| e.to_string())?;
        at += len;
        Ok(())
    })?;
    crate::sql_batch::end(&mut frames, 1, rows)?;
    frames.end()?;
    writer.flush()?;
    drop(writer);
    index
        .as_mut_slice()
        .sort_unstable_by_key(|stored| (stored.key, stored.at));
    Ok(Spool {
        file,
        index,
        resolved,
        rows,
    })
}

impl Spool {
    pub fn rows(&self) -> u64 {
        self.rows
    }

    fn range(&self, key: &[u8; 16]) -> std::ops::Range<usize> {
        let index = self.index.as_slice();
        let start = index.partition_point(|stored| stored.key < *key);
        let end = start
            + index
                .get(start..)
                .unwrap_or_default()
                .partition_point(|stored| stored.key == *key);
        start..end
    }

    pub fn parts(&mut self, reference: &StorageRef) -> Result<Vec<BlobPart>, SpoolError> {
        if reference.size > MAX_BLOB_BYTES {
            return Err(Error("payload limit exceeded").into());
        }
        let mut parts = Vec::new();
        let range = self.range(&reference.key);
        for &stored in self.index.get(range).unwrap_or_default() {
            let mut data =
                vec![0; usize::try_from(stored.len).map_err(|_| Error("payload limit exceeded"))?];
            self.file.seek(SeekFrom::Start(stored.at))?;
            self.file.read_exact(&mut data)?;
            parts.push(BlobPart {
                key: reference.key,
                offset: stored.offset,
                number: stored.number,
                data,
            });
        }
        Ok(parts)
    }

    pub fn resolve(&mut self, reference: &StorageRef) -> Result<Vec<u8>, SpoolError> {
        let parts = self.parts(reference)?;
        let data = resolve_single_part(reference, parts)?;
        let position = self.range(&reference.key).start;
        let word = self
            .resolved
            .get_mut(position / 64)
            .ok_or(Error("storage row outside the index"))?;
        *word |= 1 << (position % 64);
        Ok(data)
    }

    pub fn finish(&self, source_rows: u64) -> Result<(), Error> {
        let resolved: u64 = self
            .resolved
            .iter()
            .map(|word| u64::from(word.count_ones()))
            .sum();
        if self.rows != source_rows || resolved != source_rows {
            return Err(Error("unaccounted binary storage rows"));
        }
        Ok(())
    }
}
