use std::{collections::HashSet, io::Read};

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
        parts.push(BlobPart {
            key,
            offset: number(&wire.offset)?,
            number: number(&wire.number)?,
            data: crate::sql_json_row::binary(&wire.data).map_err(|e| e.to_string())?,
        });
        Ok(())
    })
    .map_err(|_| Error("invalid SQL storage output"))?;
    if parts.len() != 1 {
        return Err(Error("expected exactly one part"));
    }
    Ok(parts)
}
