use super::abi::NativeOwner;
use memory_budget::{Category, MemoryBudget, MemoryLimit, Reservation};
use std::alloc::{GlobalAlloc, Layout};
use std::cell::Cell;
use std::ffi::c_void;
use std::mem::size_of;
use std::ptr;

unsafe extern "C" {
    fn managed_memory_alignment_46() -> usize;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureCause {
    MemoryLimit(MemoryLimit),
    LayoutOverflow,
    SystemAllocation,
    InvalidRole,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AllocationFailure {
    pub owner: NativeOwner,
    pub cause: FailureCause,
}

pub struct AllocationScope {
    operation: MemoryBudget,
    process: MemoryBudget,
    first: Cell<Option<AllocationFailure>>,
}

pub(super) struct Header {
    pub(super) layout: Layout,
    pub(super) capacity: usize,
    pub(super) owner: NativeOwner,
    pub(super) reservation: Reservation,
}

pub(super) fn native_alignment() -> usize {
    unsafe { managed_memory_alignment_46() }
}
fn header_offset() -> usize {
    let alignment = native_alignment();
    size_of::<Header>().div_ceil(alignment) * alignment
}
pub(super) unsafe fn header(pointer: *mut c_void) -> *mut Header {
    unsafe { pointer.cast::<u8>().sub(header_offset()).cast() }
}

impl AllocationScope {
    pub fn new(operation: &MemoryBudget) -> Self {
        Self {
            operation: operation.clone(),
            process: operation.root_budget(),
            first: Cell::new(None),
        }
    }
    pub fn failure(&self) -> Option<AllocationFailure> {
        self.first.get()
    }
    fn refuse(&self, owner: NativeOwner, cause: FailureCause) -> *mut c_void {
        if self.first.get().is_none() {
            self.first.set(Some(AllocationFailure { owner, cause }));
        }
        ptr::null_mut()
    }
    pub unsafe fn allocate<A: GlobalAlloc>(
        &self,
        allocator: &A,
        owner: NativeOwner,
        size: usize,
    ) -> *mut c_void {
        let (budget, category) = match owner.role {
            0 => (&self.operation, Category::Execution),
            1..=12 => (&self.process, Category::Metadata),
            _ => return self.refuse(owner, FailureCause::InvalidRole),
        };
        unsafe { self.allocate_for(allocator, budget, category, owner, size) }
    }
    unsafe fn allocate_for<A: GlobalAlloc>(
        &self,
        allocator: &A,
        budget: &MemoryBudget,
        category: Category,
        owner: NativeOwner,
        size: usize,
    ) -> *mut c_void {
        if size == 0 {
            return ptr::null_mut();
        }
        let layout = Layout::from_size_align(size, native_alignment())
            .and_then(|payload| Layout::new::<Header>().extend(payload))
            .map(|(layout, offset)| (layout.pad_to_align(), offset));
        let (layout, offset) = match layout {
            Ok(value) => value,
            Err(_) => return self.refuse(owner, FailureCause::LayoutOverflow),
        };
        let Ok(bytes) = u64::try_from(layout.size()) else {
            return self.refuse(owner, FailureCause::LayoutOverflow);
        };
        let reservation = match budget.try_reserve_no_evict(category, bytes) {
            Ok(reservation) => reservation,
            Err(cause) => return self.refuse(owner, FailureCause::MemoryLimit(cause)),
        };
        let base = unsafe { allocator.alloc(layout) };
        if base.is_null() {
            drop(reservation);
            return self.refuse(owner, FailureCause::SystemAllocation);
        }
        unsafe {
            base.cast::<Header>().write(Header {
                layout,
                capacity: size,
                owner,
                reservation,
            });
            base.add(offset).cast()
        }
    }
    pub unsafe fn reallocate<A: GlobalAlloc>(
        &self,
        allocator: &A,
        owner_if_null: NativeOwner,
        pointer: *mut c_void,
        size: usize,
    ) -> *mut c_void {
        if pointer.is_null() {
            return unsafe { self.allocate(allocator, owner_if_null, size) };
        }
        if size == 0 {
            unsafe { release(allocator, pointer) };
            return ptr::null_mut();
        }
        let old = unsafe { &*header(pointer) };
        let replacement = unsafe {
            self.allocate_for(
                allocator,
                old.reservation.budget(),
                old.reservation.category(),
                old.owner,
                size,
            )
        };
        if replacement.is_null() {
            return ptr::null_mut();
        }
        unsafe {
            ptr::copy_nonoverlapping(
                pointer.cast::<u8>(),
                replacement.cast::<u8>(),
                old.capacity.min(size),
            );
            release(allocator, pointer);
        }
        replacement
    }
}

pub unsafe fn release<A: GlobalAlloc>(allocator: &A, pointer: *mut c_void) {
    if pointer.is_null() {
        return;
    }
    let Header {
        layout,
        reservation,
        ..
    } = unsafe { header(pointer).read() };
    unsafe { allocator.dealloc(pointer.cast::<u8>().sub(header_offset()), layout) };
    drop(reservation);
}
