use std::collections::HashSet;
use std::ops::Range;

use crate::error::{Error, Location, Result};
use crate::format::blob::{BLOCK, BLOCK_DATA, BlobBlock, decode_block, encode_block};
use crate::format::bytes::OrCorrupt;
use crate::format::value::BlobRef;
use crate::ifile::IFile;
use crate::pager::PageRead;
use crate::pager::txn::TxnPages;

#[derive(Debug, Clone)]
pub(crate) struct BlobStore {
    pub file: IFile,
    table: Option<String>,
    free: Option<FreeSet>,
}

#[derive(Debug, Clone)]
struct FreeSet {
    head: u32,
    blocks: HashSet<u32>,
}

#[derive(Debug, Default)]
pub(crate) struct ChainWriter {
    start: u32,
    cur: u32,
    buf: Vec<u8>,
    len: u64,
    taken: HashSet<u32>,
}

impl BlobStore {
    pub fn new(file: IFile) -> Result<BlobStore> {
        if !file.length.is_multiple_of(BLOCK as u64) {
            if file.is_managed() {
                return Err(Error::CorruptStatic {
                    location: Location::InternalFile(file.header_page),
                    what: "blob file length is not a multiple of its block size",
                });
            }
            return Err(Error::corrupt(
                Location::InternalFile(file.header_page),
                format!(
                    "blob file length {} is not a multiple of {BLOCK}",
                    file.length
                ),
            ));
        }
        Ok(BlobStore {
            file,
            table: None,
            free: None,
        })
    }

    pub fn named(mut self, table: &str) -> BlobStore {
        self.table = Some(table.to_string());
        self.file.owner = Some((table.to_string(), "blob"));
        self
    }

    fn location(&self, block: u32) -> Location {
        match &self.table {
            Some(table) => Location::Blob {
                table: table.clone(),
                block,
            },
            None => Location::InternalFile(self.file.header_page),
        }
    }

    fn corrupt(&self, block: u32, what: String) -> Error {
        Error::corrupt(self.location(block), what)
    }

    pub fn nblocks(&self) -> u32 {
        u32::try_from(self.file.length / BLOCK as u64).unwrap_or(u32::MAX)
    }

    fn check_range(&self, i: u32) -> Result<()> {
        if i >= self.nblocks() {
            return Err(self.corrupt(
                i,
                format!("block {i} is out of range ({} blocks)", self.nblocks()),
            ));
        }
        Ok(())
    }

    fn relocate(&self, block: u32, e: Error) -> Error {
        match (&self.table, e) {
            (Some(_), Error::Corrupt { location, what }) => {
                self.corrupt(block, format!("block {block}: {what} ({location})"))
            }
            (_, other) => other,
        }
    }

    fn read_raw(&self, pr: &dyn PageRead, i: u32, buf: &mut [u8]) -> Result<()> {
        self.check_range(i)?;
        self.file
            .read_at(pr, u64::from(i) * BLOCK as u64, buf)
            .map_err(|e| self.relocate(i, e))
    }

    pub fn block(&self, pr: &dyn PageRead, i: u32) -> Result<BlobBlock> {
        let mut raw = [0u8; BLOCK];
        self.read_raw(pr, i, &mut raw)?;
        decode_block(&raw).map_err(|e| match e {
            Error::Corrupt { what, .. } => self.corrupt(i, format!("block {i}: {what}")),
            other => other,
        })
    }

    fn next_of(&self, pr: &dyn PageRead, i: u32) -> Result<u32> {
        let mut raw = [0u8; 4];
        self.read_raw(pr, i, &mut raw)?;
        Ok(u32::from_le_bytes(raw))
    }

    fn follow(&self, start: u32, mut step: impl FnMut(u32) -> Result<u32>) -> Result<()> {
        let mut seen = HashSet::new();
        let mut cur = start;
        while cur != 0 {
            self.check_range(cur)?;
            if !seen.insert(cur) {
                return Err(self.corrupt(
                    cur,
                    format!("chain from block {start} has a cycle at block {cur}"),
                ));
            }
            cur = step(cur)?;
        }
        Ok(())
    }

    pub fn chain(&self, pr: &dyn PageRead, start: u32) -> Result<Vec<u32>> {
        let mut out = Vec::new();
        self.follow(start, |b| {
            out.push(b);
            self.next_of(pr, b)
        })?;
        Ok(out)
    }

    pub fn read_chain(&self, pr: &dyn PageRead, start: u32) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        self.follow(start, |b| {
            let blk = self.block(pr, b)?;
            out.extend_from_slice(blk.bytes());
            Ok(blk.next)
        })?;
        Ok(out)
    }

    fn managed_corrupt(&self, what: &'static str) -> Error {
        Error::CorruptStatic {
            location: Location::InternalFile(self.file.header_page),
            what,
        }
    }
    fn block_managed(&self, pr: &dyn PageRead, i: u32) -> Result<BlobBlock> {
        if i >= self.nblocks() {
            return Err(self.managed_corrupt("blob block is out of range"));
        }
        let mut raw = [0u8; BLOCK];
        self.file
            .read_at_managed(pr, u64::from(i) * BLOCK as u64, &mut raw)?;
        let loc = || Location::InternalFile(self.file.header_page);
        let next =
            crate::format::bytes::u32le(&raw, 0).or_corrupt_static(loc, "truncated blob block")?;
        let used =
            crate::format::bytes::u16le(&raw, 4).or_corrupt_static(loc, "truncated blob block")?;
        if usize::from(used) > BLOCK_DATA {
            return Err(self.managed_corrupt("blob used length exceeds its block capacity"));
        }
        let data =
            crate::format::bytes::array(&raw, 6).or_corrupt_static(loc, "truncated blob block")?;
        Ok(BlobBlock { next, used, data })
    }

    pub(crate) fn read_chain_managed(
        &self,
        pr: &dyn PageRead,
        start: u32,
        budget: &memory_budget::MemoryBudget,
        category: memory_budget::Category,
        cap: u64,
    ) -> Result<memory_budget::Charged<Vec<u8>>> {
        self.read_managed(pr, start, None, None, budget, category, cap)
    }

    pub(crate) fn read_value_managed(
        &self,
        pr: &dyn PageRead,
        value: BlobRef,
        budget: &memory_budget::MemoryBudget,
        category: memory_budget::Category,
        cap: u64,
    ) -> Result<memory_budget::Charged<Vec<u8>>> {
        if value.len == 0 {
            return self.read_managed(pr, 0, Some(0), None, budget, category, cap);
        }
        if u64::from(value.len) > u64::from(self.nblocks()) * BLOCK_DATA as u64 {
            return Err(self.managed_corrupt("blob declared length exceeds file capacity"));
        }
        let expected = usize::try_from(value.len)
            .map_err(|_| self.managed_corrupt("blob declared length exceeds address space"))?;
        self.read_managed(pr, value.start, Some(expected), None, budget, category, cap)
    }

    pub(crate) fn read_value_into<'s>(
        &self,
        pr: &dyn PageRead,
        value: BlobRef,
        stop: Option<usize>,
        scratch: &'s mut memory_budget::Scratch<Vec<u8>>,
        category: memory_budget::Category,
        cap: u64,
    ) -> Result<&'s [u8]> {
        use memory_budget::{BudgetError, BudgetedVec, LimitReason, MemoryLimit};
        if value.len != 0 && u64::from(value.len) > u64::from(self.nblocks()) * BLOCK_DATA as u64 {
            return Err(self.managed_corrupt("blob declared length exceeds file capacity"));
        }
        let declared = usize::try_from(value.len)
            .map_err(|_| self.managed_corrupt("blob declared length exceeds address space"))?;
        let (expected, cut) = match stop {
            Some(stop) if declared > stop => (stop, true),
            _ => (declared, false),
        };
        let descriptor = std::mem::size_of::<BudgetedVec<u8>>() as u64;
        let charge = (expected as u64)
            .checked_add(descriptor)
            .ok_or(BudgetError::Overflow)?;
        if charge > cap {
            return Err(Error::Memory(BudgetError::Limit(MemoryLimit {
                category,
                requested: charge,
                reason: LimitReason::Category,
            })));
        }
        let start = if value.len == 0 { 0 } else { value.start };
        let mismatch = || self.managed_corrupt("blob length mismatch");
        let out = scratch.refill(charge, |grant| -> Result<Vec<u8>> {
            let mut out = grant.try_vec_unowned::<u8>(expected)?;
            let mut current = start;
            let mut steps = 0u64;
            while current != 0 {
                if current >= self.nblocks() {
                    return Err(self.managed_corrupt("blob block is out of range"));
                }
                steps += 1;
                if steps > u64::from(self.nblocks()) {
                    return Err(self.managed_corrupt("blob chain has a cycle"));
                }
                let block = self.block_managed(pr, current)?;
                let bytes = if cut {
                    block
                        .bytes()
                        .get(..expected - out.len())
                        .unwrap_or(block.bytes())
                } else {
                    block.bytes()
                };
                let next_len = out
                    .len()
                    .checked_add(bytes.len())
                    .ok_or(BudgetError::Overflow)?;
                if next_len > expected {
                    return Err(mismatch());
                }
                memory_budget::extend_within(&mut out, bytes).map_err(|_| mismatch())?;
                if cut && out.len() == expected {
                    return Ok(out);
                }
                current = block.next;
            }
            if out.len() != expected {
                return Err(mismatch());
            }
            Ok(out)
        })?;
        Ok(out.as_slice())
    }

    pub(crate) fn read_value_prefix_managed(
        &self,
        pr: &dyn PageRead,
        value: BlobRef,
        max_bytes: usize,
        budget: &memory_budget::MemoryBudget,
        category: memory_budget::Category,
        cap: u64,
    ) -> Result<memory_budget::Charged<Vec<u8>>> {
        if u64::from(value.len) <= max_bytes as u64 {
            return self.read_value_managed(pr, value, budget, category, cap);
        }
        if u64::from(value.len) > u64::from(self.nblocks()) * BLOCK_DATA as u64 {
            return Err(self.managed_corrupt("blob declared length exceeds file capacity"));
        }
        self.read_managed(
            pr,
            value.start,
            None,
            Some(max_bytes),
            budget,
            category,
            cap,
        )
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the chain, its bounds and its charge"
    )]
    fn read_managed(
        &self,
        pr: &dyn PageRead,
        start: u32,
        expected: Option<usize>,
        stop: Option<usize>,
        budget: &memory_budget::MemoryBudget,
        category: memory_budget::Category,
        cap: u64,
    ) -> Result<memory_budget::Charged<Vec<u8>>> {
        use memory_budget::{BudgetError, BudgetedVec, LimitReason, MemoryLimit};
        let refusal = |requested| {
            Error::Memory(BudgetError::Limit(MemoryLimit {
                category,
                requested,
                reason: LimitReason::Category,
            }))
        };
        let descriptor = std::mem::size_of::<BudgetedVec<u8>>() as u64;
        let reserve = expected.or(stop).unwrap_or(0);
        let initial = (reserve as u64)
            .checked_add(descriptor)
            .ok_or(BudgetError::Overflow)?;
        if initial > cap {
            return Err(refusal(initial));
        }
        let max_payload = usize::try_from(cap - descriptor).unwrap_or(usize::MAX);
        let mut out = BudgetedVec::try_with_capacity(budget, category, reserve)?;
        let mut current = start;
        let mut steps = 0u64;
        while current != 0 {
            if current >= self.nblocks() {
                return Err(self.managed_corrupt("blob block is out of range"));
            }
            steps += 1;
            if steps > u64::from(self.nblocks()) {
                return Err(self.managed_corrupt("blob chain has a cycle"));
            }
            let block = self.block_managed(pr, current)?;
            let take = match stop {
                Some(stop) => block
                    .bytes()
                    .get(..stop.saturating_sub(out.len()))
                    .unwrap_or(block.bytes()),
                None => block.bytes(),
            };
            let next_len = out
                .len()
                .checked_add(take.len())
                .ok_or(BudgetError::Overflow)?;
            let retained = (next_len as u64)
                .checked_add(descriptor)
                .ok_or(BudgetError::Overflow)?;
            if retained > cap {
                return Err(refusal(retained));
            }
            if expected.is_some_and(|expected| next_len > expected) {
                return Err(self.managed_corrupt("blob length mismatch"));
            }
            if next_len > out.capacity() {
                let capacity = out
                    .capacity()
                    .checked_mul(2)
                    .unwrap_or(max_payload)
                    .max(next_len)
                    .min(max_payload);
                let mut replacement = BudgetedVec::try_with_capacity(budget, category, capacity)?;
                replacement.try_extend_from_slice(out.as_slice())?;
                out = replacement;
            }
            out.try_extend_from_slice(take)?;
            if stop.is_some_and(|stop| out.len() >= stop) {
                return Ok(out.into_charged_vec());
            }
            current = block.next;
        }
        if stop.is_some() || expected.is_some_and(|expected| expected != out.len()) {
            return Err(self.managed_corrupt("blob length mismatch"));
        }
        Ok(out.into_charged_vec())
    }

    fn expected_len(&self, r: BlobRef) -> Result<usize> {
        let want = usize::try_from(r.len).map_err(|_| {
            self.corrupt(
                r.start,
                format!("value length {} does not fit in memory", r.len),
            )
        })?;
        if u64::from(r.len) > u64::from(self.nblocks()) * BLOCK_DATA as u64 {
            return Err(self.corrupt(
                r.start,
                format!(
                    "value length {} exceeds the capacity of the blob file",
                    r.len
                ),
            ));
        }
        Ok(want)
    }

    fn walk_value(
        &self,
        pr: &dyn PageRead,
        r: BlobRef,
        mut visit: impl FnMut(u32, &BlobBlock),
    ) -> Result<()> {
        let want = self.expected_len(r)?;
        let mut got = 0usize;
        self.follow(r.start, |b| {
            let blk = self.block(pr, b)?;
            got = got.saturating_add(usize::from(blk.used));
            if got > want {
                return Err(self.corrupt(
                    b,
                    format!(
                        "length mismatch: the chain from block {} holds more than the {want} bytes expected",
                        r.start
                    ),
                ));
            }
            visit(b, &blk);
            Ok(blk.next)
        })?;
        if got != want {
            return Err(self.corrupt(
                r.start,
                format!(
                    "length mismatch: the chain from block {} holds {got} bytes, {want} expected",
                    r.start
                ),
            ));
        }
        Ok(())
    }

    pub fn read_value(&self, pr: &dyn PageRead, r: BlobRef) -> Result<Vec<u8>> {
        if r.len == 0 {
            return Ok(Vec::new());
        }
        let mut out = Vec::with_capacity(self.expected_len(r)?);
        self.walk_value(pr, r, |_, blk| out.extend_from_slice(blk.bytes()))?;
        Ok(out)
    }

    pub fn read_value_prefix(
        &self,
        pr: &dyn PageRead,
        r: BlobRef,
        max_bytes: usize,
    ) -> Result<Vec<u8>> {
        if u64::from(r.len) <= max_bytes as u64 {
            return self.read_value(pr, r);
        }
        self.expected_len(r)?;
        let mut out = Vec::with_capacity(max_bytes.saturating_add(BLOCK_DATA));
        self.follow(r.start, |b| {
            let blk = self.block(pr, b)?;
            out.extend_from_slice(blk.bytes());
            if out.len() >= max_bytes {
                return Ok(0);
            }
            Ok(blk.next)
        })?;
        if out.len() < max_bytes {
            return Err(self.corrupt(
                r.start,
                format!(
                    "length mismatch: the chain from block {} holds {} bytes, {} expected",
                    r.start,
                    out.len(),
                    r.len
                ),
            ));
        }
        out.truncate(max_bytes);
        Ok(out)
    }

    pub fn value_blocks(&self, pr: &dyn PageRead, r: BlobRef) -> Result<Vec<(u32, u16)>> {
        if r.len == 0 {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        self.walk_value(pr, r, |b, blk| out.push((b, blk.used)))?;
        Ok(out)
    }

    pub fn free_head(&self, pr: &dyn PageRead) -> Result<u32> {
        if self.nblocks() == 0 {
            return Ok(0);
        }
        self.next_of(pr, 0)
    }

    pub fn free_blocks(&self, pr: &dyn PageRead) -> Result<Vec<u32>> {
        let head = self.free_head(pr)?;
        self.chain(pr, head)
    }
}

const VALUE_LIMIT: &str = "blob value length limit";

impl BlobStore {
    pub fn write_new_chain(&mut self, tx: &mut TxnPages, data: &[u8]) -> Result<BlobRef> {
        self.place(tx, Vec::new(), data)
    }

    pub fn rewrite_chain(
        &mut self,
        tx: &mut TxnPages,
        old: BlobRef,
        data: &[u8],
    ) -> Result<BlobRef> {
        tx.check_alive()?;
        let own = if old.len == 0 {
            Vec::new()
        } else {
            tx.run(|tx| self.value_blocks(&*tx, old))?
                .into_iter()
                .map(|(b, _)| b)
                .collect()
        };
        self.place(tx, own, data)
    }

    pub fn free_chain(&mut self, tx: &mut TxnPages, start: u32) -> Result<()> {
        tx.check_alive()?;
        if start == 0 {
            return Ok(());
        }
        tx.run(|tx| {
            let chain = self.chain(&*tx, start)?;
            let head = self.free_head(&*tx)?;
            self.check_not_free(&*tx, head, &chain)?;
            let Some(&last) = chain.last() else {
                return Ok(());
            };
            self.set_next(tx, last, head)?;
            self.set_next(tx, 0, start)?;
            self.note_free(head, &[], &chain, start);
            Ok(())
        })
    }

    pub fn push(&mut self, tx: &mut TxnPages, w: &mut ChainWriter, data: &[u8]) -> Result<()> {
        tx.check_alive()?;
        if data.is_empty() {
            return Ok(());
        }
        let len = (w.len.checked_add(data.len() as u64))
            .filter(|&l| l <= u64::from(u32::MAX))
            .ok_or_else(|| self.file.does_not_fit(VALUE_LIMIT))?;
        let blocks = |n: u64| n.div_ceil(BLOCK_DATA as u64);
        let nblocks = u64::from(self.nblocks());
        let worst = (nblocks + u64::from(nblocks == 0))
            .saturating_add(blocks(len).saturating_sub(blocks(w.len)))
            .saturating_mul(BLOCK as u64);
        self.file.check_len(tx.format_info(), worst)?;
        tx.run(|tx| {
            let mut fresh = 0..0;
            let mut rest = data;
            while !rest.is_empty() {
                if w.cur == 0 || w.buf.len() >= BLOCK_DATA {
                    let b = self.take_block(tx, &mut fresh, &mut w.taken, rest.len())?;
                    if w.cur == 0 {
                        w.start = b;
                    } else {
                        self.put_block(tx, w.cur, b, &w.buf)?;
                        w.buf.clear();
                    }
                    w.cur = b;
                }
                let n = rest.len().min(BLOCK_DATA.saturating_sub(w.buf.len()));
                let (head, tail) = rest
                    .split_at_checked(n)
                    .filter(|_| n > 0)
                    .or_corrupt(|| self.location(w.cur), "block buffer is full")?;
                w.buf.extend_from_slice(head);
                rest = tail;
            }
            if !fresh.is_empty() {
                return Err(self.corrupt(fresh.start, "new blocks are left unused".into()));
            }
            w.len = len;
            Ok(())
        })
    }

    pub fn finish_chain(&mut self, tx: &mut TxnPages, w: ChainWriter) -> Result<BlobRef> {
        tx.run(|tx| {
            if w.cur != 0 {
                self.put_block(tx, w.cur, 0, &w.buf)?;
            }
            let len = u32::try_from(w.len)
                .ok()
                .or_corrupt(|| self.location(w.start), "value length overflows")?;
            Ok(BlobRef {
                start: w.start,
                len,
            })
        })
    }

    pub fn abandon_chain(&mut self, tx: &mut TxnPages, w: ChainWriter) -> Result<()> {
        if w.cur == 0 {
            return tx.check_alive();
        }
        tx.run(|tx| self.set_next(tx, w.cur, 0))?;
        self.free_chain(tx, w.start)
    }

    fn take_block(
        &mut self,
        tx: &mut TxnPages,
        fresh: &mut Range<u32>,
        taken: &mut HashSet<u32>,
        rest: usize,
    ) -> Result<u32> {
        if let Some(b) = fresh.next() {
            return Ok(b);
        }
        let head = self.free_head(&*tx)?;
        if head != 0 {
            if !taken.insert(head) {
                return Err(self.corrupt(
                    head,
                    format!("chain of free blocks has a cycle at block {head}"),
                ));
            }
            let next = self.next_of(&*tx, head)?;
            self.set_next(tx, 0, next)?;
            self.note_free(head, &[head], &[], next);
            return Ok(head);
        }
        let first = self.nblocks().max(1);
        let end = u64::from(first).saturating_add(rest.div_ceil(BLOCK_DATA) as u64);
        self.file.set_len(tx, end.saturating_mul(BLOCK as u64))?;
        let end = u32::try_from(end)
            .ok()
            .or_corrupt(|| self.location(first), "block number overflows")?;
        *fresh = first..end;
        fresh
            .next()
            .or_corrupt(|| self.location(first), "no block for the bytes")
    }

    fn place(&mut self, tx: &mut TxnPages, own: Vec<u32>, data: &[u8]) -> Result<BlobRef> {
        tx.check_alive()?;
        let len = u32::try_from(data.len()).map_err(|_| self.file.does_not_fit(VALUE_LIMIT))?;
        let n = data.len().div_ceil(BLOCK_DATA);
        let reused = own.len().min(n);
        let wanted = n - reused;
        let (head, taken, rest) = tx.run(|tx| {
            let head = self.free_head(&*tx)?;
            if own.len() > reused {
                self.check_not_free(&*tx, head, &own)?;
            }
            let (taken, rest) = self.take_free(&*tx, head, wanted, &own)?;
            Ok((head, taken, rest))
        })?;
        let nblocks = u64::from(self.nblocks());
        let first_new = nblocks + u64::from(nblocks == 0 && n > 0);
        let end_new = first_new.saturating_add((wanted - taken.len()) as u64);
        let new_len = end_new.saturating_mul(BLOCK as u64);
        self.file.check_len(tx.format_info(), new_len)?;
        tx.run(|tx| {
            if new_len > self.file.length {
                self.file.set_len(tx, new_len)?;
            }
            let (Ok(first_new), Ok(end_new)) = (u32::try_from(first_new), u32::try_from(end_new))
            else {
                return Err(self.corrupt(0, "block number overflows".into()));
            };
            let chain: Vec<u32> = own
                .iter()
                .take(reused)
                .chain(&taken)
                .copied()
                .chain(first_new..end_new)
                .collect();
            for (i, chunk) in data.chunks(BLOCK_DATA).enumerate() {
                let block = *chain
                    .get(i)
                    .or_corrupt(|| self.location(0), "chain is shorter than the value")?;
                let next = chain.get(i.saturating_add(1)).copied().unwrap_or(0);
                self.put_block(tx, block, next, chunk)?;
            }
            let mut new_head = rest;
            let surplus = own.get(reused..).unwrap_or_default();
            if let (Some(&first), Some(&last)) = (surplus.first(), surplus.last()) {
                self.set_next(tx, last, new_head)?;
                new_head = first;
            }
            if new_head != head {
                self.set_next(tx, 0, new_head)?;
            }
            self.note_free(head, &taken, surplus, new_head);
            Ok(BlobRef {
                start: chain.first().copied().unwrap_or(0),
                len,
            })
        })
    }

    fn take_free(
        &self,
        pr: &dyn PageRead,
        head: u32,
        wanted: usize,
        own: &[u32],
    ) -> Result<(Vec<u32>, u32)> {
        let own: HashSet<u32> = own.iter().copied().collect();
        let mut seen = HashSet::new();
        let mut taken = Vec::new();
        let mut cur = head;
        while taken.len() < wanted && cur != 0 {
            self.check_range(cur)?;
            if own.contains(&cur) {
                return Err(self.corrupt(
                    cur,
                    format!("free block {cur} belongs to the value being rewritten"),
                ));
            }
            if !seen.insert(cur) {
                return Err(self.corrupt(
                    cur,
                    format!("chain of free blocks has a cycle at block {cur}"),
                ));
            }
            taken.push(cur);
            cur = self.next_of(pr, cur)?;
        }
        Ok((taken, cur))
    }

    fn check_not_free(&mut self, pr: &dyn PageRead, head: u32, blocks: &[u32]) -> Result<()> {
        if self.free.as_ref().is_none_or(|f| f.head != head) {
            let blocks = self.chain(pr, head)?.into_iter().collect();
            self.free = Some(FreeSet { head, blocks });
        }
        let free = self.free.as_ref().map(|f| &f.blocks);
        if let Some(&b) = blocks.iter().find(|b| free.is_some_and(|f| f.contains(b))) {
            return Err(self.corrupt(b, format!("block {b} is already free")));
        }
        Ok(())
    }

    fn note_free(&mut self, from: u32, taken: &[u32], freed: &[u32], head: u32) {
        if self.free.as_ref().is_some_and(|f| f.head != from) {
            self.free = None;
        }
        if let Some(f) = &mut self.free {
            for b in taken {
                f.blocks.remove(b);
            }
            f.blocks.extend(freed);
            f.head = head;
        }
    }

    fn put_block(&mut self, tx: &mut TxnPages, b: u32, next: u32, chunk: &[u8]) -> Result<()> {
        let used = u16::try_from(chunk.len())
            .ok()
            .filter(|&u| usize::from(u) <= BLOCK_DATA)
            .or_corrupt(|| self.location(b), "block data is longer than 250 bytes")?;
        let mut data = [0u8; BLOCK_DATA];
        data.get_mut(..chunk.len())
            .or_corrupt(|| self.location(b), "block data is longer than 250 bytes")?
            .copy_from_slice(chunk);
        let mut raw = [0u8; BLOCK];
        encode_block(&BlobBlock { next, used, data }, &mut raw);
        self.file.write_at(tx, block_offset(b), &raw)
    }

    fn set_next(&mut self, tx: &mut TxnPages, b: u32, next: u32) -> Result<()> {
        self.file.write_at(tx, block_offset(b), &next.to_le_bytes())
    }
}

fn block_offset(b: u32) -> u64 {
    u64::from(b) * BLOCK as u64
}
