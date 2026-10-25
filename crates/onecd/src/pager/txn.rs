use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::versioned::{DirtyPage, Snapshot};
use super::{PageBuf, PageKind, PageRead};
use crate::error::{Error, Location, Result, Unsupported};
use crate::format::bytes;
use crate::format::freelist::{FreeListHeader, encode_freelist_header_838, entries_per_list_page};
use crate::format::header::{DbHeader, encode_header};
use crate::format::ifile::{
    IFileHeader, ifile_versions_838, parse_ifile_header, put_ifile_versions_838,
};
use crate::format::index_page::{IndexPage, encode_branch, encode_leaf};
use crate::format::{Format, FormatInfo};
use crate::freelist::FreeList;

pub(crate) const ABORTED: &str = "transaction aborted by an earlier error";

struct Private {
    kind: PageKind,
    data: PageBuf,
    base: Option<PageBuf>,
}

pub(crate) struct TreePage {
    pub page: IndexPage,
    key_len: u32,
    pub used: usize,
    dirty: bool,
    est: usize,
}

impl TreePage {
    pub fn changed(&self) -> bool {
        self.dirty
    }

    pub fn key_len(&self) -> u32 {
        self.key_len
    }

    fn new(page: IndexPage, key_len: u32, used: usize, dirty: bool) -> TreePage {
        let mut t = TreePage {
            page,
            key_len,
            used,
            dirty,
            est: 0,
        };
        t.est = t.estimate();
        t
    }

    fn estimate(&self) -> usize {
        let entries = match &self.page {
            IndexPage::Leaf(l) => l.entries.len(),
            IndexPage::Branch(b) => b.entries.len(),
        };
        let per = usize::try_from(self.key_len)
            .unwrap_or(usize::MAX)
            .saturating_add(48);
        entries.saturating_mul(per).saturating_add(64)
    }

    fn render(&self, no: u32, page_size: u32) -> Result<PageBuf> {
        let bytes = match &self.page {
            IndexPage::Leaf(l) => encode_leaf(l, self.key_len, page_size),
            IndexPage::Branch(b) => encode_branch(b, self.key_len, page_size),
        };
        bytes
            .map(PageBuf::from)
            .ok_or_else(|| Error::corrupt(Location::Page(no), "a held index page does not encode"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileTouch {
    Changed { pages: bool },
    Created,
    Freed,
}

pub(crate) struct TxnPages {
    base: Snapshot,
    fi: FormatInfo,
    private: HashMap<u32, Private>,
    num_pages: u32,
    freelist: FreeList,
    free_set: HashSet<u32>,
    reserved: HashSet<u32>,
    freelist_changed: bool,
    dirty_bytes: usize,
    max_dirty_bytes: usize,
    poisoned: Arc<AtomicBool>,
    touched: HashMap<u32, FileTouch>,
    trees: HashMap<u32, TreePage>,
    tree_bytes: usize,
    tree_budget: usize,
}

impl fmt::Debug for TxnPages {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TxnPages")
            .field("epoch", &self.base.epoch)
            .field("num_pages", &self.num_pages)
            .field("private", &self.private.len())
            .field("trees", &self.trees.len())
            .field("poisoned", &self.is_poisoned())
            .finish_non_exhaustive()
    }
}

fn aborted() -> Error {
    Error::corrupt(Location::File, ABORTED)
}

fn not_held(no: u32) -> Error {
    Error::corrupt(Location::Page(no), "page is not held decoded")
}

fn check_free_list(
    fl: &FreeList,
    header_page: u32,
    num_pages: u32,
) -> Result<(HashSet<u32>, HashSet<u32>)> {
    let bad = |what: String| Error::corrupt(Location::Page(header_page), what);
    let mut reserved = HashSet::from([0, header_page]);
    for (i, &s) in fl.slots.iter().enumerate().filter(|(_, s)| **s != 0) {
        if s >= num_pages {
            return Err(bad(format!("list page {s} in slot {i} is out of range")));
        }
        if !reserved.insert(s) {
            return Err(bad(format!(
                "list page {s} in slot {i} is listed twice or is a header page"
            )));
        }
    }
    if fl.free_pages.len() != fl.count as usize {
        return Err(bad(format!(
            "{} free pages are listed, the header says {}",
            fl.free_pages.len(),
            fl.count
        )));
    }
    let mut free = HashSet::with_capacity(fl.free_pages.len());
    for &p in &fl.free_pages {
        if p == header_page {
            return Err(bad(format!("free page {p} is the free-list header")));
        }
        if reserved.contains(&p) {
            return Err(bad(format!("free page {p} is a list page")));
        }
        if !free.insert(p) {
            return Err(bad(format!("free page {p} is listed twice")));
        }
    }
    Ok((free, reserved))
}

impl TxnPages {
    pub fn new(base: Snapshot, max_dirty_bytes: usize) -> Result<TxnPages> {
        if base.header.format != Format::V838 {
            return Err(Error::Unsupported(Unsupported::WriteLegacyFormat));
        }
        let fi = FormatInfo {
            format: base.header.format,
            page_size: base.header.page_size,
        };
        let header_page = base.header.freelist_page;
        let freelist = FreeList::load(&base, fi, header_page)?;
        let (free_set, reserved) = check_free_list(&freelist, header_page, base.num_pages)?;
        Ok(TxnPages {
            fi,
            private: HashMap::new(),
            num_pages: base.num_pages,
            freelist,
            free_set,
            reserved,
            freelist_changed: false,
            dirty_bytes: 0,
            max_dirty_bytes,
            poisoned: Arc::new(AtomicBool::new(false)),
            touched: HashMap::new(),
            trees: HashMap::new(),
            tree_bytes: 0,
            tree_budget: max_dirty_bytes / 4,
            base,
        })
    }

    pub fn page_mut(&mut self, no: u32, kind: PageKind) -> Result<&mut [u8]> {
        self.run(|t| {
            t.check_kind(kind)?;
            t.check_target(no)?;
            t.make_private(no, kind)?;
            t.settle_tree(no)
        })?;
        self.data_mut(no)
    }

    pub fn alloc_page(&mut self, kind: PageKind) -> Result<u32> {
        self.run(|t| {
            t.check_kind(kind)?;
            let Some(no) = t.freelist.pop() else {
                return t.grow(kind);
            };
            t.free_set.remove(&no);
            t.freelist_changed = true;
            if !t.private.contains_key(&no) {
                t.charge()?;
            }
            t.put_zeroed(no, kind);
            Ok(no)
        })
    }

    pub fn free_pages(&mut self, pages: &[u32]) -> Result<()> {
        self.run(|t| {
            let mut batch = HashSet::with_capacity(pages.len());
            for &no in pages {
                t.check_target(no)?;
                if !batch.insert(no) {
                    return Err(Error::corrupt(Location::Page(no), "page is freed twice"));
                }
            }
            pages.iter().try_for_each(|&no| {
                t.settle_tree(no)?;
                t.push_free(no)
            })
        })
    }

    pub fn tree(&self, no: u32, key_len: u32) -> Option<&TreePage> {
        self.trees.get(&no).filter(|t| t.key_len() == key_len)
    }

    pub fn hold_tree(&mut self, no: u32, page: IndexPage, key_len: u32, used: usize) -> Result<()> {
        self.run(|t| {
            t.settle_tree(no)?;
            t.keep_tree(no, TreePage::new(page, key_len, used, false));
            Ok(())
        })
    }

    pub fn tree_mut(&mut self, no: u32) -> Result<&mut TreePage> {
        self.run(|t| {
            if !t.trees.contains_key(&no) {
                return Err(not_held(no));
            }
            t.make_tree_private(no)
        })?;
        let t = self.trees.get_mut(&no).ok_or_else(|| not_held(no))?;
        let est = t.estimate();
        self.tree_bytes = self.tree_bytes.saturating_sub(t.est).saturating_add(est);
        t.est = est;
        t.dirty = true;
        Ok(t)
    }

    pub fn put_tree(&mut self, no: u32, page: IndexPage, key_len: u32, used: usize) -> Result<()> {
        self.run(|t| {
            t.make_tree_private(no)?;
            t.keep_tree(no, TreePage::new(page, key_len, used, true));
            Ok(())
        })
    }

    pub fn trim_trees(&mut self) -> Result<()> {
        self.run(|t| {
            if t.tree_bytes <= t.tree_budget {
                return Ok(());
            }
            t.trees.retain(|_, held| held.dirty);
            t.tree_bytes = t
                .trees
                .values()
                .fold(0, |n, held| n.saturating_add(held.est));
            if t.tree_bytes > t.tree_budget / 2 {
                t.render_trees()?;
            }
            Ok(())
        })
    }

    pub fn into_dirty(mut self) -> Result<(Vec<DirtyPage>, u32)> {
        self.check_alive()?;
        self.render_trees()?;
        if self.freelist_changed {
            let h = FreeListHeader {
                count: self.freelist.count,
                counter: self.freelist.counter.wrapping_add(1),
                slots: self.freelist.slots.clone(),
            };
            let no = self.base.header.freelist_page;
            self.make_private(no, PageKind::FreeList)?;
            encode_freelist_header_838(&h, self.data_mut(no)?)?;
        }
        if self.num_pages != self.base.num_pages {
            let h = self.header();
            self.make_private(0, PageKind::DbHeader)?;
            encode_header(&h, self.data_mut(0)?)?;
        }
        let dirty = self
            .private
            .into_iter()
            .map(|(no, p)| DirtyPage {
                no,
                kind: p.kind,
                data: p.data,
                base: p.base,
            })
            .collect();
        Ok((dirty, self.num_pages))
    }

    pub fn header(&self) -> DbHeader {
        DbHeader {
            length_pages: self.num_pages,
            ..self.base.header.clone()
        }
    }

    pub fn base(&self) -> &Snapshot {
        &self.base
    }

    pub fn free_count(&self) -> u32 {
        self.freelist.count
    }

    pub fn pending_bytes(&self) -> usize {
        self.dirty_bytes.saturating_add(self.tree_bytes)
    }

    pub fn format_info(&self) -> FormatInfo {
        self.fi
    }

    pub fn check_alive(&self) -> Result<()> {
        if self.is_poisoned() {
            return Err(aborted());
        }
        Ok(())
    }

    fn is_poisoned(&self) -> bool {
        self.poisoned.load(Ordering::Relaxed)
    }

    pub fn is_free(&self, no: u32) -> bool {
        self.free_set.contains(&no)
    }

    pub fn abort(&mut self) {
        self.poisoned.store(true, Ordering::Relaxed);
    }

    pub fn abort_on_panic(&self) -> UnwindAbort {
        UnwindAbort {
            poisoned: Arc::clone(&self.poisoned),
            already_panicking: std::thread::panicking(),
        }
    }

    pub fn touch_file(&mut self, header_page: u32, page_set_changed: bool) {
        let touch = self
            .touched
            .entry(header_page)
            .or_insert(FileTouch::Changed { pages: false });
        if let FileTouch::Changed { pages } = touch {
            *pages |= page_set_changed;
        }
    }

    pub fn file_created(&mut self, header_page: u32) {
        self.touched.insert(header_page, FileTouch::Created);
    }

    pub fn file_freed(&mut self, header_page: u32) {
        self.touched.insert(header_page, FileTouch::Freed);
    }

    pub fn v1_after_commit(&self, header_page: u32) -> Result<u32> {
        if self.touched.get(&header_page) == Some(&FileTouch::Created) {
            return Ok(1);
        }
        let [v1, _, _] = ifile_versions_838(&self.base.read_page(header_page)?, header_page)?;
        Ok(v1.wrapping_add(1))
    }

    pub fn apply_versions(&mut self) -> Result<()> {
        self.run(|t| {
            let mut files: Vec<(u32, FileTouch)> = t.touched.drain().collect();
            files.sort_unstable_by_key(|&(h, _)| h);
            for (h, touch) in files {
                let changed = match touch {
                    FileTouch::Freed => continue,
                    FileTouch::Created => None,
                    FileTouch::Changed { pages } => Some(pages),
                };
                let IFileHeader::V838 {
                    fatlevel, length, ..
                } = parse_ifile_header(t.fi, &t.read_page(h)?, h)?
                else {
                    return Err(Error::Unsupported(Unsupported::WriteLegacyFormat));
                };
                let v3 = u32::from(fatlevel == 1);
                let versions = match changed {
                    None if length > 0 => [1, 1, v3],
                    None => [0, 0, 0],
                    Some(pages) => {
                        let [v1, v2, _] = ifile_versions_838(&t.base.read_page(h)?, h)?;
                        let v2 = if pages { v2.wrapping_add(1) } else { v2 };
                        [v1.wrapping_add(1), v2, v3]
                    }
                };
                put_ifile_versions_838(t.page_mut(h, PageKind::FileHeader)?, h, versions)?;
            }
            Ok(())
        })
    }

    pub fn run<T>(&mut self, op: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        self.check_alive()?;
        let _unwind = self.abort_on_panic();
        let r = op(self);
        if r.is_err() {
            self.abort();
        }
        r
    }

    fn check_kind(&self, kind: PageKind) -> Result<()> {
        match kind {
            PageKind::Content | PageKind::Alloc | PageKind::FileHeader => Ok(()),
            PageKind::FreeList | PageKind::DbHeader => Err(Error::corrupt(
                Location::File,
                format!("page kind {kind:?} is written by the transaction itself"),
            )),
        }
    }

    fn check_target(&self, no: u32) -> Result<()> {
        let bad = |what: String| Err(Error::corrupt(Location::Page(no), what));
        if no >= self.num_pages {
            return bad(format!(
                "page is beyond the end of the file ({} pages)",
                self.num_pages
            ));
        }
        if self.reserved.contains(&no) {
            return bad("page belongs to the file header or the free list".into());
        }
        if self.free_set.contains(&no) {
            return bad("page is free".into());
        }
        Ok(())
    }

    fn charge(&mut self) -> Result<()> {
        let next = self.dirty_bytes.saturating_add(self.fi.page_size as usize);
        if next > self.max_dirty_bytes {
            return Err(Error::TxnTooLarge);
        }
        self.dirty_bytes = next;
        Ok(())
    }

    fn make_private(&mut self, no: u32, kind: PageKind) -> Result<()> {
        if let Some(p) = self.private.get_mut(&no) {
            p.kind = kind;
            return Ok(());
        }
        if no != 0 && no != self.base.header.freelist_page {
            self.charge()?;
        }
        let (data, base) = if no < self.base.num_pages {
            let b = self.base.read_page(no)?;
            (b.clone(), Some(b))
        } else {
            (self.zeroed(), None)
        };
        self.private.insert(no, Private { kind, data, base });
        Ok(())
    }

    fn data_mut(&mut self, no: u32) -> Result<&mut [u8]> {
        match self.private.get_mut(&no) {
            Some(p) => Ok(PageBuf::make_mut(&mut p.data)),
            None => {
                self.poisoned.store(true, Ordering::Relaxed);
                Err(Error::corrupt(Location::Page(no), "page is not private"))
            }
        }
    }

    fn zeroed(&self) -> PageBuf {
        PageBuf::from(vec![0u8; self.fi.page_size as usize])
    }

    fn make_tree_private(&mut self, no: u32) -> Result<()> {
        if self.trees.get(&no).is_some_and(|t| t.dirty) {
            return Ok(());
        }
        self.check_target(no)?;
        self.make_private(no, PageKind::Content)
    }

    fn keep_tree(&mut self, no: u32, t: TreePage) {
        self.tree_bytes = self.tree_bytes.saturating_add(t.est);
        if let Some(old) = self.trees.insert(no, t) {
            self.tree_bytes = self.tree_bytes.saturating_sub(old.est);
        }
    }

    fn drop_tree(&mut self, no: u32) -> Option<TreePage> {
        let t = self.trees.remove(&no)?;
        self.tree_bytes = self.tree_bytes.saturating_sub(t.est);
        Some(t)
    }

    fn settle_tree(&mut self, no: u32) -> Result<()> {
        match self.drop_tree(no) {
            Some(t) if t.dirty => self.put_rendered(no, &t),
            _ => Ok(()),
        }
    }

    fn render_trees(&mut self) -> Result<()> {
        self.tree_bytes = 0;
        let changed = std::mem::take(&mut self.trees)
            .into_iter()
            .filter(|(_, t)| t.dirty);
        for (no, t) in changed {
            self.put_rendered(no, &t)?;
        }
        Ok(())
    }

    fn put_rendered(&mut self, no: u32, t: &TreePage) -> Result<()> {
        let data = t.render(no, self.fi.page_size)?;
        match self.private.get_mut(&no) {
            Some(p) => {
                p.data = data;
                Ok(())
            }
            None => Err(Error::corrupt(Location::Page(no), "page is not private")),
        }
    }

    fn put_zeroed(&mut self, no: u32, kind: PageKind) {
        self.drop_tree(no);
        let data = self.zeroed();
        match self.private.get_mut(&no) {
            Some(p) => {
                p.kind = kind;
                p.data = data;
            }
            None => {
                let p = Private {
                    kind,
                    data,
                    base: None,
                };
                self.private.insert(no, p);
            }
        }
    }

    fn grow(&mut self, kind: PageKind) -> Result<u32> {
        let no = self.num_pages;
        let next = no.checked_add(1).ok_or_else(|| {
            Error::corrupt(Location::File, "the file cannot grow past 2^32 pages")
        })?;
        self.charge()?;
        self.put_zeroed(no, kind);
        self.num_pages = next;
        Ok(no)
    }

    fn push_free(&mut self, no: u32) -> Result<()> {
        let per = entries_per_list_page(self.fi);
        let Some((slot, pos)) = self.freelist.push_target(per) else {
            return Ok(());
        };
        let mut list_page = self.freelist.slots.get(slot).copied().unwrap_or(0);
        if list_page == 0 {
            list_page = self.grow(PageKind::FreeList)?;
            self.freelist.set_list_page(slot, list_page);
            self.reserved.insert(list_page);
        }
        self.drop_tree(list_page);
        self.make_private(list_page, PageKind::FreeList)?;
        let page = self.data_mut(list_page)?;
        pos.checked_mul(4)
            .and_then(|off| bytes::put_u32le(page, off, no))
            .ok_or_else(|| Error::corrupt(Location::Page(list_page), "list page is too short"))?;
        self.freelist.push(no);
        self.free_set.insert(no);
        self.freelist_changed = true;
        Ok(())
    }
}

#[must_use = "the guard covers the scope it lives in"]
pub(crate) struct UnwindAbort {
    poisoned: Arc<AtomicBool>,
    already_panicking: bool,
}

impl Drop for UnwindAbort {
    fn drop(&mut self) {
        if std::thread::panicking() && !self.already_panicking {
            self.poisoned.store(true, Ordering::Relaxed);
        }
    }
}

impl PageRead for TxnPages {
    fn page_size(&self) -> u32 {
        self.fi.page_size
    }

    fn num_pages(&self) -> u32 {
        self.num_pages
    }

    fn read_page(&self, no: u32) -> Result<PageBuf> {
        self.check_alive()?;
        if let Some(t) = self.trees.get(&no).filter(|t| t.dirty) {
            return t
                .render(no, self.fi.page_size)
                .inspect_err(|_| self.poisoned.store(true, Ordering::Relaxed));
        }
        if let Some(p) = self.private.get(&no) {
            return Ok(p.data.clone());
        }
        if no >= self.num_pages {
            return Err(Error::corrupt(
                Location::Page(no),
                format!(
                    "page is beyond the end of the file ({} pages)",
                    self.num_pages
                ),
            ));
        }
        self.base.read_page(no)
    }
}
