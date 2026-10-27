use std::{
    alloc::{GlobalAlloc, Layout, System},
    ptr,
    sync::atomic::{AtomicPtr, AtomicUsize, Ordering},
};

pub const RESOURCE_EXIT_CODE: i32 = 72;
pub const ANALYSIS_HEAP_BYTES: usize = 256 * 1024 * 1024;
pub const ANALYSIS_OWNER_STACK_BYTES: usize = 133 * 1024 * 1024;
pub const ANALYSIS_PARSER_STACK_BYTES: usize = 133 * 1024 * 1024;
pub const ANALYSIS_ADMISSION_BYTES: u64 = ANALYSIS_HEAP_BYTES as u64
    + ANALYSIS_OWNER_STACK_BYTES as u64
    + ANALYSIS_PARSER_STACK_BYTES as u64
    + (64 * 1024 * 1024);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SealError {
    InvalidCapacity,
    AlreadyActive,
    AllocationFailed,
}
pub struct SealedHeap {
    phase: AtomicUsize,
    base: AtomicPtr<u8>,
    capacity: AtomicUsize,
    next: AtomicUsize,
}
impl Default for SealedHeap {
    fn default() -> Self {
        Self::new()
    }
}
impl SealedHeap {
    pub const fn new() -> Self {
        Self {
            phase: AtomicUsize::new(0),
            base: AtomicPtr::new(ptr::null_mut()),
            capacity: AtomicUsize::new(0),
            next: AtomicUsize::new(0),
        }
    }
    pub unsafe fn seal(&self, capacity: usize) -> Result<(), SealError> {
        if capacity == 0 {
            return Err(SealError::InvalidCapacity);
        }
        let layout = Layout::from_size_align(capacity, std::mem::align_of::<usize>())
            .map_err(|_| SealError::InvalidCapacity)?;
        self.phase
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| SealError::AlreadyActive)?;
        let base = unsafe { System.alloc(layout) };
        if base.is_null() {
            self.phase.store(0, Ordering::Release);
            return Err(SealError::AllocationFailed);
        }
        self.capacity.store(capacity, Ordering::Relaxed);
        self.base.store(base, Ordering::Relaxed);
        self.phase.store(2, Ordering::Release);
        Ok(())
    }
    pub fn capacity(&self) -> usize {
        if self.phase.load(Ordering::Acquire) == 2 {
            self.capacity.load(Ordering::Relaxed)
        } else {
            0
        }
    }
    pub fn used(&self) -> usize {
        self.next.load(Ordering::Relaxed)
    }
    pub fn contains(&self, pointer: *const u8) -> bool {
        if self.phase.load(Ordering::Acquire) != 2 {
            return false;
        }
        let base = self.base.load(Ordering::Relaxed) as usize;
        (pointer as usize)
            .checked_sub(base)
            .is_some_and(|offset| offset < self.capacity.load(Ordering::Relaxed))
    }
    fn take(&self, layout: Layout) -> *mut u8 {
        let base = self.base.load(Ordering::Relaxed);
        let capacity = self.capacity.load(Ordering::Relaxed);
        let mut cursor = self.next.load(Ordering::Relaxed);
        loop {
            let Some(address) = (base as usize)
                .checked_add(cursor)
                .and_then(|v| v.checked_add(layout.align() - 1))
            else {
                resource_exit()
            };
            let address = address & !(layout.align() - 1);
            let Some(start) = address.checked_sub(base as usize) else {
                resource_exit()
            };
            let Some(end) = start
                .checked_add(layout.size().max(1))
                .filter(|&end| end <= capacity)
            else {
                resource_exit()
            };
            match self
                .next
                .compare_exchange_weak(cursor, end, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => {
                    return unsafe { base.add(start) };
                }
                Err(observed) => cursor = observed,
            }
        }
    }
}
fn resource_exit() -> ! {
    unsafe { libc::_exit(RESOURCE_EXIT_CODE) }
}
unsafe impl GlobalAlloc for SealedHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if self.phase.load(Ordering::Acquire) == 2 {
            self.take(layout)
        } else {
            unsafe { System.alloc(layout) }
        }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if self.phase.load(Ordering::Acquire) != 2 {
            return unsafe { System.alloc_zeroed(layout) };
        }
        let pointer = self.take(layout);
        unsafe { ptr::write_bytes(pointer, 0, layout.size()) };
        pointer
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        if !self.contains(pointer) {
            unsafe { System.dealloc(pointer, layout) };
        }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if self.phase.load(Ordering::Acquire) != 2 {
            return unsafe { System.realloc(pointer, layout, new_size) };
        }
        let Ok(new_layout) = Layout::from_size_align(new_size, layout.align()) else {
            resource_exit()
        };
        let replacement = self.take(new_layout);
        unsafe {
            ptr::copy_nonoverlapping(pointer, replacement, layout.size().min(new_size));
            self.dealloc(pointer, layout);
        }
        replacement
    }
}
