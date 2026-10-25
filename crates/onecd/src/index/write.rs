use std::collections::HashSet;

use crate::error::{Error, Location, Result};
use crate::format::FormatInfo;
use crate::format::bytes::{put_u32le, u32le};
use crate::format::index_page::{
    BranchEntry, BranchPage, DESCRIPTOR_FLAG, IndexDescriptor, IndexPage, LEAF_HEADER, LeafPage,
    NONE, decode_index_page, encode_branch, encode_descriptor, encode_leaf, leaf_fits_page,
    leaf_used, parse_descriptor, parse_index_file_header, stored_key_len,
};
use crate::format::key::MAX_KEY_LEN;
use crate::ifile::IFile;
use crate::index::read::page_number;
use crate::pager::txn::{ABORTED, TreePage, TxnPages};
use crate::pager::{PageBuf, PageRead};

const BRANCH_HEADER: u32 = 12;
const BRANCH_TAIL: u32 = 8;
const SCAN_HOPS: usize = 64;
const PREV_AT: u64 = 4;
const NEXT_AT: u64 = 8;

#[derive(Debug, Clone, Copy)]
pub(crate) struct IndexLoc<'n> {
    pub header_page: u32,
    pub names: Option<(&'n str, &'n str)>,
}

impl<'n> IndexLoc<'n> {
    pub fn file(header_page: u32) -> IndexLoc<'n> {
        IndexLoc {
            header_page,
            names: None,
        }
    }

    pub fn corrupt(self, page: Option<u32>, what: impl Into<String>) -> Error {
        let what = what.into();
        match self.names {
            Some((table, index)) => Error::corrupt(
                Location::Index {
                    table: table.to_string(),
                    index: index.to_string(),
                    page,
                },
                what,
            ),
            None => Error::corrupt(
                Location::InternalFile(self.header_page),
                match page {
                    Some(p) => format!("index page {p}: {what}"),
                    None => what,
                },
            ),
        }
    }

    fn decoded(self, no: u32, e: Error) -> Error {
        match e {
            Error::Corrupt {
                location: Location::File,
                what,
            } => self.corrupt(Some(no), what),
            other => self.adopt(Some(no), other),
        }
    }

    pub fn adopt(self, page: Option<u32>, e: Error) -> Error {
        let (Some(_), Error::Corrupt { location, what }) = (self.names, &e) else {
            return e;
        };
        match location {
            Location::InternalFile(h) if *h == self.header_page => self.corrupt(page, what.clone()),
            Location::Index { .. } => e,
            Location::File if what == ABORTED => e,
            other => self.corrupt(page, format!("{what} ({other})")),
        }
    }
}

pub(crate) fn create_index_file(
    tx: &mut TxnPages,
    file: &mut IFile,
    fi: FormatInfo,
    key_lens: &[u32],
) -> Result<()> {
    let loc = IndexLoc::file(file.header_page);
    let ps = fi.page_size;
    let n = u32::try_from(key_lens.len()).ok().filter(|&n| n < (ps / 4));
    let n = n.ok_or_else(|| loc.corrupt(Some(0), "the header page has no room for the indexes"))?;
    let ptr = |no: u32| {
        u64::from(no)
            .checked_mul(u64::from(ps))
            .and_then(|off| off.checked_div(fi.index_ptr_unit()))
            .and_then(|p| u32::try_from(p).ok())
            .ok_or_else(|| loc.corrupt(Some(no), "page has no pointer"))
    };
    let mut header = vec![0u8; ps as usize];
    let mut trees = Vec::new();
    for (i, &kl) in (0..n).zip(key_lens) {
        let (descr_no, root_no) = (1 + 2 * i, 2 + 2 * i);
        let key_len = u16::try_from(kl)
            .ok()
            .filter(|&k| u32::from(k) <= MAX_KEY_LEN);
        let key_len = key_len.ok_or_else(|| loc.corrupt(None, format!("key length {kl}")))?;
        put_u32le(&mut header, 4 + 4 * i as usize, ptr(descr_no)?)
            .ok_or_else(|| loc.corrupt(Some(0), "the header page has no room"))?;
        let mut descr = vec![0u8; ps as usize];
        let d = IndexDescriptor {
            root: ptr(root_no)?,
            key_len,
            flag: DESCRIPTOR_FLAG,
        };
        encode_descriptor(&d, &mut descr);
        let leaf = encode_leaf(&LeafPage::empty_root(kl), kl, ps)
            .ok_or_else(|| loc.corrupt(Some(root_no), "an empty leaf does not fit a page"))?;
        trees.extend_from_slice(&descr);
        trees.extend_from_slice(&leaf);
    }
    let mut image = header;
    image.extend_from_slice(&trees);
    file.set_len(tx, image.len() as u64)?;
    file.write_at(tx, 0, &image)
}

fn index_pages(loc: IndexLoc<'_>, file: &IFile, ps: u64) -> Result<u32> {
    if ps == 0 || !file.length.is_multiple_of(ps) {
        return Err(loc.corrupt(
            None,
            format!(
                "index file length {} is not a multiple of the page size",
                file.length
            ),
        ));
    }
    u32::try_from(file.length / ps).map_err(|_| loc.corrupt(None, "index file is too long"))
}

fn read_index_page(loc: IndexLoc<'_>, tx: &TxnPages, file: &IFile, no: u32) -> Result<PageBuf> {
    let data = file
        .data_pages
        .get(no as usize)
        .ok_or_else(|| loc.corrupt(Some(no), "page is beyond the index file"))?;
    tx.read_page(*data).map_err(|e| loc.adopt(Some(no), e))
}

fn first_u32(loc: IndexLoc<'_>, tx: &TxnPages, file: &IFile, no: u32) -> Result<u32> {
    u32le(&read_index_page(loc, tx, file, no)?, 0)
        .ok_or_else(|| loc.corrupt(Some(no), "page is truncated"))
}

fn write_index_page(
    loc: IndexLoc<'_>,
    tx: &mut TxnPages,
    file: &mut IFile,
    no: u32,
    at: u64,
    bytes: &[u8],
) -> Result<()> {
    let off = u64::from(no)
        .checked_mul(u64::from(tx.page_size()))
        .and_then(|o| o.checked_add(at))
        .ok_or_else(|| loc.corrupt(Some(no), "offset overflows"))?;
    file.write_at(tx, off, bytes)
        .map_err(|e| loc.adopt(Some(no), e))
}

fn alloc_index_page(loc: IndexLoc<'_>, tx: &mut TxnPages, file: &mut IFile) -> Result<u32> {
    let ps = u64::from(tx.page_size());
    let n = index_pages(loc, file, ps)?;
    let head = first_u32(loc, tx, file, 0)?;
    if head != 0 {
        if head >= n {
            return Err(loc.corrupt(
                Some(0),
                format!("free page link {head} is beyond the {n} pages"),
            ));
        }
        let next = first_u32(loc, tx, file, head)?;
        write_index_page(loc, tx, file, 0, 0, &next.to_le_bytes())?;
        return Ok(head);
    }
    let len = file
        .length
        .checked_add(ps)
        .ok_or_else(|| loc.corrupt(None, "index file is too long"))?;
    file.set_len(tx, len).map_err(|e| loc.adopt(None, e))?;
    Ok(n)
}

fn free_index_page(
    loc: IndexLoc<'_>,
    tx: &mut TxnPages,
    file: &mut IFile,
    page: u32,
) -> Result<()> {
    let ps = tx.page_size();
    let n = index_pages(loc, file, u64::from(ps))?;
    if page == 0 || page >= n {
        return Err(loc.corrupt(
            None,
            format!("page {page} cannot be freed: the file has pages 1..{n}"),
        ));
    }
    let head = first_u32(loc, tx, file, 0)?;
    let mut buf = vec![0u8; ps as usize];
    buf.get_mut(..4)
        .ok_or_else(|| loc.corrupt(Some(page), "page is truncated"))?
        .copy_from_slice(&head.to_le_bytes());
    write_index_page(loc, tx, file, page, 0, &buf)?;
    write_index_page(loc, tx, file, 0, 0, &page.to_le_bytes())
}

#[derive(Debug, Clone, Copy)]
struct Hop {
    no: u32,
    idx: usize,
    below: bool,
}

struct Step {
    no: u32,
    page: BranchPage,
    idx: usize,
    dirty: bool,
}

#[derive(Default)]
struct DeletePlan {
    links: Vec<(u32, u64, u32)>,
    pages: Vec<(u32, IndexPage, usize)>,
    freed: Vec<u32>,
}

fn used_of(page: &IndexPage, key_len: u32) -> usize {
    match page {
        IndexPage::Leaf(l) => leaf_used(l, key_len).unwrap_or(usize::MAX),
        IndexPage::Branch(_) => 0,
    }
}

fn used_after_insert(
    leaf: &LeafPage,
    used: usize,
    pos: usize,
    key: &[u8],
    row: u32,
    ps: u32,
) -> Option<usize> {
    let (p, n) = (leaf.packing, leaf.entries.len());
    let grown = p.grown_for(row);
    if !grown.is_usable() || !grown.fits_row(row) || n >= usize::from(u16::MAX) {
        return None;
    }
    let prev = pos
        .checked_sub(1)
        .and_then(|i| leaf.entries.get(i))
        .map(|(_, k)| k.as_slice());
    let next = leaf.entries.get(pos).map(|(_, k)| k.as_slice());
    let mids = used
        .checked_sub(LEAF_HEADER)?
        .checked_sub(n.checked_mul(usize::from(p.recbytes))?)?;
    let (next_after, next_now) = match next {
        Some(k) => (
            stored_key_len(Some(key), k, grown)?,
            stored_key_len(prev, k, p)?,
        ),
        None => (0, 0),
    };
    let mids = mids
        .checked_add(stored_key_len(prev, key, grown)?)?
        .checked_add(next_after)?
        .checked_sub(next_now)?;
    let elements = n.checked_add(1)?.checked_mul(usize::from(grown.recbytes))?;
    let after = LEAF_HEADER.checked_add(elements)?.checked_add(mids)?;
    leaf_fits_page(after, ps).then_some(after)
}

fn used_after_delete(leaf: &LeafPage, used: usize, pos: usize, ps: u32) -> Option<usize> {
    let (p, n) = (leaf.packing, leaf.entries.len());
    let rec = usize::from(p.recbytes);
    let prev = pos
        .checked_sub(1)
        .and_then(|i| leaf.entries.get(i))
        .map(|(_, k)| k.as_slice());
    let key = leaf.entries.get(pos).map(|(_, k)| k.as_slice())?;
    let next = leaf
        .entries
        .get(pos.checked_add(1)?)
        .map(|(_, k)| k.as_slice());
    let (next_now, next_after) = match next {
        Some(k) => (
            stored_key_len(Some(key), k, p)?,
            stored_key_len(prev, k, p)?,
        ),
        None => (0, 0),
    };
    let mids = used
        .checked_sub(LEAF_HEADER)?
        .checked_sub(n.checked_mul(rec)?)?
        .checked_sub(stored_key_len(prev, key, p)?)?
        .checked_sub(next_now)?
        .checked_add(next_after)?;
    let elements = n.checked_sub(1)?.checked_mul(rec)?;
    let after = LEAF_HEADER.checked_add(elements)?.checked_add(mids)?;
    leaf_fits_page(after, ps).then_some(after)
}

fn set_separator(e: &mut BranchEntry, key: &[u8], row: u32) {
    e.key.clear();
    e.key.extend_from_slice(key);
    e.row = row;
}

type Carry = Vec<(Vec<u8>, u32, u32)>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IndexHead {
    pub descr_page: u32,
    pub key_len: u32,
    pub root: u32,
}

pub(crate) struct IndexWriter<'a> {
    tx: &'a mut TxnPages,
    file: &'a mut IFile,
    loc: IndexLoc<'a>,
    fi: FormatInfo,
    key_len: u32,
    descr_page: u32,
    root: u32,
}

impl<'a> IndexWriter<'a> {
    pub fn open(
        tx: &'a mut TxnPages,
        ifile: &'a mut IFile,
        fi: FormatInfo,
        n_indexes: usize,
        i: usize,
        names: Option<(&'a str, &'a str)>,
    ) -> Result<IndexWriter<'a>> {
        let loc = Self::locate(tx, ifile, fi, names)?;
        let (ps, unit) = (u64::from(fi.page_size), fi.index_ptr_unit());
        let n = index_pages(loc, ifile, ps)?;
        if n == 0 {
            return Err(loc.corrupt(None, "the index file is empty"));
        }
        let header = parse_index_file_header(&read_index_page(loc, tx, ifile, 0)?, n_indexes)
            .map_err(|e| loc.decoded(0, e))?;
        let ptr = *header
            .descr_ptrs
            .get(i)
            .ok_or_else(|| loc.corrupt(Some(0), format!("no descriptor pointer for index {i}")))?;
        let descr_page = page_number(ptr, unit, ps, n).ok_or_else(|| {
            loc.corrupt(
                Some(0),
                format!("descriptor pointer {ptr:#x} does not refer to a page"),
            )
        })?;
        let descriptor = parse_descriptor(&read_index_page(loc, tx, ifile, descr_page)?)
            .map_err(|e| loc.decoded(descr_page, e))?;
        let key_len = u32::from(descriptor.key_len);
        if key_len > MAX_KEY_LEN {
            return Err(loc.corrupt(
                Some(descr_page),
                format!("key length {key_len} exceeds {MAX_KEY_LEN}"),
            ));
        }
        let root = page_number(descriptor.root, unit, ps, n).ok_or_else(|| {
            loc.corrupt(
                Some(descr_page),
                format!(
                    "root pointer {:#x} does not refer to a page",
                    descriptor.root
                ),
            )
        })?;
        Ok(IndexWriter {
            tx,
            file: ifile,
            loc,
            fi,
            key_len,
            descr_page,
            root,
        })
    }

    pub fn resume(
        tx: &'a mut TxnPages,
        ifile: &'a mut IFile,
        fi: FormatInfo,
        head: IndexHead,
        names: Option<(&'a str, &'a str)>,
    ) -> Result<IndexWriter<'a>> {
        let loc = Self::locate(tx, ifile, fi, names)?;
        Ok(IndexWriter {
            tx,
            file: ifile,
            loc,
            fi,
            key_len: head.key_len,
            descr_page: head.descr_page,
            root: head.root,
        })
    }

    fn locate(
        tx: &TxnPages,
        ifile: &IFile,
        fi: FormatInfo,
        names: Option<(&'a str, &'a str)>,
    ) -> Result<IndexLoc<'a>> {
        tx.check_alive()?;
        let loc = IndexLoc {
            header_page: ifile.header_page,
            names,
        };
        if fi.page_size != tx.page_size() {
            return Err(loc.corrupt(
                None,
                format!(
                    "page size {} differs from the database's {}",
                    fi.page_size,
                    tx.page_size()
                ),
            ));
        }
        Ok(loc)
    }

    pub fn head(&self) -> IndexHead {
        IndexHead {
            descr_page: self.descr_page,
            key_len: self.key_len,
            root: self.root,
        }
    }

    pub fn insert(&mut self, key: &[u8], row: u32) -> Result<()> {
        self.tx.check_alive()?;
        self.tx.trim_trees()?;
        self.check_key(key)?;
        let (hops, leaf_no) = self.descend(key, row)?;
        let t = self.held(leaf_no)?;
        let IndexPage::Leaf(leaf) = &t.page else {
            return Err(self.bad(Some(leaf_no), "page is not a leaf"));
        };
        if leaf.entries.is_empty() && !hops.is_empty() {
            return Err(self.bad(
                Some(leaf_no),
                "the only leaf of the tree is empty under branches",
            ));
        }
        let pos = leaf
            .entries
            .partition_point(|(r, k)| (k.as_slice(), *r) < (key, row));
        if leaf
            .entries
            .get(pos)
            .is_some_and(|(r, k)| *r == row && k.as_slice() == key)
        {
            return Err(self.bad(
                Some(leaf_no),
                format!("row {row} is indexed with this key already"),
            ));
        }
        match used_after_insert(leaf, t.used, pos, key, row, self.fi.page_size) {
            Some(after) => {
                let r = self.insert_in_place(&hops, leaf_no, pos, key, row, after);
                self.finish(r)
            }
            None => self.insert_on_copies(&hops, leaf_no, pos, key, row),
        }
    }

    fn insert_in_place(
        &mut self,
        hops: &[Hop],
        leaf_no: u32,
        pos: usize,
        key: &[u8],
        row: u32,
        after: usize,
    ) -> Result<()> {
        for &Hop { no, idx, below } in hops {
            if below && let Some(e) = self.branch_mut(no)?.entries.get_mut(idx) {
                set_separator(e, key, row);
            }
        }
        let (loc, key_len) = (self.loc, self.key_len);
        let t = self.tree_mut(leaf_no)?;
        let IndexPage::Leaf(leaf) = &mut t.page else {
            return Err(loc.corrupt(Some(leaf_no), "page is not a leaf"));
        };
        leaf.insert_at(pos, row, key.to_vec());
        t.used = after;
        debug_assert_eq!(Some(after), leaf_used(leaf, key_len), "leaf {leaf_no}");
        self.tx.touch_file(self.file.header_page, false);
        Ok(())
    }

    fn insert_on_copies(
        &mut self,
        hops: &[Hop],
        leaf_no: u32,
        pos: usize,
        key: &[u8],
        row: u32,
    ) -> Result<()> {
        let mut path = self.steps(hops)?;
        let mut leaf = self.leaf_copy(leaf_no)?;
        for (s, hop) in path.iter_mut().zip(hops) {
            if hop.below
                && let Some(e) = s.page.entries.get_mut(s.idx)
            {
                set_separator(e, key, row);
                s.dirty = true;
            }
        }
        leaf.insert_at(pos, row, key.to_vec());
        let (kl, ps) = (self.key_len, self.fi.page_size);
        let pieces = if leaf.fits(kl, ps) {
            vec![leaf]
        } else {
            split_leaf(leaf, kl, ps)
                .ok_or_else(|| self.bad(Some(leaf_no), "an entry does not fit an empty leaf"))?
        };
        let fresh = new_pages(&path, pieces.len(), self.branch_cap());
        self.check_room(fresh)?;
        let beside = self.check_split_neighbours(&path, leaf_no, &pieces)?;
        let r = self.apply(path, leaf_no, pieces, fresh, &beside);
        self.finish(r)
    }

    pub fn delete(&mut self, key: &[u8], row: u32) -> Result<bool> {
        self.tx.check_alive()?;
        self.tx.trim_trees()?;
        self.check_key(key)?;
        let (hops, leaf_no) = self.descend(key, row)?;
        let t = self.held(leaf_no)?;
        let IndexPage::Leaf(leaf) = &t.page else {
            return Err(self.bad(Some(leaf_no), "page is not a leaf"));
        };
        let pos = leaf
            .entries
            .partition_point(|(r, k)| (k.as_slice(), *r) < (key, row));
        if !leaf
            .entries
            .get(pos)
            .is_some_and(|(r, k)| *r == row && k.as_slice() == key)
        {
            return Ok(false);
        }
        let n = leaf.entries.len();
        let after = if n > 1 || hops.is_empty() {
            used_after_delete(leaf, t.used, pos, self.fi.page_size)
        } else {
            None
        };
        let last = match after {
            Some(_) if pos + 1 == n && !hops.is_empty() => pos
                .checked_sub(1)
                .and_then(|i| leaf.entries.get(i))
                .cloned(),
            _ => None,
        };
        let r = match after {
            Some(after) => self.delete_in_place(&hops, leaf_no, pos, last, after),
            None => {
                let path = self.steps(&hops)?;
                let mut leaf = self.leaf_copy(leaf_no)?;
                if pos < leaf.entries.len() {
                    leaf.entries.remove(pos);
                }
                let plan = self.plan_delete(path, leaf_no, leaf, pos)?;
                self.apply_delete(plan)
            }
        };
        self.finish(r).map(|()| true)
    }

    fn delete_in_place(
        &mut self,
        hops: &[Hop],
        leaf_no: u32,
        pos: usize,
        last: Option<(u32, Vec<u8>)>,
        after: usize,
    ) -> Result<()> {
        if let Some((row, key)) = last {
            self.raise_in_place(hops, &key, row)?;
        }
        let (loc, key_len) = (self.loc, self.key_len);
        let t = self.tree_mut(leaf_no)?;
        let IndexPage::Leaf(leaf) = &mut t.page else {
            return Err(loc.corrupt(Some(leaf_no), "page is not a leaf"));
        };
        if pos >= leaf.entries.len() {
            return Err(loc.corrupt(Some(leaf_no), "entry is missing"));
        }
        leaf.entries.remove(pos);
        t.used = after;
        debug_assert_eq!(Some(after), leaf_used(leaf, key_len), "leaf {leaf_no}");
        self.tx.touch_file(self.file.header_page, false);
        Ok(())
    }

    fn raise_in_place(&mut self, hops: &[Hop], key: &[u8], row: u32) -> Result<()> {
        for &Hop { no, idx, .. } in hops.iter().rev() {
            let b = self.branch_mut(no)?;
            let Some(e) = b.entries.get_mut(idx) else {
                return Ok(());
            };
            set_separator(e, key, row);
            if idx.saturating_add(1) != b.entries.len() {
                return Ok(());
            }
        }
        Ok(())
    }

    fn finish(&mut self, r: Result<()>) -> Result<()> {
        if r.is_ok() || self.tx.check_alive().is_err() {
            return r;
        }
        self.tx.run(|_| r)
    }

    pub fn contains_key(&mut self, key: &[u8]) -> Result<Option<u32>> {
        self.tx.trim_trees()?;
        self.check_key(key)?;
        let (n, loc) = (self.npages()?, self.loc);
        let mut no = self.root;
        let mut seen = HashSet::new();
        for _ in 0..=n {
            self.first_visit(&mut seen, no)?;
            let child = match self.page(no)? {
                IndexPage::Leaf(l) => {
                    let pos = l.entries.partition_point(|(_, k)| k.as_slice() < key);
                    return Ok(l
                        .entries
                        .get(pos)
                        .filter(|(_, k)| k.as_slice() == key)
                        .map(|(r, _)| *r));
                }
                IndexPage::Branch(b) => {
                    if b.entries.is_empty() {
                        return Err(loc.corrupt(Some(no), "a branch has no entries"));
                    }
                    let at = b.entries.partition_point(|e| e.key.as_slice() < key);
                    match b.entries.get(at) {
                        Some(e) => e.child,
                        None => return Ok(None),
                    }
                }
            };
            no = self.page_of(child, no)?;
        }
        Err(self.bad(Some(self.root), "the tree is deeper than the file"))
    }

    pub fn key_len(&self) -> u32 {
        self.key_len
    }

    pub fn descr_page(&self) -> u32 {
        self.descr_page
    }

    fn bad(&self, page: Option<u32>, what: impl Into<String>) -> Error {
        self.loc.corrupt(page, what)
    }

    fn check_key(&self, key: &[u8]) -> Result<()> {
        if key.len() != self.key_len as usize {
            return Err(self.bad(
                None,
                format!(
                    "a key of {} bytes for an index of {}-byte keys",
                    key.len(),
                    self.key_len
                ),
            ));
        }
        Ok(())
    }

    fn ps(&self) -> u64 {
        u64::from(self.fi.page_size)
    }

    fn npages(&self) -> Result<u32> {
        index_pages(self.loc, self.file, self.ps())
    }

    fn branch_cap(&self) -> usize {
        let room = self.fi.page_size.saturating_sub(BRANCH_HEADER);
        (room / self.key_len.saturating_add(BRANCH_TAIL)) as usize
    }

    fn ptr(&self, no: u32) -> Result<u32> {
        u64::from(no)
            .checked_mul(self.ps())
            .and_then(|off| off.checked_div(self.fi.index_ptr_unit()))
            .and_then(|p| u32::try_from(p).ok())
            .ok_or_else(|| self.bad(Some(no), "page has no pointer"))
    }

    fn page_of(&self, ptr: u32, holder: u32) -> Result<u32> {
        page_number(ptr, self.fi.index_ptr_unit(), self.ps(), self.npages()?).ok_or_else(|| {
            self.bad(
                Some(holder),
                format!("pointer {ptr:#x} does not refer to a tree page"),
            )
        })
    }

    fn db_page(&self, no: u32) -> Result<u32> {
        self.file
            .data_pages
            .get(no as usize)
            .copied()
            .ok_or_else(|| self.bad(Some(no), "page is beyond the index file"))
    }

    fn hold(&mut self, no: u32) -> Result<u32> {
        let db = self.db_page(no)?;
        if self.tx.tree(db, self.key_len).is_some() {
            return Ok(db);
        }
        let raw = read_index_page(self.loc, self.tx, self.file, no)?;
        let page = decode_index_page(&raw, self.key_len).map_err(|e| self.loc.decoded(no, e))?;
        let used = used_of(&page, self.key_len);
        self.tx
            .hold_tree(db, page, self.key_len, used)
            .map_err(|e| self.loc.adopt(Some(no), e))?;
        Ok(db)
    }

    fn page(&mut self, no: u32) -> Result<&IndexPage> {
        self.hold(no)?;
        Ok(&self.held(no)?.page)
    }

    fn held(&self, no: u32) -> Result<&TreePage> {
        let db = self.db_page(no)?;
        self.tx
            .tree(db, self.key_len)
            .ok_or_else(|| self.bad(Some(no), "page is not held decoded"))
    }

    fn tree_mut(&mut self, no: u32) -> Result<&mut TreePage> {
        let db = self.db_page(no)?;
        let loc = self.loc;
        self.tx.tree_mut(db).map_err(|e| loc.adopt(Some(no), e))
    }

    fn branch_mut(&mut self, no: u32) -> Result<&mut BranchPage> {
        let loc = self.loc;
        match &mut self.tree_mut(no)?.page {
            IndexPage::Branch(b) => Ok(b),
            IndexPage::Leaf(_) => Err(loc.corrupt(Some(no), "page is not a branch")),
        }
    }

    fn put(&mut self, no: u32, mut page: IndexPage, used: usize) -> Result<()> {
        match &mut page {
            IndexPage::Leaf(l) => l.entries.shrink_to_fit(),
            IndexPage::Branch(b) => b.entries.shrink_to_fit(),
        }
        let db = self.db_page(no)?;
        self.tx
            .put_tree(db, page, self.key_len, used)
            .map_err(|e| self.loc.adopt(Some(no), e))?;
        self.tx.touch_file(self.file.header_page, false);
        Ok(())
    }

    fn steps(&mut self, hops: &[Hop]) -> Result<Vec<Step>> {
        let loc = self.loc;
        let mut path = Vec::with_capacity(hops.len());
        for &Hop { no, idx, .. } in hops {
            let IndexPage::Branch(b) = self.page(no)? else {
                return Err(loc.corrupt(Some(no), "page is not a branch"));
            };
            path.push(Step {
                no,
                page: b.clone(),
                idx,
                dirty: false,
            });
        }
        Ok(path)
    }

    fn leaf_copy(&mut self, no: u32) -> Result<LeafPage> {
        let loc = self.loc;
        match self.page(no)? {
            IndexPage::Leaf(l) => Ok(l.clone()),
            IndexPage::Branch(_) => Err(loc.corrupt(Some(no), "page is not a leaf")),
        }
    }

    fn descend(&mut self, key: &[u8], row: u32) -> Result<(Vec<Hop>, u32)> {
        let (n, loc) = (self.npages()?, self.loc);
        let mut hops: Vec<Hop> = Vec::new();
        let mut seen = HashSet::new();
        let mut no = self.root;
        for _ in 0..=n {
            if hops.len() <= SCAN_HOPS {
                if hops.iter().any(|h| h.no == no) {
                    return Err(self.cycle_at(no));
                }
            } else {
                if hops.len() == SCAN_HOPS + 1 {
                    seen.extend(hops.iter().map(|h| h.no));
                }
                self.first_visit(&mut seen, no)?;
            }
            let IndexPage::Branch(b) = self.page(no)? else {
                return Ok((hops, no));
            };
            let at = b
                .entries
                .partition_point(|e| (e.key.as_slice(), e.row) < (key, row));
            let idx = at.min(b.entries.len().saturating_sub(1));
            let (child, below) = b
                .entries
                .get(idx)
                .map(|e| (e.child, (e.key.as_slice(), e.row) < (key, row)))
                .ok_or_else(|| loc.corrupt(Some(no), "a branch has no entries"))?;
            let child = self.page_of(child, no)?;
            hops.push(Hop { no, idx, below });
            no = child;
        }
        Err(self.bad(Some(self.root), "the tree is deeper than the file"))
    }

    fn first_visit(&self, seen: &mut HashSet<u32>, no: u32) -> Result<()> {
        if seen.insert(no) {
            return Ok(());
        }
        Err(self.cycle_at(no))
    }

    fn cycle_at(&self, no: u32) -> Error {
        self.bad(
            Some(no),
            "page is reached twice on the way down: the tree has a cycle",
        )
    }

    fn check_room(&self, k: usize) -> Result<()> {
        if k == 0 {
            return Ok(());
        }
        let n = self.npages()?;
        let mut free = 0usize;
        let mut cur = first_u32(self.loc, self.tx, self.file, 0)?;
        while cur != 0 && free < k {
            if cur >= n {
                return Err(self.bad(
                    Some(0),
                    format!("free page link {cur} is beyond the {n} pages"),
                ));
            }
            free += 1;
            cur = first_u32(self.loc, self.tx, self.file, cur)?;
        }
        let grow = u64::try_from(k - free)
            .ok()
            .and_then(|pages| pages.checked_mul(self.ps()))
            .and_then(|bytes| bytes.checked_add(self.file.length))
            .ok_or_else(|| self.bad(None, "index file length overflows"))?;
        self.file.check_len(self.fi, grow)
    }

    fn alloc_pages(&mut self, k: usize, busy: &mut HashSet<u32>) -> Result<Vec<u32>> {
        let mut out = Vec::with_capacity(k);
        for _ in 0..k {
            let no = alloc_index_page(self.loc, self.tx, self.file)?;
            if !busy.insert(no) {
                return Err(self.bad(Some(no), "the free list hands out a page that is in use"));
            }
            out.push(no);
        }
        Ok(out)
    }

    fn write(&mut self, no: u32, at: u64, bytes: &[u8]) -> Result<()> {
        write_index_page(self.loc, self.tx, self.file, no, at, bytes)
    }

    fn link(&self, no: u32, at: u64) -> Result<Option<u32>> {
        let db = self.db_page(no)?;
        if let Some(t) = self.tx.tree(db, self.key_len) {
            let (prev, next) = match &t.page {
                IndexPage::Leaf(l) => (l.prev, l.next),
                IndexPage::Branch(b) => (b.prev, b.next),
            };
            match at {
                PREV_AT => return Ok(Some(prev)),
                NEXT_AT => return Ok(Some(next)),
                _ => {}
            }
        }
        let raw = read_index_page(self.loc, self.tx, self.file, no)?;
        Ok(u32le(&raw, at as usize))
    }

    fn set_link(&mut self, no: u32, at: u64, ptr: u32) -> Result<()> {
        let db = self.db_page(no)?;
        let changed = self
            .tx
            .tree(db, self.key_len)
            .is_some_and(TreePage::changed);
        if !changed {
            return self.write(no, at, &ptr.to_le_bytes());
        }
        let is_prev = match at {
            PREV_AT => true,
            NEXT_AT => false,
            _ => return Err(self.bad(Some(no), format!("no level link at offset {at}"))),
        };
        let t = self.tree_mut(no)?;
        let (prev, next) = match &mut t.page {
            IndexPage::Leaf(l) => (&mut l.prev, &mut l.next),
            IndexPage::Branch(b) => (&mut b.prev, &mut b.next),
        };
        *(if is_prev { prev } else { next }) = ptr;
        self.tx.touch_file(self.file.header_page, false);
        Ok(())
    }

    fn check_leaf(&self, no: u32, p: &LeafPage) -> Result<usize> {
        leaf_used(p, self.key_len)
            .filter(|&used| leaf_fits_page(used, self.fi.page_size))
            .ok_or_else(|| self.bad(Some(no), "a leaf does not fit"))
    }

    fn check_branch(&self, no: u32, p: &BranchPage) -> Result<()> {
        if encode_branch(p, self.key_len, self.fi.page_size).is_some() {
            return Ok(());
        }
        Err(self.bad(Some(no), "a branch does not fit"))
    }

    fn links(&self, nos: &[u32], j: usize, prev: u32, next: u32) -> Result<(u32, u32)> {
        let before = match j.checked_sub(1).and_then(|i| nos.get(i)) {
            Some(&no) => self.ptr(no)?,
            None => prev,
        };
        let after = match nos.get(j.saturating_add(1)) {
            Some(&no) => self.ptr(no)?,
            None => next,
        };
        Ok((before, after))
    }

    fn relink_next(&mut self, next: u32, split: u32, last: u32) -> Result<()> {
        let no = self.page_of(next, split)?;
        let ptr = self.ptr(last)?;
        self.set_link(no, PREV_AT, ptr)
    }

    fn neighbour(&self, ptr: u32, no: u32, at: u64, busy: &HashSet<u32>) -> Result<Option<u32>> {
        if ptr == NONE {
            return Ok(None);
        }
        let side = self.page_of(ptr, no)?;
        if busy.contains(&side) {
            return Err(self.bad(
                Some(no),
                format!("page {side} beside it on its level is on the way down"),
            ));
        }
        if self.link(side, at)? != Some(self.ptr(no)?) {
            return Err(self.bad(
                Some(side),
                format!("page does not link back to page {no} beside it"),
            ));
        }
        Ok(Some(side))
    }

    fn check_split_neighbours(
        &self,
        path: &[Step],
        leaf_no: u32,
        pieces: &[LeafPage],
    ) -> Result<Vec<u32>> {
        let mut beside = Vec::new();
        if pieces.len() < 2 {
            return Ok(beside);
        }
        let busy = self.path_pages(path, leaf_no);
        let next = pieces.first().map_or(NONE, |p| p.next);
        beside.extend(self.neighbour(next, leaf_no, PREV_AT, &busy)?);
        for (i, _) in branch_splits(path, pieces.len(), self.branch_cap()) {
            let s = path
                .get(i)
                .ok_or_else(|| self.bad(Some(leaf_no), "the path is shorter than planned"))?;
            beside.extend(self.neighbour(s.page.next, s.no, PREV_AT, &busy)?);
        }
        Ok(beside)
    }

    fn path_pages(&self, path: &[Step], leaf_no: u32) -> HashSet<u32> {
        path.iter()
            .map(|s| s.no)
            .chain([0, self.descr_page, leaf_no])
            .collect()
    }

    fn plan_delete(
        &self,
        mut path: Vec<Step>,
        leaf_no: u32,
        leaf: LeafPage,
        pos: usize,
    ) -> Result<DeletePlan> {
        let mut plan = DeletePlan::default();
        let emptied = if leaf.entries.is_empty() {
            1 + path
                .iter()
                .rev()
                .take_while(|s| s.page.entries.len() == 1)
                .count()
        } else {
            0
        };
        if emptied > path.len() && !path.is_empty() {
            return self.plan_collapse(&path, leaf_no, &leaf);
        }
        if emptied == 0 || emptied > path.len() {
            if pos == leaf.entries.len()
                && let Some((r, k)) = leaf.entries.last()
            {
                raise(&mut path, k, *r);
            }
            let used = self.check_leaf(leaf_no, &leaf)?;
            plan.pages.push((leaf_no, IndexPage::Leaf(leaf), used));
        } else {
            let busy = self.path_pages(&path, leaf_no);
            let at = path
                .len()
                .checked_sub(emptied)
                .ok_or_else(|| self.bad(Some(leaf_no), "the path is shorter than planned"))?;
            let (upper, lower) = path.split_at_mut(at.saturating_add(1));
            let (keep, above) = upper
                .split_last_mut()
                .ok_or_else(|| self.bad(Some(leaf_no), "no branch stays above the leaf"))?;
            if keep.idx >= keep.page.entries.len() {
                return Err(self.bad(Some(keep.no), "entry is missing"));
            }
            keep.page.entries.remove(keep.idx);
            keep.dirty = true;
            if keep.idx == keep.page.entries.len()
                && let Some(e) = keep.page.entries.last()
            {
                raise(above, &e.key, e.row);
            }
            let gone = std::iter::once((leaf_no, leaf.prev, leaf.next))
                .chain(lower.iter().rev().map(|s| (s.no, s.page.prev, s.page.next)));
            for (no, prev, next) in gone {
                if prev == NONE && next == NONE {
                    return Err(self.bad(
                        Some(no),
                        "page leaving the tree links to no page on its level",
                    ));
                }
                self.unlink(no, prev, next, &busy, &mut plan.links)?;
                plan.freed.push(no);
            }
            let n = self.npages()?;
            let head = first_u32(self.loc, self.tx, self.file, 0)?;
            if head >= n {
                return Err(self.bad(
                    Some(0),
                    format!("free page link {head} is beyond the {n} pages"),
                ));
            }
            if head != 0 && busy.contains(&head) {
                return Err(self.bad(Some(head), "a page of the tree is on the free list"));
            }
        }
        for s in path.into_iter().filter(|s| s.dirty) {
            self.check_branch(s.no, &s.page)?;
            plan.pages.push((s.no, IndexPage::Branch(s.page), 0));
        }
        Ok(plan)
    }

    fn plan_collapse(&self, path: &[Step], leaf_no: u32, leaf: &LeafPage) -> Result<DeletePlan> {
        let (root, below) = path
            .split_first()
            .ok_or_else(|| self.bad(Some(leaf_no), "no branch above the leaf"))?;
        let mut plan = DeletePlan::default();
        let gone = std::iter::once((leaf_no, leaf.prev, leaf.next))
            .chain(below.iter().rev().map(|s| (s.no, s.page.prev, s.page.next)));
        for (no, prev, next) in gone {
            if prev != NONE || next != NONE {
                return Err(self.bad(
                    Some(no),
                    "the page of a one-entry path links to other pages on its level",
                ));
            }
            plan.freed.push(no);
        }
        let n = self.npages()?;
        let head = first_u32(self.loc, self.tx, self.file, 0)?;
        if head >= n {
            return Err(self.bad(
                Some(0),
                format!("free page link {head} is beyond the {n} pages"),
            ));
        }
        if head != 0 && self.path_pages(path, leaf_no).contains(&head) {
            return Err(self.bad(Some(head), "a page of the tree is on the free list"));
        }
        let empty = LeafPage::empty_root(self.key_len);
        let used = leaf_used(&empty, self.key_len)
            .filter(|&used| leaf_fits_page(used, self.fi.page_size))
            .ok_or_else(|| self.bad(Some(root.no), "an empty leaf does not fit a page"))?;
        plan.pages.push((root.no, IndexPage::Leaf(empty), used));
        Ok(plan)
    }

    fn unlink(
        &self,
        no: u32,
        prev: u32,
        next: u32,
        busy: &HashSet<u32>,
        links: &mut Vec<(u32, u64, u32)>,
    ) -> Result<()> {
        if prev != NONE && prev == next {
            return Err(self.bad(Some(no), "the pages before and after it are the same"));
        }
        for (ptr, at, to) in [(prev, NEXT_AT, next), (next, PREV_AT, prev)] {
            if let Some(side) = self.neighbour(ptr, no, at, busy)? {
                links.push((side, at, to));
            }
        }
        Ok(())
    }

    fn apply_delete(&mut self, plan: DeletePlan) -> Result<()> {
        for (no, at, ptr) in plan.links {
            self.set_link(no, at, ptr)?;
        }
        for (no, page, used) in plan.pages {
            self.put(no, page, used)?;
        }
        for no in plan.freed {
            free_index_page(self.loc, self.tx, self.file, no)?;
        }
        Ok(())
    }

    fn apply(
        &mut self,
        path: Vec<Step>,
        leaf_no: u32,
        pieces: Vec<LeafPage>,
        fresh: usize,
        beside: &[u32],
    ) -> Result<()> {
        let mut busy = self.path_pages(&path, leaf_no);
        busy.extend(beside);
        let mut new = self.alloc_pages(fresh, &mut busy)?.into_iter();
        let mut take = |n: usize| -> Vec<u32> { new.by_ref().take(n).collect() };

        let first = pieces
            .first()
            .ok_or_else(|| self.bad(Some(leaf_no), "a leaf split into nothing"))?;
        let (prev, next, root) = (first.prev, first.next, first.root);
        let mut nos = vec![leaf_no];
        nos.extend(take(pieces.len().saturating_sub(1)));
        if nos.len() != pieces.len() {
            return Err(self.bad(Some(leaf_no), "fewer new pages than planned"));
        }
        let mut carry: Carry = Vec::new();
        for (j, (mut p, &no)) in pieces.into_iter().zip(&nos).enumerate() {
            (p.prev, p.next) = self.links(&nos, j, prev, next)?;
            p.root = root && nos.len() == 1;
            if let Some((r, k)) = p.entries.last() {
                carry.push((k.clone(), *r, no));
            }
            let used = self.check_leaf(no, &p)?;
            self.put(no, IndexPage::Leaf(p), used)?;
        }
        if nos.len() > 1 && next != NONE {
            self.relink_next(next, leaf_no, *nos.last().unwrap_or(&leaf_no))?;
        }
        if nos.len() == 1 {
            carry.clear();
        }

        let cap = self.branch_cap();
        for mut s in path.into_iter().rev() {
            if !carry.is_empty() {
                let mut added = Vec::with_capacity(carry.len());
                for (j, (key, row, no)) in carry.drain(..).enumerate() {
                    if j == 0 {
                        let e = s
                            .page
                            .entries
                            .get_mut(s.idx)
                            .ok_or_else(|| self.loc.corrupt(Some(s.no), "entry is missing"))?;
                        e.key = key;
                        e.row = row;
                    } else {
                        added.push(BranchEntry {
                            key,
                            row,
                            child: self.ptr(no)?,
                        });
                    }
                }
                let at = s.idx.saturating_add(1).min(s.page.entries.len());
                s.page.entries.splice(at..at, added);
                s.dirty = true;
            }
            if s.page.entries.len() <= cap {
                if s.dirty {
                    self.check_branch(s.no, &s.page)?;
                    self.put(s.no, IndexPage::Branch(s.page), 0)?;
                }
                continue;
            }
            let (prev, next) = (s.page.prev, s.page.next);
            let parts = split_branch(s.page, cap);
            let mut nos = vec![s.no];
            nos.extend(take(parts.len().saturating_sub(1)));
            if nos.len() != parts.len() {
                return Err(self.bad(Some(s.no), "fewer new pages than planned"));
            }
            for (j, (mut p, &no)) in parts.into_iter().zip(&nos).enumerate() {
                (p.prev, p.next) = self.links(&nos, j, prev, next)?;
                p.root = false;
                if let Some(e) = p.entries.last() {
                    carry.push((e.key.clone(), e.row, no));
                }
                self.check_branch(no, &p)?;
                self.put(no, IndexPage::Branch(p), 0)?;
            }
            if next != NONE {
                self.relink_next(next, s.no, *nos.last().unwrap_or(&s.no))?;
            }
        }

        if !carry.is_empty() {
            let no = take(1)
                .first()
                .copied()
                .ok_or_else(|| self.bad(None, "fewer new pages than planned"))?;
            let mut entries = Vec::with_capacity(carry.len());
            for (key, row, child) in carry {
                entries.push(BranchEntry {
                    key,
                    row,
                    child: self.ptr(child)?,
                });
            }
            let page = BranchPage {
                root: true,
                prev: NONE,
                next: NONE,
                entries,
            };
            self.check_branch(no, &page)?;
            self.put(no, IndexPage::Branch(page), 0)?;
            let ptr = self.ptr(no)?;
            self.write(self.descr_page, 0, &ptr.to_le_bytes())?;
            self.root = no;
        }
        if new.next().is_some() {
            return Err(self.bad(None, "more new pages than used"));
        }
        Ok(())
    }
}

fn raise(path: &mut [Step], key: &[u8], row: u32) {
    for s in path.iter_mut().rev() {
        let Some(e) = s.page.entries.get_mut(s.idx) else {
            return;
        };
        e.key = key.to_vec();
        e.row = row;
        s.dirty = true;
        if s.idx.saturating_add(1) != s.page.entries.len() {
            return;
        }
    }
}

fn new_pages(path: &[Step], leaf_pieces: usize, cap: usize) -> usize {
    let splits = branch_splits(path, leaf_pieces, cap);
    let extra = splits
        .iter()
        .fold(leaf_pieces.saturating_sub(1), |t, (_, k)| {
            t.saturating_add(k.saturating_sub(1))
        });
    let root_splits =
        leaf_pieces > 1 && (path.is_empty() || splits.last().is_some_and(|(i, _)| *i == 0));
    extra.saturating_add(usize::from(root_splits))
}

fn branch_splits(path: &[Step], leaf_pieces: usize, cap: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut extra = leaf_pieces.saturating_sub(1);
    for (i, s) in path.iter().enumerate().rev() {
        let n = s.page.entries.len().saturating_add(extra);
        if extra == 0 || n <= cap {
            break;
        }
        let k = branch_pieces(n, cap);
        out.push((i, k));
        extra = k.saturating_sub(1);
    }
    out
}

fn branch_pieces(n: usize, cap: usize) -> usize {
    n.div_ceil(cap.max(1)).max(1)
}

fn split_branch(b: BranchPage, cap: usize) -> Vec<BranchPage> {
    let BranchPage {
        root,
        prev,
        next,
        mut entries,
    } = b;
    let k = branch_pieces(entries.len(), cap);
    let (base, larger) = (entries.len() / k, entries.len() % k);
    let mut out = Vec::with_capacity(k);
    for j in (0..k).rev() {
        let size = base + usize::from(j >= k - larger);
        let tail = entries.split_off(entries.len().saturating_sub(size));
        out.push(BranchPage {
            root,
            prev,
            next,
            entries: tail,
        });
    }
    out.reverse();
    out
}

fn split_leaf(leaf: LeafPage, kl: u32, ps: u32) -> Option<Vec<LeafPage>> {
    let LeafPage {
        root,
        prev,
        next,
        packing,
        mut entries,
    } = leaf;
    let piece = |entries: Vec<(u32, Vec<u8>)>| LeafPage {
        root,
        prev,
        next,
        packing,
        entries,
    };
    let half = entries.len() / 2;
    if half > 0 {
        let right = piece(entries.split_off(half));
        let left = piece(entries);
        if left.fits(kl, ps) && right.fits(kl, ps) {
            return Some(vec![left, right]);
        }
        entries = left.entries;
        entries.extend(right.entries);
    }
    let fits = |e: &[(u32, Vec<u8>)]| piece(e.to_vec()).fits(kl, ps);
    let mut out = Vec::new();
    while !entries.is_empty() {
        let (mut lo, mut hi) = (0usize, entries.len());
        while lo < hi {
            let mid = lo + (hi - lo).div_ceil(2);
            if fits(entries.get(..mid)?) {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        if lo == 0 {
            return None;
        }
        let rest = entries.split_off(lo);
        out.push(piece(entries));
        entries = rest;
    }
    Some(out)
}
