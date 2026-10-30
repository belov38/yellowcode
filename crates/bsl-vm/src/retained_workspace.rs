use crate::{Exception, Execution, ObjRef};
use std::{alloc::Layout, collections::HashSet, mem::size_of};

const HASH_GROUP_BYTES: usize = if cfg!(all(
    target_feature = "sse2",
    any(target_arch = "x86", target_arch = "x86_64"),
    not(miri),
)) || cfg!(all(
    target_arch = "loongarch64",
    target_feature = "lsx",
    not(miri)
)) {
    size_of::<u128>()
} else if cfg!(any(
    target_pointer_width = "64",
    target_arch = "aarch64",
    target_arch = "x86_64",
    target_arch = "wasm32",
)) {
    size_of::<u64>()
} else {
    size_of::<u32>()
};

pub fn retained_hash_bytes<T>(capacity: usize) -> Option<u64> {
    if capacity == 0 {
        return Some(0);
    }
    let buckets = capacity.checked_next_power_of_two()?;
    let items = Layout::array::<T>(buckets).ok()?;
    let align = items.align().max(HASH_GROUP_BYTES);
    let controls = items.size().checked_add(align - 1)? & !(align - 1);
    let bytes = controls
        .checked_add(buckets)?
        .checked_add(HASH_GROUP_BYTES)?;
    let layout = Layout::from_size_align(bytes, align).ok()?;
    u64::try_from(layout.size()).ok()
}

#[derive(Default)]
pub struct RetainedWorkspace {
    seen: HashSet<(u8, usize)>,
    pending: Vec<ObjRef>,
}

impl RetainedWorkspace {
    pub(crate) fn prepare(ex: &mut Execution) -> Result<Self, Exception> {
        let envelope = ex
            .options()
            .memory_limit
            .map_or(16 * 1024, |limit| limit / 8)
            .min(16 * 1024 * 1024);
        let per_bucket = size_of::<(u8, usize)>() + 1 + size_of::<ObjRef>() / 4;
        let max_buckets = envelope.saturating_sub(HASH_GROUP_BYTES as u64) / per_bucket as u64;
        if max_buckets < 4 {
            return Ok(Self::default());
        }
        let buckets = 1usize << max_buckets.ilog2();
        let entries = if buckets < 8 {
            buckets - 1
        } else {
            buckets / 8 * 7
        };
        let queued = buckets / 4;
        let bytes = retained_hash_bytes::<(u8, usize)>(entries)
            .and_then(|bytes| bytes.checked_add((queued * size_of::<ObjRef>()) as u64))
            .ok_or_else(|| ex.out_of_memory())?;
        ex.reserve(bytes)?;
        let mut workspace = Self::default();
        workspace
            .seen
            .try_reserve(entries)
            .map_err(|_| ex.out_of_memory())?;
        workspace
            .pending
            .try_reserve_exact(queued)
            .map_err(|_| ex.out_of_memory())?;
        Ok(workspace)
    }

    pub fn bytes(&self) -> u64 {
        retained_hash_bytes::<(u8, usize)>(self.seen.capacity())
            .unwrap_or(u64::MAX)
            .saturating_add((self.pending.capacity() * size_of::<ObjRef>()) as u64)
    }

    pub fn capacity(&self) -> usize {
        self.seen.capacity()
    }

    pub fn pending_capacity(&self) -> usize {
        self.pending.capacity()
    }

    pub fn contains(&self, kind: u8, identity: usize) -> bool {
        self.seen.contains(&(kind, identity))
    }

    pub fn first(
        &mut self,
        ex: &mut Execution,
        kind: u8,
        identity: usize,
    ) -> Result<bool, Exception> {
        ex.charge(1)?;
        if self.contains(kind, identity) {
            return Ok(false);
        }
        if self.seen.len() == self.seen.capacity() {
            return Err(ex.out_of_memory());
        }
        Ok(self.seen.insert((kind, identity)))
    }

    pub fn push(&mut self, ex: &Execution, object: ObjRef) -> Result<(), Exception> {
        if self.pending.len() == self.pending.capacity() {
            return Err(ex.out_of_memory());
        }
        self.pending.push(object);
        Ok(())
    }

    pub fn pop(&mut self) -> Option<ObjRef> {
        self.pending.pop()
    }

    pub(crate) fn clear(&mut self) {
        self.pending.clear();
        self.seen.clear();
    }
}
