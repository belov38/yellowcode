#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid SQL record: {0}")]
    Invalid(&'static str),
    #[error("unsupported SQL record: {0}")]
    Unsupported(&'static str),
}

#[derive(Debug, PartialEq, Eq)]
pub struct Variable<'a> {
    pub data: &'a [u8],
    pub complex: bool,
}

#[derive(Debug)]
pub struct FixedVarRecord<'a> {
    pub status: u8,
    pub fixed: &'a [u8],
    pub column_count: usize,
    null_bitmap: &'a [u8],
    pub variables: Vec<Variable<'a>>,
}

fn part(bytes: &[u8], start: usize, len: usize) -> Result<&[u8], Error> {
    let end = start
        .checked_add(len)
        .ok_or(Error::Invalid("offset overflow"))?;
    bytes
        .get(start..end)
        .ok_or(Error::Invalid("truncated record"))
}

fn number(bytes: &[u8], start: usize) -> Result<u16, Error> {
    let pair: [u8; 2] = part(bytes, start, 2)?
        .try_into()
        .map_err(|_| Error::Invalid("short integer"))?;
    Ok(u16::from_le_bytes(pair))
}

impl<'a> FixedVarRecord<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        let status = *bytes.first().ok_or(Error::Invalid("missing status"))?;
        if status & 1 != 0 {
            return Err(Error::Unsupported("compressed record"));
        }
        let fixed_end = usize::from(number(bytes, 2)?);
        if fixed_end < 4 {
            return Err(Error::Invalid("fixed data ends before header"));
        }
        let fixed = part(bytes, 4, fixed_end - 4)?;
        let column_count = usize::from(number(bytes, fixed_end)?);
        let mut pos = fixed_end
            .checked_add(2)
            .ok_or(Error::Invalid("offset overflow"))?;
        let null_bitmap = if status & 0x10 != 0 {
            let bitmap_len = column_count.div_ceil(8);
            let bitmap = part(bytes, pos, bitmap_len)?;
            pos = pos
                .checked_add(bitmap_len)
                .ok_or(Error::Invalid("offset overflow"))?;
            bitmap
        } else {
            &[]
        };
        let mut variables = Vec::new();
        if status & 0x20 != 0 {
            let count = usize::from(number(bytes, pos)?);
            if count > column_count {
                return Err(Error::Invalid("variable count exceeds column count"));
            }
            pos = pos
                .checked_add(2)
                .ok_or(Error::Invalid("offset overflow"))?;
            let offsets_len = count
                .checked_mul(2)
                .ok_or(Error::Invalid("offset overflow"))?;
            part(bytes, pos, offsets_len)?;
            let mut previous = pos
                .checked_add(offsets_len)
                .ok_or(Error::Invalid("offset overflow"))?;
            variables
                .try_reserve(count)
                .map_err(|_| Error::Invalid("too many variable fields"))?;
            for i in 0..count {
                let at = pos
                    .checked_add(i.checked_mul(2).ok_or(Error::Invalid("offset overflow"))?)
                    .ok_or(Error::Invalid("offset overflow"))?;
                let end = number(bytes, at)?;
                let complex = end & 0x8000 != 0;
                let end = usize::from(end & 0x7fff);
                let len = end
                    .checked_sub(previous)
                    .ok_or(Error::Invalid("variable offsets are not ordered"))?;
                variables.push(Variable {
                    data: part(bytes, previous, len)?,
                    complex,
                });
                previous = end;
            }
        }
        Ok(Self {
            status,
            fixed,
            column_count,
            null_bitmap,
            variables,
        })
    }

    pub fn is_null(&self, column: usize) -> Option<bool> {
        if column >= self.column_count {
            return None;
        }
        if self.null_bitmap.is_empty() {
            return Some(false);
        }
        self.null_bitmap
            .get(column / 8)
            .map(|b| b & (1 << (column % 8)) != 0)
    }
}
