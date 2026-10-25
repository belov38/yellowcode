#[path = "budgeted.rs"]
mod budgeted;
pub use budgeted::BudgetedEntries;

use std::cmp::Ordering;
use std::collections::HashMap;
use std::ops::Bound;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::db::RowId;
use crate::error::{Error, Location, NotFound, Result};
use crate::format::descr::IndexDescr;
use crate::format::index_page::{
    BranchPage, IndexPage, LeafPage, NONE, decode_index_page, parse_descriptor,
    parse_index_descriptor_pointer,
};
use crate::format::key::{encode_key_values, index_key_len};
use crate::format::value::{Row, Value};
use crate::ifile::IFile;
use crate::pager::PageBuf;
use crate::table::TableRead;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Direction {
    Forward,
    Backward,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    pub row: RowId,
    pub key: Vec<u8>,
}

pub struct IndexRead<'a> {
    table: &'a TableRead<'a>,
    idx: &'a IndexDescr,
    file: IFile,
    page_size: u64,
    npages: u32,
    ptr_unit: u64,
    key_len: u32,
    root: u32,
    branches: Mutex<BranchCache>,
}

const BRANCH_CACHE_BYTES: usize = 32 << 20;
const BRANCH_ENTRY_OVERHEAD: usize = 48;
const BRANCH_PAGE_OVERHEAD: usize = 128;

#[derive(Default)]
struct BranchCache {
    pages: HashMap<u32, Arc<BranchPage>>,
    bytes: usize,
}

impl BranchCache {
    fn put(&mut self, no: u32, page: &Arc<BranchPage>, key_len: u32) {
        let per_entry = (key_len as usize).saturating_add(BRANCH_ENTRY_OVERHEAD);
        let size =
            (page.entries.len().saturating_mul(per_entry)).saturating_add(BRANCH_PAGE_OVERHEAD);
        let bytes = self.bytes.saturating_add(size);
        if bytes <= BRANCH_CACHE_BYTES && !self.pages.contains_key(&no) {
            self.bytes = bytes;
            self.pages.insert(no, Arc::clone(page));
        }
    }
}

enum Node {
    Leaf(LeafPage),
    Branch(Arc<BranchPage>),
}

impl std::fmt::Debug for IndexRead<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IndexRead")
            .field("index", &self.idx.name)
            .field("key_len", &self.key_len)
            .finish_non_exhaustive()
    }
}

struct Bounds {
    lo: Bound<Vec<u8>>,
    hi: Bound<Vec<u8>>,
}

fn cmp_prefix(key: &[u8], b: &[u8]) -> Ordering {
    key.get(..b.len()).unwrap_or(key).cmp(b)
}

impl Bounds {
    fn new(lo: Bound<&[u8]>, hi: Bound<&[u8]>) -> Bounds {
        Bounds {
            lo: lo.map(<[u8]>::to_vec),
            hi: hi.map(<[u8]>::to_vec),
        }
    }

    fn below_lo(&self, key: &[u8]) -> bool {
        match &self.lo {
            Bound::Unbounded => false,
            Bound::Included(b) => cmp_prefix(key, b) == Ordering::Less,
            Bound::Excluded(b) => cmp_prefix(key, b) != Ordering::Greater,
        }
    }

    fn above_hi(&self, key: &[u8]) -> bool {
        match &self.hi {
            Bound::Unbounded => false,
            Bound::Included(b) => cmp_prefix(key, b) == Ordering::Greater,
            Bound::Excluded(b) => cmp_prefix(key, b) != Ordering::Less,
        }
    }
}

pub(crate) fn page_number(ptr: u32, unit: u64, page_size: u64, npages: u32) -> Option<u32> {
    let off = u64::from(ptr).checked_mul(unit)?;
    if page_size == 0 || !off.is_multiple_of(page_size) {
        return None;
    }
    u32::try_from(off / page_size)
        .ok()
        .filter(|&n| n > 0 && n < npages)
}

impl<'a> IndexRead<'a> {
    pub(crate) fn open(table: &'a TableRead<'a>, name: &str) -> Result<IndexRead<'a>> {
        if table.pager().memory_budget().is_some() {
            return Self::open_managed(table, name);
        }
        let descr = table.descr();
        let (pos, idx) = descr
            .index(name)
            .ok_or_else(|| Error::NotFound(NotFound::Index(name.to_string())))?;
        let at_table = |what: String| Error::corrupt(Location::Table(descr.name.clone()), what);
        let key_len = index_key_len(descr, idx).map_err(|e| match e {
            Error::Constraint(crate::error::Constraint::BadDescr(why)) => at_table(why),
            other => other,
        })?;

        let [_, _, header_page] = descr.files;
        if header_page == 0 {
            return Err(at_table(format!(
                "index {} is described but the table has no index file",
                idx.name
            )));
        }
        let file = IFile::open(table.pager(), table.format_info(), header_page)?;
        let mut ix = IndexRead::from_parts(table, idx, file, key_len)?;

        let first = ix.raw_page(0)?;
        let descr_ptr = parse_index_descriptor_pointer(&first, table.descr().indexes.len(), pos)
            .map_err(|e| ix.relocate(0, e))?;
        let descr_page = ix.page_of(descr_ptr, 0)?;
        let descriptor =
            parse_descriptor(&ix.raw_page(descr_page)?).map_err(|e| ix.relocate(descr_page, e))?;
        if u32::from(descriptor.key_len) != key_len {
            return Err(ix.at(
                Some(descr_page),
                format!(
                    "descriptor key length {} differs from the {key_len} of the description",
                    descriptor.key_len
                ),
            ));
        }
        ix.root = ix.page_of(descriptor.root, descr_page)?;
        Ok(ix)
    }

    pub(crate) fn from_parts(
        table: &'a TableRead<'a>,
        idx: &'a IndexDescr,
        file: IFile,
        key_len: u32,
    ) -> Result<IndexRead<'a>> {
        let fi = table.format_info();
        let page_size = u64::from(fi.page_size);
        let at_table =
            |what: String| Error::corrupt(Location::Table(table.descr().name.clone()), what);
        if page_size == 0 || !file.length.is_multiple_of(page_size) {
            if table.pager().memory_budget().is_some() {
                return Err(Error::CorruptStatic {
                    location: Location::InternalFile(file.header_page),
                    what: "invalid index file length",
                });
            }
            return Err(at_table(format!(
                "index file length {} is not a multiple of the page size",
                file.length
            )));
        }
        let npages = u32::try_from(file.length / page_size).map_err(|_| {
            if table.pager().memory_budget().is_some() {
                Error::CorruptStatic {
                    location: Location::InternalFile(file.header_page),
                    what: "index file is too long",
                }
            } else {
                at_table("index file is too long".into())
            }
        })?;
        Ok(IndexRead {
            table,
            idx,
            file,
            page_size,
            npages,
            ptr_unit: fi.index_ptr_unit(),
            key_len,
            root: 0,
            branches: Mutex::default(),
        })
    }

    pub(crate) fn with_root(mut self, root: u32) -> IndexRead<'a> {
        self.root = root;
        self
    }

    pub fn descr(&self) -> &IndexDescr {
        self.idx
    }

    pub fn key_len(&self) -> u32 {
        self.key_len
    }

    pub(crate) fn at(&self, page: Option<u32>, what: String) -> Error {
        Error::corrupt(
            Location::Index {
                table: self.table.descr().name.clone(),
                index: self.idx.name.clone(),
                page,
            },
            what,
        )
    }

    fn relocate(&self, page: u32, e: Error) -> Error {
        match e {
            Error::Corrupt {
                location: Location::File,
                what,
            } => self.at(Some(page), what),
            Error::Corrupt { location, what } => {
                self.at(Some(page), format!("{what} ({location})"))
            }
            other => other,
        }
    }

    pub(crate) fn raw_page(&self, no: u32) -> Result<PageBuf> {
        let data = self
            .file
            .data_pages
            .get(no as usize)
            .ok_or_else(|| self.at(Some(no), "page is beyond the index file".into()))?;
        self.table
            .pager()
            .read_page(*data)
            .map_err(|e| self.relocate(no, e))
    }

    pub(crate) fn decode(&self, no: u32) -> Result<IndexPage> {
        decode_index_page(&self.raw_page(no)?, self.key_len).map_err(|e| self.relocate(no, e))
    }

    fn branch_cache(&self) -> MutexGuard<'_, BranchCache> {
        self.branches.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn node(&self, no: u32) -> Result<Node> {
        if let Some(b) = self.branch_cache().pages.get(&no) {
            return Ok(Node::Branch(Arc::clone(b)));
        }
        Ok(match self.decode(no)? {
            IndexPage::Leaf(l) => Node::Leaf(l),
            IndexPage::Branch(b) => {
                let b = Arc::new(b);
                self.branch_cache().put(no, &b, self.key_len);
                Node::Branch(b)
            }
        })
    }

    fn decode_leaf(&self, no: u32) -> Result<LeafPage> {
        match self.decode(no)? {
            IndexPage::Leaf(l) => Ok(l),
            IndexPage::Branch(_) => Err(self.at(Some(no), "a leaf was expected".into())),
        }
    }

    pub(crate) fn page_of(&self, ptr: u32, holder: u32) -> Result<u32> {
        page_number(ptr, self.ptr_unit, self.page_size, self.npages).ok_or_else(|| {
            self.at(
                Some(holder),
                format!("pointer {ptr:#x} does not refer to a tree page"),
            )
        })
    }

    fn empty_branch(&self, no: u32) -> Error {
        self.at(Some(no), "a branch below the root has no entries".into())
    }

    pub(crate) fn root_page(&self) -> u32 {
        self.root
    }

    pub fn depth(&self) -> Result<u32> {
        let mut no = self.root;
        let mut branches = 0;
        for _ in 0..=self.npages {
            match self.node(no)? {
                Node::Leaf(_) => return Ok(branches),
                Node::Branch(b) => {
                    branches += 1;
                    match b.entries.first() {
                        Some(e) => no = self.page_of(e.child, no)?,
                        None if no == self.root => return Ok(branches),
                        None => return Err(self.empty_branch(no)),
                    }
                }
            }
        }
        Err(self.at(Some(self.root), "the tree is deeper than the file".into()))
    }

    pub fn key(&self, vals: &[Value]) -> Result<Vec<u8>> {
        encode_key_values(self.table.descr(), self.idx, vals, &self.table.key_ctx())
    }

    fn start_leaf(&self, bounds: &Bounds, dir: Direction) -> Result<Option<(u32, LeafPage)>> {
        let mut no = self.root;
        for _ in 0..=self.npages {
            match self.node(no)? {
                Node::Leaf(l) => return Ok(Some((no, l))),
                Node::Branch(b) => {
                    let i = match dir {
                        Direction::Forward => {
                            b.entries.partition_point(|e| bounds.below_lo(&e.key))
                        }
                        Direction::Backward => {
                            b.entries.partition_point(|e| !bounds.above_hi(&e.key))
                        }
                    };
                    match b.entries.get(i).or_else(|| b.entries.last()) {
                        Some(e) => no = self.page_of(e.child, no)?,
                        None if no == self.root => return Ok(None),
                        None => return Err(self.empty_branch(no)),
                    }
                }
            }
        }
        Err(self.at(Some(self.root), "the tree is deeper than the file".into()))
    }

    pub fn entries<'s>(
        &'s self,
        lo: Bound<&[u8]>,
        hi: Bound<&[u8]>,
        dir: Direction,
    ) -> Result<impl Iterator<Item = Result<IndexEntry>> + use<'s, 'a>> {
        let bounds = Bounds::new(lo, hi);
        let start = self.start_leaf(&bounds, dir)?;
        let (page, leaf, pos) = match start {
            Some((no, l)) => {
                let pos = match dir {
                    Direction::Forward => 0,
                    Direction::Backward => l.entries.len(),
                };
                (no, Some(l), pos)
            }
            None => (0, None, 0),
        };
        Ok(Entries {
            ix: self,
            bounds,
            dir,
            page,
            leaf,
            pos,
            started: false,
            hops: 0,
            done: false,
        })
    }

    pub fn entries_after<'s>(
        &'s self,
        lo: Bound<&[u8]>,
        hi: Bound<&[u8]>,
        dir: Direction,
        after: Option<&IndexEntry>,
    ) -> Result<impl Iterator<Item = Result<IndexEntry>> + use<'s, 'a>> {
        let bounds = Bounds::new(lo, hi);
        if after.is_some_and(|a| a.key.len() != self.key_len as usize) {
            return Err(self.at(None, "resume key has the wrong length".into()));
        }
        let left = |key: &[u8], row: u32| -> bool {
            let pos = (key, row);
            match dir {
                Direction::Forward => {
                    bounds.below_lo(key) || after.is_some_and(|a| pos <= (a.key.as_slice(), a.row))
                }
                Direction::Backward => {
                    !bounds.above_hi(key) && after.is_none_or(|a| pos < (a.key.as_slice(), a.row))
                }
            }
        };
        let mut no = self.root;
        let mut start = None;
        for depth in 0..=self.npages {
            match self.node(no)? {
                Node::Leaf(l) => {
                    start = Some((no, l));
                    break;
                }
                Node::Branch(b) => {
                    if b.entries.is_empty() && no != self.root {
                        return Err(self.empty_branch(no));
                    }
                    let i = b.entries.partition_point(|e| left(&e.key, e.row));
                    let child = match dir {
                        Direction::Forward => b.entries.get(i),
                        Direction::Backward => b.entries.get(i).or_else(|| b.entries.last()),
                    };
                    match child {
                        Some(e) => no = self.page_of(e.child, no)?,
                        None => break,
                    }
                }
            }
            if depth == self.npages {
                return Err(self.at(Some(self.root), "the tree is deeper than the file".into()));
            }
        }
        let (page, leaf, pos) = match start {
            Some((no, l)) => {
                let pos = l.entries.partition_point(|(row, key)| left(key, *row));
                (no, Some(l), pos)
            }
            None => (0, None, 0),
        };
        Ok(Entries {
            ix: self,
            bounds,
            dir,
            page,
            leaf,
            pos,
            started: false,
            hops: 0,
            done: false,
        })
    }

    pub fn range_after<'s>(
        &'s self,
        lo: Bound<&[u8]>,
        hi: Bound<&[u8]>,
        dir: Direction,
        after: Option<&IndexEntry>,
    ) -> Result<impl Iterator<Item = Result<(IndexEntry, Row)>> + use<'s, 'a>> {
        Ok(self.entries_after(lo, hi, dir, after)?.map(move |item| {
            let e = item?;
            match self.table.get(e.row) {
                Ok(Some(row)) => Ok((e, row)),
                Ok(None) | Err(Error::NotFound(NotFound::Row { .. })) => Err(self.at(
                    None,
                    format!(
                        "an entry refers to row {}, which is not a live record",
                        e.row
                    ),
                )),
                Err(other) => Err(other),
            }
        }))
    }

    pub fn range<'s>(
        &'s self,
        lo: Bound<&[u8]>,
        hi: Bound<&[u8]>,
        dir: Direction,
    ) -> Result<impl Iterator<Item = Result<(RowId, Row)>> + use<'s, 'a>> {
        let entries = self.entries(lo, hi, dir)?;
        Ok(entries.map(move |item| {
            let e = item?;
            match self.table.get(e.row) {
                Ok(Some(row)) => Ok((e.row, row)),
                Ok(None) | Err(Error::NotFound(NotFound::Row { .. })) => Err(self.at(
                    None,
                    format!(
                        "an entry refers to row {}, which is not a live record",
                        e.row
                    ),
                )),
                Err(other) => Err(other),
            }
        }))
    }
}

struct Entries<'s, 'a> {
    ix: &'s IndexRead<'a>,
    bounds: Bounds,
    dir: Direction,
    page: u32,
    leaf: Option<LeafPage>,
    pos: usize,
    started: bool,
    hops: u32,
    done: bool,
}

impl Entries<'_, '_> {
    fn follow(&mut self, ptr: u32) -> Result<()> {
        if ptr == NONE {
            self.leaf = None;
            return Ok(());
        }
        self.hops += 1;
        if self.hops > self.ix.npages {
            return Err(self.ix.at(None, "the chain of leaves does not end".into()));
        }
        let no = self.ix.page_of(ptr, self.page)?;
        let leaf = self.ix.decode_leaf(no)?;
        self.page = no;
        self.pos = match self.dir {
            Direction::Forward => 0,
            Direction::Backward => leaf.entries.len(),
        };
        self.leaf = Some(leaf);
        Ok(())
    }

    fn advance(&mut self) -> Result<Option<IndexEntry>> {
        loop {
            let Some(leaf) = &mut self.leaf else {
                return Ok(None);
            };
            let (prev, next) = (leaf.prev, leaf.next);
            let at = match self.dir {
                Direction::Forward => {
                    let at = self.pos;
                    self.pos += 1;
                    Some(at).filter(|&i| i < leaf.entries.len())
                }
                Direction::Backward => {
                    let at = self.pos.min(leaf.entries.len()).checked_sub(1);
                    self.pos = at.unwrap_or(0);
                    at
                }
            };
            let Some((row, key)) = at.and_then(|i| leaf.entries.get_mut(i)) else {
                self.follow(match self.dir {
                    Direction::Forward => next,
                    Direction::Backward => prev,
                })?;
                continue;
            };
            let (before_start, past_end) = match self.dir {
                Direction::Forward => (self.bounds.below_lo(key), self.bounds.above_hi(key)),
                Direction::Backward => (self.bounds.above_hi(key), self.bounds.below_lo(key)),
            };
            if !self.started && before_start {
                continue;
            }
            if past_end {
                return Ok(None);
            }
            self.started = true;
            return Ok(Some(IndexEntry {
                row: *row,
                key: std::mem::take(key),
            }));
        }
    }
}

impl Iterator for Entries<'_, '_> {
    type Item = Result<IndexEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        match self.advance() {
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
