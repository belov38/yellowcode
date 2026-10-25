use crate::{BudgetError, Reservation};
use std::alloc::{Layout, alloc, dealloc};
use std::fmt;
use std::ops::Deref;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicUsize, Ordering, fence};

struct Node<T> {
    owners: AtomicUsize,
    value: T,
    reservation: Reservation,
}

pub struct Shared<T> {
    pointer: NonNull<Node<T>>,
}
unsafe impl<T: Send + Sync> Send for Shared<T> {}
unsafe impl<T: Send + Sync> Sync for Shared<T> {}

impl<T> Shared<T> {
    pub fn try_new(value: T, reservation: Reservation) -> Result<Self, BudgetError> {
        unsafe { Self::try_new_with(value, reservation, alloc) }
    }
    unsafe fn try_new_with(
        value: T,
        reservation: Reservation,
        allocate: unsafe fn(Layout) -> *mut u8,
    ) -> Result<Self, BudgetError> {
        let node = Node {
            owners: AtomicUsize::new(1),
            value,
            reservation,
        };
        let layout = Layout::new::<Node<T>>();
        let bytes = u64::try_from(layout.size()).map_err(|_| BudgetError::Overflow)?;
        node.reservation.try_grow(bytes)?;
        let raw = unsafe { allocate(layout) }.cast::<Node<T>>();
        let pointer = NonNull::new(raw).ok_or(BudgetError::AllocationFailed)?;
        unsafe { raw.write(node) };
        Ok(Self { pointer })
    }
    fn node(&self) -> &Node<T> {
        unsafe { self.pointer.as_ref() }
    }
    pub fn reservation(&self) -> &Reservation {
        &self.node().reservation
    }
    pub fn strong_count(&self) -> usize {
        self.node().owners.load(Ordering::Acquire)
    }
    pub fn ptr_eq(first: &Self, second: &Self) -> bool {
        first.pointer == second.pointer
    }
    pub fn get_mut(this: &mut Self) -> Option<&mut T> {
        if this.strong_count() != 1 {
            return None;
        }
        Some(unsafe { &mut this.pointer.as_mut().value })
    }
}
impl<T> Clone for Shared<T> {
    fn clone(&self) -> Self {
        let old = self.node().owners.fetch_add(1, Ordering::Relaxed);
        if old > isize::MAX as usize {
            std::process::abort();
        }
        Self {
            pointer: self.pointer,
        }
    }
}
impl<T> Deref for Shared<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.node().value
    }
}
impl<T: fmt::Debug> fmt::Debug for Shared<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.deref().fmt(f)
    }
}

struct FreeNode<T> {
    pointer: NonNull<Node<T>>,
    _reservation: Reservation,
}
impl<T> Drop for FreeNode<T> {
    fn drop(&mut self) {
        unsafe { dealloc(self.pointer.as_ptr().cast(), Layout::new::<Node<T>>()) };
    }
}
impl<T> Drop for Shared<T> {
    fn drop(&mut self) {
        if self.node().owners.fetch_sub(1, Ordering::Release) != 1 {
            return;
        }
        fence(Ordering::Acquire);
        let reservation =
            unsafe { std::ptr::read(std::ptr::addr_of!((*self.pointer.as_ptr()).reservation)) };
        let guard = FreeNode {
            pointer: self.pointer,
            _reservation: reservation,
        };
        unsafe { std::ptr::drop_in_place(std::ptr::addr_of_mut!((*self.pointer.as_ptr()).value)) };
        drop(guard);
    }
}

pub struct ErasedShared {
    pointer: NonNull<()>,
    vtable: &'static ErasedVtable,
}

struct ErasedVtable {
    type_id: fn() -> std::any::TypeId,
    clone: unsafe fn(NonNull<()>),
    drop: unsafe fn(NonNull<()>),
    debug: unsafe fn(NonNull<()>, &mut fmt::Formatter<'_>) -> fmt::Result,
}

struct VtableOf<T>(std::marker::PhantomData<T>);

impl<T: Send + Sync + fmt::Debug + 'static> VtableOf<T> {
    const VTABLE: ErasedVtable = ErasedVtable {
        type_id: std::any::TypeId::of::<T>,
        clone: Self::clone,
        drop: Self::drop,
        debug: Self::debug,
    };
    unsafe fn clone(pointer: NonNull<()>) {
        let shared = std::mem::ManuallyDrop::new(Shared::<T> {
            pointer: pointer.cast(),
        });
        std::mem::forget(Shared::clone(&shared));
    }
    unsafe fn drop(pointer: NonNull<()>) {
        drop(Shared::<T> {
            pointer: pointer.cast(),
        });
    }
    unsafe fn debug(pointer: NonNull<()>, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let shared = std::mem::ManuallyDrop::new(Shared::<T> {
            pointer: pointer.cast(),
        });
        fmt::Debug::fmt(&**shared, f)
    }
}

unsafe impl Send for ErasedShared {}
unsafe impl Sync for ErasedShared {}

impl<T: Send + Sync + fmt::Debug + 'static> Shared<T> {
    pub fn into_erased(self) -> ErasedShared {
        let pointer = self.pointer.cast();
        std::mem::forget(self);
        ErasedShared {
            pointer,
            vtable: &VtableOf::<T>::VTABLE,
        }
    }
}

impl ErasedShared {
    pub fn downcast_ref<T: 'static>(&self) -> Option<&T> {
        if (self.vtable.type_id)() != std::any::TypeId::of::<T>() {
            return None;
        }
        Some(unsafe { &(*self.pointer.cast::<Node<T>>().as_ptr()).value })
    }
    pub fn ptr_eq(first: &Self, second: &Self) -> bool {
        first.pointer == second.pointer
    }
}
impl Clone for ErasedShared {
    fn clone(&self) -> Self {
        unsafe { (self.vtable.clone)(self.pointer) };
        Self {
            pointer: self.pointer,
            vtable: self.vtable,
        }
    }
}
impl Drop for ErasedShared {
    fn drop(&mut self) {
        unsafe { (self.vtable.drop)(self.pointer) }
    }
}
impl fmt::Debug for ErasedShared {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        unsafe { (self.vtable.debug)(self.pointer, f) }
    }
}
