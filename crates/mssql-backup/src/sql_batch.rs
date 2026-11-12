use std::{io::Read, ops::Range};

use crate::sql_rows::{Error, Frames};

pub struct Table<'a> {
    pub index: u64,
    pub name: &'a str,
    pub columns: &'a [ibschema::columns::Column],
    pub actual: &'a [crate::sql_catalog::SqlColumn],
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_tables: usize,
    pub max_bytes: u64,
}

pub const LIMITS: Limits = Limits {
    max_tables: 1000,
    max_bytes: 256 * 1024 * 1024,
};

pub fn plan(estimates: &[u64], limits: Limits) -> Vec<Range<usize>> {
    let mut batches = Vec::new();
    let mut start = 0;
    let mut bytes = 0_u64;
    for (index, &estimate) in estimates.iter().enumerate() {
        if index > start
            && (index - start >= limits.max_tables
                || bytes.saturating_add(estimate) > limits.max_bytes)
        {
            batches.push(start..index);
            start = index;
            bytes = 0;
        }
        bytes = bytes.saturating_add(estimate);
    }
    if start < estimates.len() {
        batches.push(start..estimates.len());
    }
    batches
}

pub fn begin<R: Read>(frames: &mut Frames<R>, index: u64) -> Result<(), Error> {
    frames.expect(b"YC_T:")?;
    if frames.number()? != index {
        return Err(Error::Invalid("unexpected table in batch"));
    }
    Ok(())
}

pub fn end<R: Read>(frames: &mut Frames<R>, index: u64, rows: u64) -> Result<(), Error> {
    frames.expect(b"YC_T_END:")?;
    if frames.number()? != index {
        return Err(Error::Invalid("unexpected table in batch"));
    }
    frames.expect(b":")?;
    if frames.number()? != rows {
        return Err(Error::Invalid("row count mismatch"));
    }
    Ok(())
}
