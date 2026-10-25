use super::bytes::{OrCorrupt, put_u16le, put_u32be, put_u32le, sub, u16le, u32be, u32le, u32s_le};
use crate::error::{Error, Location, Result};

pub const NONE: u32 = 0xFFFF_FFFF;

const COMMON_HEADER: usize = 12;
pub const LEAF_HEADER: usize = 30;
const BRANCH_TAIL: usize = 8;
const DESCRIPTOR_LEN: usize = 12;
pub const DESCRIPTOR_FLAG: u8 = 0x80;
const FLAG_ROOT: u16 = 1;
const FLAG_LEAF: u16 = 2;

fn bad(what: impl Into<String>) -> Error {
    Error::corrupt(Location::File, what)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexFileHeader {
    pub free_head: u32,
    pub descr_ptrs: Vec<u32>,
}

pub fn parse_index_file_header(page0: &[u8], n_indexes: usize) -> Result<IndexFileHeader> {
    let free_head =
        u32le(page0, 0).or_corrupt(|| Location::File, "index file header page is truncated")?;
    let descr_ptrs = u32s_le(page0, 4, n_indexes).or_corrupt(
        || Location::File,
        "the index file header page has no room for all the indexes",
    )?;
    Ok(IndexFileHeader {
        free_head,
        descr_ptrs,
    })
}

pub(crate) fn parse_index_descriptor_pointer(page: &[u8], count: usize, pos: usize) -> Result<u32> {
    let end = count
        .checked_mul(4)
        .and_then(|n| n.checked_add(4))
        .ok_or_else(|| bad("index header length overflow"))?;
    if page.get(..end).is_none() || pos >= count {
        return Err(bad("index header has no room for descriptor pointers"));
    }
    let at = pos
        .checked_mul(4)
        .and_then(|n| n.checked_add(4))
        .ok_or_else(|| bad("descriptor pointer offset overflow"))?;
    u32le(page, at).ok_or_else(|| bad("descriptor pointer is truncated"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexDescriptor {
    pub root: u32,
    pub key_len: u16,
    pub flag: u8,
}

pub fn parse_descriptor(b: &[u8]) -> Result<IndexDescriptor> {
    let head = sub(b, 0, DESCRIPTOR_LEN)
        .or_corrupt(|| Location::File, "index descriptor page is truncated")?;
    match (u32le(head, 0), u16le(head, 4), head.get(6)) {
        (Some(root), Some(key_len), Some(&flag)) => Ok(IndexDescriptor {
            root,
            key_len,
            flag,
        }),
        _ => Err(bad("index descriptor is truncated")),
    }
}

pub fn encode_descriptor(d: &IndexDescriptor, page: &mut [u8]) {
    let Some(head) = page.get_mut(..DESCRIPTOR_LEN) else {
        return;
    };
    head.fill(0);
    let _ = put_u32le(head, 0, d.root);
    let _ = put_u16le(head, 4, d.key_len);
    if let Some(flag) = head.get_mut(6) {
        *flag = d.flag;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LeafPacking {
    pub nb: u16,
    pub lb: u16,
    pub rb: u16,
    pub recbytes: u16,
}

impl LeafPacking {
    pub fn initial(key_len: u32) -> LeafPacking {
        if key_len >= 256 {
            return LeafPacking {
                nb: 32,
                lb: 16,
                rb: 16,
                recbytes: 8,
            };
        }
        let bits = (u32::BITS - key_len.leading_zeros()) as u16;
        let mut nb = 12;
        while !(nb + 2 * bits).is_multiple_of(8) {
            nb += 1;
        }
        LeafPacking {
            nb,
            lb: bits,
            rb: bits,
            recbytes: (nb + 2 * bits) / 8,
        }
    }

    pub fn fits_row(&self, row: u32) -> bool {
        u64::from(row)
            .checked_shr(u32::from(self.nb))
            .is_none_or(|rest| rest == 0)
    }

    pub fn grown_for(&self, row: u32) -> LeafPacking {
        let mut p = *self;
        while !p.fits_row(row) && p.nb < 32 {
            p.nb = (p.nb + 8).min(32);
            p.recbytes = p.recbytes.saturating_add(1);
        }
        p
    }

    pub fn is_usable(&self) -> bool {
        self.masks().is_some()
    }

    fn masks(&self) -> Option<(u32, u16, u16)> {
        let bits = u32::from(self.nb) + u32::from(self.lb) + u32::from(self.rb);
        if self.nb > 32
            || self.lb > 16
            || self.rb > 16
            || !(1..=8).contains(&self.recbytes)
            || bits > u32::from(self.recbytes) * 8
        {
            return None;
        }
        let mask = |n: u16| (1u64 << n) - 1;
        Some((
            u32::try_from(mask(self.nb)).ok()?,
            u16::try_from(mask(self.lb)).ok()?,
            u16::try_from(mask(self.rb)).ok()?,
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeafPage {
    pub root: bool,
    pub prev: u32,
    pub next: u32,
    pub packing: LeafPacking,
    pub entries: Vec<(u32, Vec<u8>)>,
}

impl LeafPage {
    pub fn empty_root(key_len: u32) -> LeafPage {
        LeafPage {
            root: true,
            prev: NONE,
            next: NONE,
            packing: LeafPacking::initial(key_len),
            entries: Vec::new(),
        }
    }

    pub fn insert_at(&mut self, pos: usize, row: u32, key: Vec<u8>) {
        self.packing = self.packing.grown_for(row);
        let pos = pos.min(self.entries.len());
        self.entries.insert(pos, (row, key));
    }

    pub fn fits(&self, key_len: u32, page_size: u32) -> bool {
        leaf_used(self, key_len).is_some_and(|used| leaf_fits_page(used, page_size))
    }
}

pub fn leaf_fits_page(used: usize, page_size: u32) -> bool {
    usize::try_from(page_size)
        .ok()
        .and_then(|ps| ps.checked_sub(used))
        .is_some_and(|free| free <= usize::from(u16::MAX))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchEntry {
    pub key: Vec<u8>,
    pub row: u32,
    pub child: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchPage {
    pub root: bool,
    pub prev: u32,
    pub next: u32,
    pub entries: Vec<BranchEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexPage {
    Leaf(LeafPage),
    Branch(BranchPage),
}

pub fn leaf_flag(page: &[u8]) -> Option<bool> {
    let flags = u16le(page, 0)?;
    (flags & !(FLAG_ROOT | FLAG_LEAF) == 0).then_some(flags & FLAG_LEAF != 0)
}

pub fn decode_index_page(page: &[u8], key_len: u32) -> Result<IndexPage> {
    let short = || bad("index page is truncated");
    let flags = u16le(page, 0).ok_or_else(short)?;
    if flags & !(FLAG_ROOT | FLAG_LEAF) != 0 {
        return Err(bad(format!("unknown page flags {flags:#06x}")));
    }
    let root = flags & FLAG_ROOT != 0;
    let count = usize::from(u16le(page, 2).ok_or_else(short)?);
    let prev = u32le(page, 4).ok_or_else(short)?;
    let next = u32le(page, 8).ok_or_else(short)?;
    let key_len = usize::try_from(key_len).map_err(|_| bad("key length is too large"))?;
    if flags & FLAG_LEAF != 0 {
        decode_leaf(page, key_len, count, root, prev, next).map(IndexPage::Leaf)
    } else {
        decode_branch(page, key_len, count, root, prev, next).map(IndexPage::Branch)
    }
}

fn decode_leaf(
    page: &[u8],
    key_len: usize,
    count: usize,
    root: bool,
    prev: u32,
    next: u32,
) -> Result<LeafPage> {
    let short = || bad("leaf header is truncated");
    let free = usize::from(u16le(page, 12).ok_or_else(short)?);
    let record_mask = u32le(page, 14).ok_or_else(short)?;
    let left_mask = u16le(page, 18).ok_or_else(short)?;
    let right_mask = u16le(page, 20).ok_or_else(short)?;
    let packing = LeafPacking {
        nb: u16le(page, 22).ok_or_else(short)?,
        lb: u16le(page, 24).ok_or_else(short)?,
        rb: u16le(page, 26).ok_or_else(short)?,
        recbytes: u16le(page, 28).ok_or_else(short)?,
    };
    let Some(masks) = packing.masks() else {
        return Err(bad(format!("unusable leaf packing {packing:?}")));
    };
    if masks != (record_mask, left_mask, right_mask) {
        return Err(bad(format!(
            "leaf masks {record_mask:#x}/{left_mask:#x}/{right_mask:#x} do not match the packing {packing:?}"
        )));
    }

    let rec = usize::from(packing.recbytes);
    let elems_len = count
        .checked_mul(rec)
        .ok_or_else(|| bad("leaf element area overflows"))?;
    let elems = sub(page, LEAF_HEADER, elems_len)
        .ok_or_else(|| bad(format!("{count} leaf elements do not fit the page")))?;
    let elems_end = LEAF_HEADER + elems_len;

    let (nb, lb) = (u32::from(packing.nb), u32::from(packing.lb));
    let mut entries: Vec<(u32, Vec<u8>)> = Vec::with_capacity(count);
    let mut keys_at = page.len();
    let mut prev_stored = 0usize;
    for chunk in elems.chunks_exact(rec) {
        let v = chunk
            .iter()
            .rev()
            .fold(0u64, |acc, &b| (acc << 8) | u64::from(b));
        let recno = u32::try_from(v & u64::from(record_mask)).map_err(|_| bad("record number"))?;
        let left = usize::try_from((v >> nb) & u64::from(left_mask)).unwrap_or(usize::MAX);
        let right = usize::try_from((v >> (nb + lb)) & u64::from(right_mask)).unwrap_or(usize::MAX);
        let i = entries.len();
        let mid = key_len
            .checked_sub(left)
            .and_then(|n| n.checked_sub(right))
            .ok_or_else(|| {
                bad(format!(
                    "element {i}: left {left} + right {right} exceed the key length {key_len}"
                ))
            })?;
        if left > prev_stored {
            return Err(bad(format!(
                "element {i}: left {left} exceeds the {prev_stored} stored bytes of the previous key"
            )));
        }
        keys_at = keys_at
            .checked_sub(mid)
            .filter(|&at| at >= elems_end)
            .ok_or_else(|| bad(format!("element {i}: the keys overrun the elements")))?;
        let stored =
            sub(page, keys_at, mid).ok_or_else(|| bad("key bytes are outside the page"))?;
        let mut key = Vec::with_capacity(key_len);
        if let Some((_, p)) = entries.last() {
            key.extend_from_slice(
                p.get(..left)
                    .ok_or_else(|| bad("left is outside the previous key"))?,
            );
        }
        key.extend_from_slice(stored);
        key.resize(key_len, 0);
        prev_stored = key_len - right;
        entries.push((recno, key));
    }

    let used = elems_end + (page.len() - keys_at);
    if used.checked_add(free) != Some(page.len()) {
        return Err(bad(format!(
            "leaf free bytes {free} do not match its content of {used} bytes in {}",
            page.len()
        )));
    }
    Ok(LeafPage {
        root,
        prev,
        next,
        packing,
        entries,
    })
}

fn decode_branch(
    page: &[u8],
    key_len: usize,
    count: usize,
    root: bool,
    prev: u32,
    next: u32,
) -> Result<BranchPage> {
    let size = key_len
        .checked_add(BRANCH_TAIL)
        .ok_or_else(|| bad("key length is too large"))?;
    let len = count
        .checked_mul(size)
        .ok_or_else(|| bad("branch entry area overflows"))?;
    let area = sub(page, COMMON_HEADER, len)
        .ok_or_else(|| bad(format!("{count} branch entries do not fit the page")))?;
    let mut entries = Vec::with_capacity(count);
    for chunk in area.chunks_exact(size) {
        match (
            chunk.get(..key_len),
            u32be(chunk, key_len),
            u32be(chunk, key_len + 4),
        ) {
            (Some(key), Some(row), Some(child)) => entries.push(BranchEntry {
                key: key.to_vec(),
                row,
                child,
            }),
            _ => return Err(bad("branch entry is truncated")),
        }
    }
    Ok(BranchPage {
        root,
        prev,
        next,
        entries,
    })
}

fn managed_bad(what: &'static str) -> Error {
    Error::CorruptStatic {
        location: Location::File,
        what,
    }
}

pub(crate) fn decode_index_page_budgeted(
    page: &[u8],
    key_len: u32,
    budget: &memory_budget::MemoryBudget,
) -> Result<memory_budget::Charged<IndexPage>> {
    let short = || managed_bad("index page is truncated");
    let flags = u16le(page, 0).ok_or_else(short)?;
    if flags & !(FLAG_ROOT | FLAG_LEAF) != 0 {
        return Err(managed_bad("invalid index page"));
    }
    let root = flags & FLAG_ROOT != 0;
    let count = usize::from(u16le(page, 2).ok_or_else(short)?);
    let prev = u32le(page, 4).ok_or_else(short)?;
    let next = u32le(page, 8).ok_or_else(short)?;
    let key_len = usize::try_from(key_len).map_err(|_| managed_bad("key length is too large"))?;
    if flags & FLAG_LEAF != 0 {
        decode_leaf_budgeted(page, key_len, count, root, prev, next, budget)
            .map(|v| v.map(IndexPage::Leaf))
    } else {
        decode_branch_budgeted(page, key_len, count, root, prev, next, budget)
            .map(|v| v.map(IndexPage::Branch))
    }
}

#[allow(clippy::too_many_arguments)]
fn decode_leaf_budgeted(
    page: &[u8],
    key_len: usize,
    count: usize,
    root: bool,
    prev: u32,
    next: u32,
    budget: &memory_budget::MemoryBudget,
) -> Result<memory_budget::Charged<LeafPage>> {
    let short = || managed_bad("leaf header is truncated");
    let free = usize::from(u16le(page, 12).ok_or_else(short)?);
    let record_mask = u32le(page, 14).ok_or_else(short)?;
    let left_mask = u16le(page, 18).ok_or_else(short)?;
    let right_mask = u16le(page, 20).ok_or_else(short)?;
    let packing = LeafPacking {
        nb: u16le(page, 22).ok_or_else(short)?,
        lb: u16le(page, 24).ok_or_else(short)?,
        rb: u16le(page, 26).ok_or_else(short)?,
        recbytes: u16le(page, 28).ok_or_else(short)?,
    };
    let Some(masks) = packing.masks() else {
        return Err(managed_bad("invalid index page"));
    };
    if masks != (record_mask, left_mask, right_mask) {
        return Err(managed_bad("invalid index page"));
    }

    let rec = usize::from(packing.recbytes);
    let elems_len = count
        .checked_mul(rec)
        .ok_or_else(|| managed_bad("leaf element area overflows"))?;
    let elems =
        sub(page, LEAF_HEADER, elems_len).ok_or_else(|| managed_bad("invalid index page"))?;
    let elems_end = LEAF_HEADER + elems_len;

    let (nb, lb) = (u32::from(packing.nb), u32::from(packing.lb));
    let element = |chunk: &[u8]| -> Result<(u32, usize, usize)> {
        let v = chunk
            .iter()
            .rev()
            .fold(0u64, |acc, &b| (acc << 8) | u64::from(b));
        let recno =
            u32::try_from(v & u64::from(record_mask)).map_err(|_| managed_bad("record number"))?;
        let left = usize::try_from((v >> nb) & u64::from(left_mask)).unwrap_or(usize::MAX);
        let right = usize::try_from((v >> (nb + lb)) & u64::from(right_mask)).unwrap_or(usize::MAX);
        let mid = key_len
            .checked_sub(left)
            .and_then(|n| n.checked_sub(right))
            .ok_or_else(|| managed_bad("invalid index page"))?;
        Ok((recno, left, mid))
    };

    let mut keys_at = page.len();
    let mut prev_stored = 0usize;
    for chunk in elems.chunks_exact(rec) {
        let (_, left, mid) = element(chunk)?;
        if left > prev_stored {
            return Err(managed_bad("invalid index page"));
        }
        keys_at = keys_at
            .checked_sub(mid)
            .filter(|&at| at >= elems_end)
            .ok_or_else(|| managed_bad("invalid index page"))?;
        sub(page, keys_at, mid).ok_or_else(|| managed_bad("key bytes are outside the page"))?;
        prev_stored = left + mid;
    }
    let used = elems_end + (page.len() - keys_at);
    if used.checked_add(free) != Some(page.len()) {
        return Err(managed_bad("invalid index page"));
    }

    let per_key = key_len
        .checked_add(std::mem::size_of::<memory_budget::BudgetedVec<u8>>())
        .ok_or(memory_budget::BudgetError::Overflow)?;
    let charge = count
        .checked_mul(std::mem::size_of::<(u32, Vec<u8>)>())
        .and_then(|n| {
            n.checked_add(std::mem::size_of::<
                memory_budget::BudgetedVec<(u32, Vec<u8>)>,
            >())
        })
        .and_then(|n| count.checked_mul(per_key).and_then(|k| n.checked_add(k)))
        .and_then(|n| u64::try_from(n).ok())
        .ok_or(memory_budget::BudgetError::Overflow)?;
    budget.try_prepaid(memory_budget::Category::ReaderWorking, charge, |grant| {
        let mut entries = grant.try_vec_unowned::<(u32, Vec<u8>)>(count)?;
        let mut keys_at = page.len();
        for chunk in elems.chunks_exact(rec) {
            let (recno, left, mid) = element(chunk)?;
            keys_at = keys_at
                .checked_sub(mid)
                .ok_or_else(|| managed_bad("invalid index page"))?;
            let stored = sub(page, keys_at, mid)
                .ok_or_else(|| managed_bad("key bytes are outside the page"))?;
            let mut key = grant.try_vec_unowned::<u8>(key_len)?;
            if let Some((_, p)) = entries.last() {
                memory_budget::extend_within(
                    &mut key,
                    p.get(..left)
                        .ok_or_else(|| managed_bad("left is outside the previous key"))?,
                )?;
            }
            memory_budget::extend_within(&mut key, stored)?;
            memory_budget::resize_within(&mut key, key_len, 0)?;
            if entries.len() == entries.capacity() {
                return Err(managed_bad("invalid index page"));
            }
            entries.push((recno, key));
        }
        Ok(LeafPage {
            root,
            prev,
            next,
            packing,
            entries,
        })
    })
}

#[allow(clippy::too_many_arguments)]
fn decode_branch_budgeted(
    page: &[u8],
    key_len: usize,
    count: usize,
    root: bool,
    prev: u32,
    next: u32,
    budget: &memory_budget::MemoryBudget,
) -> Result<memory_budget::Charged<BranchPage>> {
    let size = key_len
        .checked_add(BRANCH_TAIL)
        .ok_or_else(|| managed_bad("key length is too large"))?;
    let len = count
        .checked_mul(size)
        .ok_or_else(|| managed_bad("branch entry area overflows"))?;
    let area = sub(page, COMMON_HEADER, len).ok_or_else(|| managed_bad("invalid index page"))?;
    let per_key = key_len
        .checked_add(std::mem::size_of::<memory_budget::BudgetedVec<u8>>())
        .ok_or(memory_budget::BudgetError::Overflow)?;
    let charge = count
        .checked_mul(std::mem::size_of::<BranchEntry>())
        .and_then(|n| n.checked_add(std::mem::size_of::<memory_budget::BudgetedVec<BranchEntry>>()))
        .and_then(|n| count.checked_mul(per_key).and_then(|k| n.checked_add(k)))
        .and_then(|n| u64::try_from(n).ok())
        .ok_or(memory_budget::BudgetError::Overflow)?;
    budget.try_prepaid(memory_budget::Category::ReaderWorking, charge, |grant| {
        let mut entries = grant.try_vec_unowned::<BranchEntry>(count)?;
        for chunk in area.chunks_exact(size) {
            match (
                chunk.get(..key_len),
                u32be(chunk, key_len),
                u32be(chunk, key_len + 4),
            ) {
                (Some(key), Some(row), Some(child)) => {
                    let mut owned = grant.try_vec_unowned::<u8>(key_len)?;
                    memory_budget::extend_within(&mut owned, key)?;
                    if entries.len() == entries.capacity() {
                        return Err(managed_bad("invalid index page"));
                    }
                    entries.push(BranchEntry {
                        key: owned,
                        row,
                        child,
                    });
                }
                _ => return Err(managed_bad("branch entry is truncated")),
            }
        }
        Ok(BranchPage {
            root,
            prev,
            next,
            entries,
        })
    })
}

fn without_right(key: &[u8], right_mask: u16) -> &[u8] {
    let zeros = key
        .iter()
        .rev()
        .take(usize::from(right_mask))
        .take_while(|&&b| b == 0)
        .count();
    key.get(..key.len() - zeros).unwrap_or(key)
}

fn stored_len(prev_kept: &[u8], kept: &[u8], left_mask: u16) -> usize {
    let left = kept
        .iter()
        .zip(prev_kept)
        .take(usize::from(left_mask))
        .take_while(|(a, b)| a == b)
        .count();
    kept.len() - left
}

pub fn stored_key_len(prev: Option<&[u8]>, key: &[u8], packing: LeafPacking) -> Option<usize> {
    let (_, left_mask, right_mask) = packing.masks()?;
    let prev_kept = prev.map_or(&[][..], |prev| without_right(prev, right_mask));
    Some(stored_len(
        prev_kept,
        without_right(key, right_mask),
        left_mask,
    ))
}

pub fn leaf_used(p: &LeafPage, key_len: u32) -> Option<usize> {
    let key_len = usize::try_from(key_len).ok()?;
    let (record_mask, left_mask, right_mask) = p.packing.masks()?;
    if p.entries.len() > usize::from(u16::MAX) {
        return None;
    }
    let elements = p
        .entries
        .len()
        .checked_mul(usize::from(p.packing.recbytes))?;
    let mut used = LEAF_HEADER.checked_add(elements)?;
    let mut prev_kept: &[u8] = &[];
    for (recno, key) in &p.entries {
        if key.len() != key_len || *recno > record_mask {
            return None;
        }
        let kept = without_right(key, right_mask);
        used = used.checked_add(stored_len(prev_kept, kept, left_mask))?;
        prev_kept = kept;
    }
    Some(used)
}

pub fn encode_leaf(p: &LeafPage, key_len: u32, page_size: u32) -> Option<Vec<u8>> {
    let key_len = usize::try_from(key_len).ok()?;
    let ps = usize::try_from(page_size).ok()?;
    let (record_mask, left_mask, right_mask) = p.packing.masks()?;
    let count = u16::try_from(p.entries.len()).ok()?;
    let rec = usize::from(p.packing.recbytes);
    let (nb, lb) = (u32::from(p.packing.nb), u32::from(p.packing.lb));
    let elems_end = LEAF_HEADER.checked_add(p.entries.len().checked_mul(rec)?)?;
    let mut page = vec![0u8; ps];
    let mut keys_at = ps;
    let mut prev_stored: &[u8] = &[];
    for (i, (recno, key)) in p.entries.iter().enumerate() {
        if key.len() != key_len || *recno > record_mask {
            return None;
        }
        let zeros = key.iter().rev().take_while(|&&b| b == 0).count();
        let right = zeros.min(usize::from(right_mask));
        let shared = key
            .iter()
            .zip(prev_stored)
            .take_while(|(a, b)| a == b)
            .count();
        let left = shared
            .min(usize::from(left_mask))
            .min(key_len.checked_sub(right)?);
        let mid = key_len - left - right;
        keys_at = keys_at.checked_sub(mid).filter(|&at| at >= elems_end)?;
        page.get_mut(keys_at..keys_at + mid)?
            .copy_from_slice(key.get(left..left + mid)?);
        let packed = u64::from(*recno)
            | (u64::try_from(left).ok()? << nb)
            | (u64::try_from(right).ok()? << (nb + lb));
        let at = LEAF_HEADER + i * rec;
        page.get_mut(at..at + rec)?
            .copy_from_slice(packed.to_le_bytes().get(..rec)?);
        prev_stored = key.get(..key_len - right)?;
    }
    let free = u16::try_from(keys_at.checked_sub(elems_end)?).ok()?;
    let flags = FLAG_LEAF | if p.root { FLAG_ROOT } else { 0 };
    put_u16le(&mut page, 0, flags)?;
    put_u16le(&mut page, 2, count)?;
    put_u32le(&mut page, 4, p.prev)?;
    put_u32le(&mut page, 8, p.next)?;
    put_u16le(&mut page, 12, free)?;
    put_u32le(&mut page, 14, record_mask)?;
    put_u16le(&mut page, 18, left_mask)?;
    put_u16le(&mut page, 20, right_mask)?;
    put_u16le(&mut page, 22, p.packing.nb)?;
    put_u16le(&mut page, 24, p.packing.lb)?;
    put_u16le(&mut page, 26, p.packing.rb)?;
    put_u16le(&mut page, 28, p.packing.recbytes)?;
    Some(page)
}

pub fn encode_branch(p: &BranchPage, key_len: u32, page_size: u32) -> Option<Vec<u8>> {
    let key_len = usize::try_from(key_len).ok()?;
    let ps = usize::try_from(page_size).ok()?;
    let count = u16::try_from(p.entries.len()).ok()?;
    let size = key_len.checked_add(BRANCH_TAIL)?;
    let end = COMMON_HEADER.checked_add(p.entries.len().checked_mul(size)?)?;
    if end > ps {
        return None;
    }
    let mut page = vec![0u8; ps];
    for (i, e) in p.entries.iter().enumerate() {
        if e.key.len() != key_len {
            return None;
        }
        let at = COMMON_HEADER + i * size;
        page.get_mut(at..at + key_len)?.copy_from_slice(&e.key);
        put_u32be(&mut page, at + key_len, e.row)?;
        put_u32be(&mut page, at + key_len + 4, e.child)?;
    }
    put_u16le(&mut page, 0, if p.root { FLAG_ROOT } else { 0 })?;
    put_u16le(&mut page, 2, count)?;
    put_u32le(&mut page, 4, p.prev)?;
    put_u32le(&mut page, 8, p.next)?;
    Some(page)
}
