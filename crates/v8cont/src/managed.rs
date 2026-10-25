use crate::{EntryInfo, FileHeader, Format, PageHeader, ReadAt, V8Time, detect};
use memory_budget::{BudgetError, BudgetedVec, Category, Charged, MemoryBudget};

#[derive(Debug, thiserror::Error)]
pub enum ManagedReadError {
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("not a v8 container")]
    NotContainer,
    #[error("corrupt container at 0x{offset:x}: {what}")]
    Corrupt { offset: u64, what: &'static str },
    #[error("{what} {size} exceeds the limit {limit}")]
    TooLarge {
        what: &'static str,
        size: u64,
        limit: u64,
    },
    #[error("{0}")]
    Memory(#[from] BudgetError),
}
type Result<T> = std::result::Result<T, ManagedReadError>;
fn corrupt(offset: u64, what: &'static str) -> ManagedReadError {
    ManagedReadError::Corrupt { offset, what }
}

#[derive(Debug)]
pub struct ManagedContainer<'a, R: ReadAt> {
    src: R,
    format: Format,
    header: FileHeader,
    size: u64,
    entries: Charged<Vec<EntryInfo>>,
    budget: &'a MemoryBudget,
    category: Category,
}
impl<'a, R: ReadAt> ManagedContainer<'a, R> {
    pub fn open(src: R, budget: &'a MemoryBudget, category: Category) -> Result<Self> {
        let format = detect(&src)?.ok_or(ManagedReadError::NotContainer)?;
        let size = src.size()?;
        let mut bytes = [0u8; 20];
        let bytes = bytes
            .get_mut(..format.file_header_size() as usize)
            .unwrap_or_default();
        src.read_exact_at(bytes, format.base())?;
        let header = FileHeader::parse(format, bytes)
            .ok_or_else(|| corrupt(format.base(), "file header"))?;
        let reader = Reader {
            src: &src,
            format,
            size,
            budget,
            category,
        };
        let entries =
            if size >= format.base() + format.file_header_size() + format.page_header_size() {
                reader.entries()?
            } else {
                BudgetedVec::new(budget, category)?.into_charged_vec()
            };
        Ok(Self {
            src,
            format,
            header,
            size,
            entries,
            budget,
            category,
        })
    }
    pub fn format(&self) -> Format {
        self.format
    }
    pub fn header(&self) -> &FileHeader {
        &self.header
    }
    pub fn entries(&self) -> &[EntryInfo] {
        &self.entries
    }
    pub fn find(&self, name: &str) -> Option<&EntryInfo> {
        self.entries.iter().find(|entry| entry.name == name)
    }
    pub fn file_len(&self) -> u64 {
        self.size
    }
    pub fn read_data(&self, entry: &EntryInfo, limit: u64) -> Result<Charged<Vec<u8>>> {
        let Some(addr) = entry.data_addr else {
            return Ok(BudgetedVec::new(self.budget, self.category)?.into_charged_vec());
        };
        Reader {
            src: &self.src,
            format: self.format,
            size: self.size,
            budget: self.budget,
            category: self.category,
        }
        .chain(addr, limit)
        .map(|(bytes, _)| bytes)
    }
}

struct Reader<'a, R: ReadAt> {
    src: &'a R,
    format: Format,
    size: u64,
    budget: &'a MemoryBudget,
    category: Category,
}
impl<R: ReadAt> Reader<'_, R> {
    fn page_header(&self, addr: u64) -> Result<PageHeader> {
        #[cfg(feature = "testing")]
        crate::testing::probe::hit();
        let size = self.format.page_header_size();
        let at = self
            .format
            .base()
            .checked_add(addr)
            .filter(|at| at.checked_add(size).is_some_and(|end| end <= self.size))
            .ok_or_else(|| corrupt(addr, "page beyond the end of the file"))?;
        let mut bytes = [0u8; 55];
        let bytes = bytes.get_mut(..size as usize).unwrap_or_default();
        self.src.read_exact_at(bytes, at)?;
        PageHeader::parse(self.format, bytes).ok_or_else(|| corrupt(at, "bad page header"))
    }

    fn chain(&self, addr: u64, limit: u64) -> Result<(Charged<Vec<u8>>, u64)> {
        let first = self.page_header(addr)?;
        let total = first.data_size;
        let first_at = self.format.base().saturating_add(addr);
        if total > self.size {
            return Err(corrupt(first_at, "data size exceeds the file"));
        }
        if total > limit {
            return Err(ManagedReadError::TooLarge {
                what: "stored entry",
                size: total,
                limit,
            });
        }
        let len = usize::try_from(total).map_err(|_| BudgetError::Overflow)?;
        let mut output = BudgetedVec::try_with_capacity(self.budget, self.category, len)?;
        output.try_resize(len, 0)?;
        let mut seen = PageOwners::new(self.budget, self.category)?;
        let mut current = addr;
        let mut left = total;
        let mut written = 0usize;
        let mut pages = 0u64;
        loop {
            let at = self.format.base().saturating_add(current);
            if seen.insert(current, 0)?.is_some()
                || seen.len as u64 > self.size / self.format.page_header_size()
            {
                return Err(corrupt(at, "page loop"));
            }
            let header = if pages == 0 {
                first
            } else {
                self.page_header(current)?
            };
            pages += 1;
            let take = header.page_size.min(left);
            let start = at + self.format.page_header_size();
            if start.checked_add(take).is_none_or(|end| end > self.size) {
                return Err(corrupt(start, "page data beyond the end of the file"));
            }
            let end = written + usize::try_from(take).map_err(|_| BudgetError::Overflow)?;
            self.src.read_exact_at(
                output
                    .get_mut(written..end)
                    .ok_or_else(|| corrupt(start, "buffer"))?,
                start,
            )?;
            written = end;
            left -= take;
            if left == 0 {
                return Ok((output.into_charged_vec(), pages));
            }
            if header.next_page == self.format.undefined() {
                return Err(corrupt(at, "short chain"));
            }
            current = header.next_page;
        }
    }

    fn entries(&self) -> Result<Charged<Vec<EntryInfo>>> {
        let format = self.format;
        let (toc, _) = self.chain(format.file_header_size(), u64::MAX)?;
        let address_size = format.addr_size() as usize;
        let read_address = |bytes: &[u8], index: usize| -> Option<u64> {
            let bytes = bytes.get(index * address_size..(index + 1) * address_size)?;
            let mut word = [0u8; 8];
            word.get_mut(..address_size)?.copy_from_slice(bytes);
            Some(u64::from_le_bytes(word))
        };
        let count = toc
            .chunks_exact(format.toc_record_size() as usize)
            .take_while(|record| read_address(record, 2) == Some(format.undefined()))
            .count();
        let mut output =
            BudgetedVec::try_with_capacity(self.budget, self.category, count)?.into_charged_vec();
        let mut remaining = self.size;
        let mut owners = PageOwners::new(self.budget, self.category)?;
        for (index, record) in toc
            .chunks_exact(format.toc_record_size() as usize)
            .take(count)
            .enumerate()
        {
            let header_addr =
                read_address(record, 0).ok_or_else(|| corrupt(format.base(), "TOC record"))?;
            let data =
                read_address(record, 1).ok_or_else(|| corrupt(format.base(), "TOC record"))?;
            let (header, pages) = self.chain(header_addr, u64::MAX)?;
            let cost = pages
                .saturating_mul(format.page_header_size())
                .saturating_add(header.len() as u64);
            remaining = remaining.checked_sub(cost).ok_or_else(|| {
                corrupt(
                    format.base().saturating_add(header_addr),
                    "entry headers share pages",
                )
            })?;
            let entry = self.entry_header(&header, header_addr)?;
            let data_addr = (data != format.undefined()).then_some(data);
            let data_len = data_addr.map_or(Ok(0), |addr| {
                self.walk_data(addr, index, &mut owners, &mut remaining)
            })?;
            output = output.try_zip(entry)?.map(|(mut output, mut entry)| {
                entry.data_addr = data_addr;
                entry.data_len = data_len;
                output.push(entry);
                output
            });
        }
        Ok(output)
    }

    fn entry_header(&self, bytes: &[u8], addr: u64) -> Result<Charged<EntryInfo>> {
        let bad = || corrupt(self.format.base().saturating_add(addr), "entry header");
        let ctime = u64::from_le_bytes(
            bytes
                .get(..8)
                .ok_or_else(bad)?
                .try_into()
                .map_err(|_| bad())?,
        );
        let mtime = u64::from_le_bytes(
            bytes
                .get(8..16)
                .ok_or_else(bad)?
                .try_into()
                .map_err(|_| bad())?,
        );
        let attrs = u32::from_le_bytes(
            bytes
                .get(16..20)
                .ok_or_else(bad)?
                .try_into()
                .map_err(|_| bad())?,
        );
        let name_bytes = bytes.get(20..).ok_or_else(bad)?;
        let chars = || {
            char::decode_utf16(
                name_bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|bytes| u16::from_le_bytes(*bytes))
                    .take_while(|&unit| unit != 0),
            )
            .map(|decoded| decoded.unwrap_or(char::REPLACEMENT_CHARACTER))
        };
        let size = chars().try_fold(0usize, |size, ch| {
            size.checked_add(ch.len_utf8()).ok_or(BudgetError::Overflow)
        })?;
        let mut name = BudgetedVec::try_with_capacity(self.budget, self.category, size)?;
        for ch in chars() {
            name.try_extend_from_slice(ch.encode_utf8(&mut [0; 4]).as_bytes())?;
        }
        let name = name
            .into_charged_vec()
            .try_map(String::from_utf8)
            .map_err(|_| bad())?;
        Ok(name.map(|name| EntryInfo {
            name,
            ctime: V8Time(ctime),
            mtime: V8Time(mtime),
            attrs,
            header_addr: addr,
            data_addr: None,
            data_len: 0,
        }))
    }

    fn walk_data(
        &self,
        addr: u64,
        entry: usize,
        owners: &mut PageOwners<'_>,
        remaining: &mut u64,
    ) -> Result<u64> {
        let format = self.format;
        let page_size = format.page_header_size();
        let mut current = addr;
        let mut first = None;
        let mut left = 0u64;
        for _ in 0..self.size / page_size {
            let at = format.base().saturating_add(current);
            if owners.get(current) == Some(entry) {
                break;
            }
            let header = match self.page_header(current) {
                Ok(header) => header,
                Err(ManagedReadError::Io(error)) => return Err(error.into()),
                Err(_) => break,
            };
            if owners.insert(current, entry)?.is_some() {
                return Err(corrupt(at, "entries share pages"));
            }
            let total = match first {
                Some(_) => left,
                None => {
                    first = Some(header.data_size);
                    header.data_size
                }
            };
            let take = header.page_size.min(total);
            let fits = total <= self.size
                && at
                    .saturating_add(page_size)
                    .checked_add(take)
                    .is_some_and(|end| end <= self.size);
            let cost = page_size.saturating_add(if fits { take } else { 0 });
            *remaining = remaining
                .checked_sub(cost)
                .ok_or_else(|| corrupt(at, "overlapping pages"))?;
            left = total - take;
            if !fits || left == 0 || header.next_page == format.undefined() {
                break;
            }
            current = header.next_page;
        }
        Ok(first.unwrap_or(0))
    }
}

struct PageOwners<'a> {
    slots: BudgetedVec<Option<(u64, usize)>>,
    len: usize,
    budget: &'a MemoryBudget,
    category: Category,
}
impl<'a> PageOwners<'a> {
    fn new(budget: &'a MemoryBudget, category: Category) -> Result<Self> {
        Ok(Self {
            slots: BudgetedVec::new(budget, category)?,
            len: 0,
            budget,
            category,
        })
    }
    fn bucket(key: u64, slots: usize) -> usize {
        let mut hash = key ^ (key >> 30);
        hash = hash.wrapping_mul(0xbf58_476d_1ce4_e5b9);
        hash ^= hash >> 27;
        hash = hash.wrapping_mul(0x94d0_49bb_1331_11eb);
        ((hash ^ (hash >> 31)) as usize) & slots.saturating_sub(1)
    }
    fn get(&self, key: u64) -> Option<usize> {
        if self.slots.is_empty() {
            return None;
        }
        let mut index = Self::bucket(key, self.slots.len());
        for _ in 0..self.slots.len() {
            match self.slots.get(index).copied().flatten() {
                Some((found, value)) if found == key => return Some(value),
                None => return None,
                _ => index = (index + 1) & (self.slots.len() - 1),
            }
        }
        None
    }
    fn place(slots: &mut [Option<(u64, usize)>], key: u64, value: usize) -> Result<()> {
        let mut index = Self::bucket(key, slots.len());
        let len = slots.len();
        for _ in 0..len {
            let slot = slots.get_mut(index).ok_or(BudgetError::Overflow)?;
            if slot.is_none() {
                *slot = Some((key, value));
                return Ok(());
            }
            index = (index + 1) & (len - 1);
        }
        Err(BudgetError::Overflow.into())
    }
    fn insert(&mut self, key: u64, value: usize) -> Result<Option<usize>> {
        if let Some(existing) = self.get(key) {
            return Ok(Some(existing));
        }
        if self.len >= self.slots.len() / 2 {
            let capacity = self
                .slots
                .len()
                .checked_mul(2)
                .ok_or(BudgetError::Overflow)?
                .max(16);
            let mut next = BudgetedVec::try_with_capacity(self.budget, self.category, capacity)?;
            next.try_resize(capacity, None)?;
            for &(key, value) in self.slots.iter().flatten() {
                Self::place(&mut next, key, value)?;
            }
            self.slots = next;
        }
        Self::place(&mut self.slots, key, value)?;
        self.len += 1;
        Ok(None)
    }
}
