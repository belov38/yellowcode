const PAGE_SIZE: usize = 8192;
const HEADER_SIZE: usize = 96;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid SQL page: {0}")]
    Invalid(&'static str),
    #[error("unsupported SQL page: {0}")]
    Unsupported(&'static str),
}

fn part(bytes: &[u8], offset: usize, size: usize) -> Result<&[u8], Error> {
    let end = offset
        .checked_add(size)
        .ok_or(Error::Invalid("offset overflow"))?;
    bytes
        .get(offset..end)
        .ok_or(Error::Invalid("truncated page"))
}

fn u16_at(bytes: &[u8], offset: usize) -> Result<u16, Error> {
    let value = part(bytes, offset, 2)?
        .try_into()
        .map_err(|_| Error::Invalid("short integer"))?;
    Ok(u16::from_le_bytes(value))
}

pub struct Page<'a> {
    bytes: &'a [u8],
    page_type: u8,
    file_id: u16,
    page_id: u32,
}

pub struct RawSlot<'a> {
    pub ordinal: usize,
    pub offset: usize,
    pub upper_bound: usize,
    pub bytes: &'a [u8],
}

impl<'a> Page<'a> {
    pub fn raw_slots(&self) -> Result<Vec<RawSlot<'a>>, Error> {
        if !matches!(self.page_type, 1 | 2) {
            return Err(Error::Unsupported("page is not data or index"));
        }
        let count = usize::from(u16_at(self.bytes, 22)?);
        let directory_len = count
            .checked_mul(2)
            .ok_or(Error::Invalid("slot count overflow"))?;
        let directory_start = PAGE_SIZE
            .checked_sub(directory_len)
            .ok_or(Error::Invalid("slot directory exceeds page"))?;
        if directory_start < HEADER_SIZE {
            return Err(Error::Invalid("slot directory overlaps header"));
        }
        let free_data = usize::from(u16_at(self.bytes, 30)?);
        if !(HEADER_SIZE..=directory_start).contains(&free_data) {
            return Err(Error::Invalid("free-data boundary outside record area"));
        }
        let mut slots = Vec::new();
        let mut physical = Vec::new();
        slots
            .try_reserve_exact(count)
            .map_err(|_| Error::Invalid("slot allocation failed"))?;
        physical
            .try_reserve_exact(count)
            .map_err(|_| Error::Invalid("slot allocation failed"))?;
        for ordinal in 0..count {
            let at = PAGE_SIZE - 2 * (ordinal + 1);
            let offset = usize::from(u16_at(self.bytes, at)?);
            if !(HEADER_SIZE..free_data).contains(&offset) {
                return Err(Error::Invalid("slot offset outside record area"));
            }
            slots.push(RawSlot {
                ordinal,
                offset,
                upper_bound: 0,
                bytes: &[],
            });
            physical.push((offset, ordinal));
        }
        physical.sort_unstable();
        for (index, &(offset, ordinal)) in physical.iter().enumerate() {
            let end = physical.get(index + 1).map_or(free_data, |next| next.0);
            if end <= offset {
                return Err(Error::Invalid("duplicate slot offset"));
            }
            let slot = slots
                .get_mut(ordinal)
                .ok_or(Error::Invalid("slot ordinal outside directory"))?;
            slot.upper_bound = end;
            slot.bytes = part(self.bytes, offset, end - offset)?;
        }
        Ok(slots)
    }

    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        if bytes.len() != PAGE_SIZE {
            return Err(Error::Invalid("page size must be 8192 bytes"));
        }
        if bytes.first() != Some(&1) {
            return Err(Error::Unsupported("unknown page-header version"));
        }
        let page_type = *bytes.get(1).ok_or(Error::Invalid("missing page type"))?;
        let file_id = u16_at(bytes, 36)?;
        let id = part(bytes, 32, 4)?
            .try_into()
            .map_err(|_| Error::Invalid("short page id"))?;
        Ok(Self {
            bytes,
            page_type,
            file_id,
            page_id: u32::from_le_bytes(id),
        })
    }

    pub fn page_type(&self) -> u8 {
        self.page_type
    }
    pub fn file_id(&self) -> u16 {
        self.file_id
    }
    pub fn page_id(&self) -> u32 {
        self.page_id
    }

    pub fn allocation_bitmap(&self) -> Result<Bitmap<'a>, Error> {
        if !matches!(self.page_type, 8 | 9 | 16 | 17) {
            return Err(Error::Unsupported("page is not an allocation bitmap"));
        }
        if u16_at(self.bytes, 22)? != 2 {
            return Err(Error::Invalid("allocation page must have two slots"));
        }
        let slot_start = PAGE_SIZE - 4;
        let offset = usize::from(u16_at(self.bytes, slot_start)?);
        if offset < HEADER_SIZE || offset.checked_add(4).is_none_or(|end| end > slot_start) {
            return Err(Error::Invalid("allocation record offset outside page"));
        }
        let len = usize::from(u16_at(self.bytes, offset + 2)?);
        let start = offset + 4;
        if start.checked_add(len).is_none_or(|end| end > slot_start) {
            return Err(Error::Invalid("allocation bitmap overlaps slot array"));
        }
        Ok(Bitmap {
            bytes: part(self.bytes, start, len)?,
        })
    }
}

pub struct Bitmap<'a> {
    bytes: &'a [u8],
}

impl Bitmap<'_> {
    pub fn bit(&self, index: usize) -> Option<bool> {
        self.bytes
            .get(index / 8)
            .map(|byte| byte & (1 << (index % 8)) != 0)
    }
}
