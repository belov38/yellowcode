use super::*;
use memory_budget::{
    BudgetError, BudgetedVec, Category, Charged, LimitReason, MemoryLimit, RequestMemory,
};

fn copy(bytes: &[u8], memory: &RequestMemory, category: Category) -> Result<Charged<Vec<u8>>> {
    let mut out = BudgetedVec::try_with_capacity(&memory.budget, category, bytes.len())?;
    out.try_extend_from_slice(bytes)?;
    Ok(out.into_charged_vec())
}
fn bounds(lo: Bound<&[u8]>, hi: Bound<&[u8]>, memory: &RequestMemory) -> Result<Charged<Bounds>> {
    fn bound(b: Bound<&[u8]>, memory: &RequestMemory) -> Result<Charged<Bound<Vec<u8>>>> {
        match b {
            Bound::Unbounded => {
                Ok(copy(&[], memory, Category::ReaderWorking)?.map(|_| Bound::Unbounded))
            }
            Bound::Included(v) => {
                Ok(copy(v, memory, Category::ReaderWorking)?.map(Bound::Included))
            }
            Bound::Excluded(v) => {
                Ok(copy(v, memory, Category::ReaderWorking)?.map(Bound::Excluded))
            }
        }
    }
    Ok(bound(lo, memory)?
        .try_zip(bound(hi, memory)?)?
        .map(|(lo, hi)| Bounds { lo, hi }))
}
impl<'a> IndexRead<'a> {
    fn at_budgeted(&self, page: Option<u32>, what: &'static str) -> Error {
        Error::CorruptStatic {
            location: page.map_or(
                Location::InternalFile(self.file.header_page),
                Location::Page,
            ),
            what,
        }
    }
    fn relocate_budgeted(&self, no: u32, error: Error) -> Error {
        match error {
            Error::CorruptStatic { what, .. } => self.at_budgeted(Some(no), what),
            Error::Corrupt { .. } => self.at_budgeted(Some(no), "invalid index page"),
            other => other,
        }
    }
    fn raw_page_budgeted(&self, no: u32) -> Result<PageBuf> {
        let data = self
            .file
            .data_pages
            .get(no as usize)
            .ok_or_else(|| self.at_budgeted(Some(no), "index page is beyond file"))?;
        self.table
            .pager()
            .read_page(*data)
            .map_err(|e| self.relocate_budgeted(no, e))
    }
    fn page_of_budgeted(&self, ptr: u32, holder: u32) -> Result<u32> {
        page_number(ptr, self.ptr_unit, self.page_size, self.npages).ok_or_else(|| {
            self.at_budgeted(Some(holder), "index pointer does not refer to a tree page")
        })
    }
    pub(super) fn open_managed(table: &'a TableRead<'a>, name: &str) -> Result<IndexRead<'a>> {
        use crate::format::bytes::{self, OrCorrupt};
        use crate::format::descr::FieldType;
        let d = table.descr();
        let (pos, idx) = d
            .index(name)
            .ok_or_else(|| Error::NotFound(NotFound::Index(String::new())))?;
        let invalid = || Error::CorruptStatic {
            location: Location::Root,
            what: "invalid index metadata",
        };
        let mut key_len = 0u64;
        for field in &idx.fields {
            let (_, f) = d.field(&field.name).ok_or_else(invalid)?;
            let n = u64::from(if field.prefix_len == 0 {
                f.length
            } else {
                field.prefix_len
            });
            key_len += match f.ty {
                FieldType::N | FieldType::L | FieldType::D | FieldType::DT => {
                    u64::from(crate::format::layout::value_size(f))
                }
                FieldType::C | FieldType::NC | FieldType::VC | FieldType::NVC => {
                    if f.case_sensitive {
                        4 * n + 3
                    } else {
                        3 * n + 2
                    }
                }
                FieldType::B | FieldType::VB => n,
                _ => return Err(invalid()),
            } + u64::from(f.nullable);
            if key_len > u64::from(crate::format::key::MAX_KEY_LEN) {
                return Err(invalid());
            }
        }
        let header = d
            .files
            .get(2)
            .copied()
            .filter(|&p| p != 0)
            .ok_or_else(invalid)?;
        let file = IFile::open(table.pager(), table.format_info(), header)?;
        let mut ix = Self::from_parts(table, idx, file, key_len as u32)?;
        let raw = ix.raw_page_budgeted(0)?;
        let count = d.indexes.len();
        let end = count
            .checked_mul(4)
            .and_then(|n| n.checked_add(4))
            .ok_or_else(invalid)?;
        if raw.get(..end).is_none() {
            return Err(invalid());
        }
        let at = pos
            .checked_mul(4)
            .and_then(|n| n.checked_add(4))
            .ok_or_else(invalid)?;
        let ptr = bytes::u32le(&raw, at)
            .or_corrupt_static(|| Location::Root, "truncated index header")?;
        let no = ix.page_of_budgeted(ptr, 0)?;
        let descriptor = ix.raw_page_budgeted(no)?;
        let root = bytes::u32le(&descriptor, 0)
            .or_corrupt_static(|| Location::Page(no), "truncated index descriptor")?;
        let len = bytes::u16le(&descriptor, 4)
            .or_corrupt_static(|| Location::Page(no), "truncated index descriptor")?;
        if descriptor.get(..12).is_none() || u32::from(len) != ix.key_len {
            return Err(invalid());
        }
        ix.root = ix.page_of_budgeted(root, no)?;
        Ok(ix)
    }
    pub fn entries_budgeted<'s>(
        &'s self,
        lo: Bound<&[u8]>,
        hi: Bound<&[u8]>,
        dir: Direction,
        memory: &RequestMemory,
    ) -> Result<impl BudgetedEntries + use<'s, 'a>> {
        ManagedEntries::new(self, lo, hi, dir, None, memory)
    }
    pub fn entries_after_budgeted<'s>(
        &'s self,
        lo: Bound<&[u8]>,
        hi: Bound<&[u8]>,
        dir: Direction,
        after: Option<&IndexEntry>,
        memory: &RequestMemory,
    ) -> Result<impl BudgetedEntries + use<'s, 'a>> {
        ManagedEntries::new(self, lo, hi, dir, after, memory)
    }
    fn decode_budgeted(&self, no: u32, memory: &RequestMemory) -> Result<Charged<IndexPage>> {
        crate::format::index_page::decode_index_page_budgeted(
            &self.raw_page_budgeted(no)?,
            self.key_len,
            &memory.budget,
        )
        .map_err(|e| self.relocate_budgeted(no, e))
    }
}
struct ManagedEntries<'s, 'a> {
    ix: &'s IndexRead<'a>,
    memory: RequestMemory,
    bounds: Charged<Bounds>,
    after: Option<Charged<IndexEntry>>,
    dir: Direction,
    page: u32,
    leaf: Option<Charged<LeafPage>>,
    pos: usize,
    started: bool,
    hops: u32,
    done: bool,
}
impl<'s, 'a> ManagedEntries<'s, 'a> {
    fn new(
        ix: &'s IndexRead<'a>,
        lo: Bound<&[u8]>,
        hi: Bound<&[u8]>,
        dir: Direction,
        after: Option<&IndexEntry>,
        memory: &RequestMemory,
    ) -> Result<Self> {
        if after.is_some_and(|a| a.key.len() != ix.key_len as usize) {
            return Err(ix.at_budgeted(None, "resume key has the wrong length"));
        }
        let bounds = bounds(lo, hi, memory)?;
        let after = after
            .map(|a| {
                copy(&a.key, memory, Category::ReaderWorking)
                    .map(|v| v.map(|key| IndexEntry { row: a.row, key }))
            })
            .transpose()?;
        let mut no = ix.root;
        let mut leaf = None;
        for depth in 0..=ix.npages {
            let decoded = ix.decode_budgeted(no, memory)?;
            match &*decoded {
                IndexPage::Leaf(_) => {
                    leaf = Some(
                        decoded
                            .try_map(|p| match p {
                                IndexPage::Leaf(l) => Ok(l),
                                IndexPage::Branch(_) => Err(()),
                            })
                            .map_err(|_| ix.at_budgeted(Some(no), "expected leaf"))?,
                    );
                    break;
                }
                IndexPage::Branch(b) => {
                    let i = match dir {
                        Direction::Forward => b.entries.partition_point(|e| {
                            bounds.below_lo(&e.key)
                                || after.as_ref().is_some_and(|a| {
                                    (e.key.as_slice(), e.row) <= (a.key.as_slice(), a.row)
                                })
                        }),
                        Direction::Backward => b.entries.partition_point(|e| {
                            !bounds.above_hi(&e.key)
                                && after.as_ref().is_none_or(|a| {
                                    (e.key.as_slice(), e.row) < (a.key.as_slice(), a.row)
                                })
                        }),
                    };
                    match b.entries.get(i).or_else(|| b.entries.last()) {
                        Some(e) => no = ix.page_of_budgeted(e.child, no)?,
                        None if no == ix.root => break,
                        None => return Err(ix.at_budgeted(Some(no), "empty non-root branch")),
                    }
                }
            }
            if depth == ix.npages {
                return Err(ix.at_budgeted(Some(ix.root), "tree exceeds file depth"));
            }
        }
        let pos = match dir {
            Direction::Forward => 0,
            Direction::Backward => leaf.as_ref().map_or(0, |l| {
                l.entries.partition_point(|(row, key)| {
                    !bounds.above_hi(key)
                        && after
                            .as_ref()
                            .is_none_or(|a| (key.as_slice(), *row) < (a.key.as_slice(), a.row))
                })
            }),
        };
        Ok(Self {
            ix,
            memory: memory.clone(),
            bounds,
            after,
            dir,
            page: no,
            leaf,
            pos,
            started: false,
            hops: 0,
            done: false,
        })
    }
    fn follow(&mut self, ptr: u32) -> Result<()> {
        self.leaf = None;
        if ptr == NONE {
            return Ok(());
        }
        self.hops += 1;
        if self.hops > self.ix.npages {
            return Err(self.ix.at_budgeted(None, "chain of leaves does not end"));
        }
        let no = self.ix.page_of_budgeted(ptr, self.page)?;
        let decoded = self.ix.decode_budgeted(no, &self.memory)?;
        let leaf = decoded
            .try_map(|p| match p {
                IndexPage::Leaf(l) => Ok(l),
                IndexPage::Branch(_) => Err(()),
            })
            .map_err(|_| self.ix.at_budgeted(Some(no), "expected leaf"))?;
        self.page = no;
        self.pos = match self.dir {
            Direction::Forward => 0,
            Direction::Backward => leaf.entries.len(),
        };
        self.leaf = Some(leaf);
        Ok(())
    }
    fn advance(&mut self) -> Result<Option<(u32, usize)>> {
        loop {
            let Some(leaf) = &self.leaf else {
                return Ok(None);
            };
            let at = match self.dir {
                Direction::Forward => {
                    let at = self.pos;
                    self.pos = self.pos.saturating_add(1);
                    Some(at)
                }
                Direction::Backward => {
                    let at = self.pos.min(leaf.entries.len()).checked_sub(1);
                    self.pos = at.unwrap_or(0);
                    at
                }
            };
            let Some((row, key)) = at.and_then(|i| leaf.entries.get(i)) else {
                let ptr = match self.dir {
                    Direction::Forward => leaf.next,
                    Direction::Backward => leaf.prev,
                };
                self.follow(ptr)?;
                continue;
            };
            let (before, past) = match self.dir {
                Direction::Forward => (self.bounds.below_lo(key), self.bounds.above_hi(key)),
                Direction::Backward => (self.bounds.above_hi(key), self.bounds.below_lo(key)),
            };
            let behind = self.after.as_ref().is_some_and(|a| {
                let ord = (key.as_slice(), *row).cmp(&(a.key.as_slice(), a.row));
                match self.dir {
                    Direction::Forward => ord.is_le(),
                    Direction::Backward => ord.is_ge(),
                }
            });
            if (!self.started && before) || behind {
                continue;
            }
            if past {
                return Ok(None);
            }
            self.started = true;
            if key.len() as u64 > self.memory.row_limit {
                return Err(Error::Memory(BudgetError::Limit(MemoryLimit {
                    category: Category::Row,
                    requested: key.len() as u64,
                    reason: LimitReason::Category,
                })));
            }
            let row = *row;
            let Some(at) = at else {
                return Ok(None);
            };
            return Ok(Some((row, at)));
        }
    }
    fn key_at(&self, at: usize) -> Result<&[u8]> {
        self.leaf
            .as_ref()
            .and_then(|leaf| leaf.entries.get(at))
            .map(|(_, key)| key.as_slice())
            .ok_or_else(|| self.ix.at_budgeted(Some(self.page), "index entry vanished"))
    }
    fn next_charged(&mut self) -> Result<Option<Charged<IndexEntry>>> {
        let Some((row, at)) = self.advance()? else {
            return Ok(None);
        };
        Ok(Some(
            copy(self.key_at(at)?, &self.memory, Category::Row)?.map(|key| IndexEntry { row, key }),
        ))
    }
    fn next_into_scratch(
        &mut self,
        key: &mut memory_budget::Scratch<Vec<u8>>,
    ) -> Result<Option<u32>> {
        let Some((row, at)) = self.advance()? else {
            return Ok(None);
        };
        let bytes = self.key_at(at)?;
        let charge = (bytes.len() as u64)
            .checked_add(std::mem::size_of::<memory_budget::BudgetedVec<u8>>() as u64)
            .ok_or(BudgetError::Overflow)?;
        key.refill(charge, |grant| -> Result<Vec<u8>> {
            let mut out = grant.try_vec_unowned::<u8>(bytes.len())?;
            memory_budget::extend_within(&mut out, bytes)?;
            Ok(out)
        })?;
        Ok(Some(row))
    }
}

pub trait BudgetedEntries: Iterator<Item = Result<Charged<IndexEntry>>> {
    fn next_into(&mut self, key: &mut memory_budget::Scratch<Vec<u8>>) -> Option<Result<u32>>;
}
impl BudgetedEntries for ManagedEntries<'_, '_> {
    fn next_into(&mut self, key: &mut memory_budget::Scratch<Vec<u8>>) -> Option<Result<u32>> {
        if self.done {
            return None;
        }
        match self.next_into_scratch(key) {
            Ok(Some(row)) => Some(Ok(row)),
            Ok(None) => {
                self.done = true;
                None
            }
            Err(e) => {
                self.done = true;
                Some(Err(e))
            }
        }
    }
}
impl Iterator for ManagedEntries<'_, '_> {
    type Item = Result<Charged<IndexEntry>>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        match self.next_charged() {
            Ok(Some(e)) => Some(Ok(e)),
            Ok(None) => {
                self.done = true;
                None
            }
            Err(e) => {
                self.done = true;
                Some(Err(e))
            }
        }
    }
}
