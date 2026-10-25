use std::cell::UnsafeCell;
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};

#[cfg(any(target_os = "linux", target_os = "android", windows))]
use futex::RawLock;
#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    windows,
    target_vendor = "apple"
)))]
use spin::RawLock;
#[cfg(target_vendor = "apple")]
use unfair::RawLock;

pub struct Lock<T> {
    raw: RawLock,
    value: UnsafeCell<T>,
}
unsafe impl<T: Send> Send for Lock<T> {}
unsafe impl<T: Send> Sync for Lock<T> {}
impl<T: std::panic::RefUnwindSafe> std::panic::RefUnwindSafe for Lock<T> {}
impl<T: std::panic::UnwindSafe> std::panic::UnwindSafe for Lock<T> {}
impl<T> Lock<T> {
    pub const fn new(value: T) -> Self {
        Self {
            raw: RawLock::new(),
            value: UnsafeCell::new(value),
        }
    }
    pub fn lock(&self) -> Guard<'_, T> {
        self.raw.lock();
        Guard {
            lock: self,
            not_send: PhantomData,
        }
    }
}
impl<T> std::fmt::Debug for Lock<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lock")
            .field("locked", &self.raw.is_locked())
            .finish()
    }
}
#[must_use = "if unused the Lock will immediately unlock"]
pub struct Guard<'a, T> {
    lock: &'a Lock<T>,
    not_send: PhantomData<*mut ()>,
}
impl<T> Deref for Guard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.lock.value.get() }
    }
}
impl<T> DerefMut for Guard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.lock.value.get() }
    }
}
impl<T> Drop for Guard<'_, T> {
    fn drop(&mut self) {
        unsafe { self.lock.raw.unlock() }
    }
}

#[cfg(any(target_os = "linux", target_os = "android", windows))]
mod futex {
    use std::sync::atomic::{AtomicU32, Ordering};

    const UNLOCKED: u32 = 0;
    const LOCKED: u32 = 1;
    const CONTENDED: u32 = 2;

    pub(super) struct RawLock {
        state: AtomicU32,
    }
    impl RawLock {
        pub(super) const fn new() -> Self {
            Self {
                state: AtomicU32::new(UNLOCKED),
            }
        }
        #[inline]
        pub(super) fn lock(&self) {
            if self
                .state
                .compare_exchange(UNLOCKED, LOCKED, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                self.lock_contended();
            }
        }
        #[cold]
        fn lock_contended(&self) {
            let mut state = self.spin();
            if state == UNLOCKED {
                match self.state.compare_exchange(
                    UNLOCKED,
                    LOCKED,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return,
                    Err(seen) => state = seen,
                }
            }
            loop {
                if state != CONTENDED && self.state.swap(CONTENDED, Ordering::Acquire) == UNLOCKED {
                    return;
                }
                wait(&self.state, CONTENDED);
                state = self.spin();
            }
        }
        fn spin(&self) -> u32 {
            let mut left = 100;
            loop {
                let state = self.state.load(Ordering::Relaxed);
                if state != LOCKED || left == 0 {
                    return state;
                }
                std::hint::spin_loop();
                left -= 1;
            }
        }
        #[inline]
        pub(super) unsafe fn unlock(&self) {
            if self.state.swap(UNLOCKED, Ordering::Release) == CONTENDED {
                wake_one(&self.state);
            }
        }
        pub(super) fn is_locked(&self) -> bool {
            self.state.load(Ordering::Relaxed) != UNLOCKED
        }
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn wait(futex: &AtomicU32, expected: u32) {
        unsafe {
            libc::syscall(
                libc::SYS_futex,
                futex.as_ptr(),
                libc::FUTEX_WAIT | libc::FUTEX_PRIVATE_FLAG,
                expected,
                std::ptr::null::<libc::timespec>(),
                std::ptr::null::<u32>(),
                0u32,
            );
        }
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn wake_one(futex: &AtomicU32) {
        unsafe {
            libc::syscall(
                libc::SYS_futex,
                futex.as_ptr(),
                libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG,
                1i32,
                std::ptr::null::<libc::timespec>(),
                std::ptr::null::<u32>(),
                0u32,
            );
        }
    }

    #[cfg(windows)]
    #[link(name = "synchronization")]
    unsafe extern "system" {
        fn WaitOnAddress(
            address: *const core::ffi::c_void,
            compare: *const core::ffi::c_void,
            size: usize,
            milliseconds: u32,
        ) -> i32;
        fn WakeByAddressSingle(address: *const core::ffi::c_void);
    }
    #[cfg(windows)]
    fn wait(futex: &AtomicU32, expected: u32) {
        const INFINITE: u32 = u32::MAX;
        unsafe {
            WaitOnAddress(
                futex.as_ptr().cast(),
                std::ptr::from_ref(&expected).cast(),
                4,
                INFINITE,
            );
        }
    }
    #[cfg(windows)]
    fn wake_one(futex: &AtomicU32) {
        unsafe { WakeByAddressSingle(futex.as_ptr().cast()) }
    }
}

#[cfg(target_vendor = "apple")]
mod unfair {
    use std::cell::UnsafeCell;
    use std::sync::atomic::{AtomicBool, Ordering};

    pub(super) struct RawLock {
        lock: UnsafeCell<libc::os_unfair_lock>,
        held: AtomicBool,
    }
    unsafe impl Sync for RawLock {}
    impl RawLock {
        pub(super) const fn new() -> Self {
            Self {
                lock: UnsafeCell::new(libc::OS_UNFAIR_LOCK_INIT),
                held: AtomicBool::new(false),
            }
        }
        pub(super) fn lock(&self) {
            let mut acquired = unsafe { libc::os_unfair_lock_trylock(self.lock.get()) };
            for _ in 0..4 {
                if acquired {
                    break;
                }
                for _ in 0..16 {
                    std::hint::spin_loop();
                }
                acquired = unsafe { libc::os_unfair_lock_trylock(self.lock.get()) };
            }
            if !acquired {
                unsafe { libc::os_unfair_lock_lock(self.lock.get()) };
            }
            self.held.store(true, Ordering::Relaxed);
        }
        pub(super) unsafe fn unlock(&self) {
            self.held.store(false, Ordering::Relaxed);
            unsafe { libc::os_unfair_lock_unlock(self.lock.get()) };
        }
        pub(super) fn is_locked(&self) -> bool {
            self.held.load(Ordering::Relaxed)
        }
    }
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    windows,
    target_vendor = "apple"
)))]
mod spin {
    use std::sync::atomic::{AtomicBool, Ordering};

    pub(super) struct RawLock {
        held: AtomicBool,
    }
    impl RawLock {
        pub(super) const fn new() -> Self {
            Self {
                held: AtomicBool::new(false),
            }
        }
        pub(super) fn lock(&self) {
            let mut spins = 0;
            loop {
                if self
                    .held
                    .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
                {
                    return;
                }
                while self.held.load(Ordering::Relaxed) {
                    if spins < 64 {
                        std::hint::spin_loop();
                        spins += 1;
                    } else {
                        std::thread::yield_now();
                    }
                }
            }
        }
        pub(super) unsafe fn unlock(&self) {
            self.held.store(false, Ordering::Release);
        }
        pub(super) fn is_locked(&self) -> bool {
            self.held.load(Ordering::Relaxed)
        }
    }
}
