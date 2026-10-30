use crate::{Collator, SORT_KEY_SLACK, Strength, ffi, init_status, legacy_entry};
use std::ffi::CStr;
use std::ptr::NonNull;
#[cfg(feature = "managed")]
use std::sync::atomic::AtomicBool;

#[cfg(feature = "managed")]
static NEVER: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IcuStatus {
    pub code: i32,
}
impl std::fmt::Display for IcuStatus {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(output, "ICU error {}", self.code)
    }
}
impl std::error::Error for IcuStatus {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortKeyWrite {
    Complete(usize),
    Required(usize),
}

impl Collator {
    pub fn try_new_cstr(locale: &CStr, strength: Strength) -> Result<Self, IcuStatus> {
        init_status()?;
        let mut status = ffi::U_ZERO_ERROR;
        let raw = unsafe { ffi::ucol_open_46(locale.as_ptr(), &mut status) };
        if status > ffi::U_ZERO_ERROR {
            if let Some(raw) = NonNull::new(raw) {
                unsafe { ffi::ucol_close_46(raw.as_ptr()) };
            }
            return Err(IcuStatus { code: status });
        }
        let raw = NonNull::new(raw).ok_or(IcuStatus {
            code: ffi::U_MEMORY_ALLOCATION_ERROR,
        })?;
        unsafe { ffi::ucol_setStrength_46(raw.as_ptr(), strength as i32) };
        Ok(Self { raw })
    }

    pub fn try_sort_key_into(
        &self,
        source: &[u16],
        output: &mut [u8],
    ) -> Result<SortKeyWrite, IcuStatus> {
        legacy_entry()?;
        self.sort_key_raw(source, output)
    }

    pub(crate) fn sort_key_raw(
        &self,
        source: &[u16],
        output: &mut [u8],
    ) -> Result<SortKeyWrite, IcuStatus> {
        let invalid = IcuStatus {
            code: ffi::U_ILLEGAL_ARGUMENT_ERROR,
        };
        if source.last() != Some(&0) {
            return Err(invalid);
        }
        let length = source.len().checked_sub(1).ok_or(invalid)?;
        let length = i32::try_from(length).map_err(|_| invalid)?;
        let capacity = output.len().checked_sub(SORT_KEY_SLACK).ok_or(invalid)?;
        if capacity == 0 {
            return Err(invalid);
        }
        let capacity = i32::try_from(capacity).map_err(|_| invalid)?;
        let needed = unsafe {
            ffi::ucol_getSortKey_46(
                self.raw.as_ptr(),
                source.as_ptr(),
                length,
                output.as_mut_ptr(),
                capacity,
            )
        };
        if needed <= 0 {
            return Err(IcuStatus {
                code: ffi::U_MEMORY_ALLOCATION_ERROR,
            });
        }
        let needed = usize::try_from(needed).map_err(|_| invalid)?;
        if needed > capacity as usize {
            Ok(SortKeyWrite::Required(needed))
        } else {
            Ok(SortKeyWrite::Complete(needed))
        }
    }
}

#[cfg(feature = "managed")]
impl Collator {
    pub fn open_managed(
        locale: &CStr,
        strength: Strength,
        operation: &memory_budget::MemoryBudget,
    ) -> Result<Self, crate::managed::runtime::RuntimeError> {
        Self::open_managed_cancellable(locale, strength, operation, &NEVER)
    }

    pub fn open_managed_cancellable(
        locale: &CStr,
        strength: Strength,
        operation: &memory_budget::MemoryBudget,
        cancel: &AtomicBool,
    ) -> Result<Self, crate::managed::runtime::RuntimeError> {
        use crate::managed::runtime::{Boundary, enter_cancellable};
        enter_cancellable(operation, cancel, Boundary::CollatorOpen, |call| {
            let mut status = ffi::U_ZERO_ERROR;
            let raw = NonNull::new(unsafe { ffi::ucol_open_46(locale.as_ptr(), &mut status) });
            let failed = status > ffi::U_ZERO_ERROR || call.failed();
            let Some(raw) = raw.filter(|_| !failed) else {
                if let Some(raw) = raw {
                    unsafe { ffi::ucol_close_46(raw.as_ptr()) };
                }
                return Err(if status > ffi::U_ZERO_ERROR {
                    status
                } else {
                    ffi::U_MEMORY_ALLOCATION_ERROR
                });
            };
            let collator = Self { raw };
            call.set_boundary(Boundary::Strength);
            unsafe { ffi::ucol_setStrength_46(collator.raw.as_ptr(), strength as i32) };
            if call.failed() {
                return Err(ffi::U_MEMORY_ALLOCATION_ERROR);
            }
            Ok(collator)
        })
    }

    pub fn sort_key_managed(
        &self,
        source: &[u16],
        output: &mut [u8],
        operation: &memory_budget::MemoryBudget,
    ) -> Result<SortKeyWrite, crate::managed::runtime::RuntimeError> {
        self.sort_key_managed_cancellable(source, output, operation, &NEVER)
    }

    pub fn sort_key_len_managed(
        &self,
        source: &[u16],
        operation: &memory_budget::MemoryBudget,
        cancel: &AtomicBool,
    ) -> Result<usize, crate::managed::runtime::RuntimeError> {
        use crate::managed::runtime::{Boundary, enter_cancellable};
        enter_cancellable(operation, cancel, Boundary::SortKeyLength, |call| {
            let mut output = [0u8; 1 + SORT_KEY_SLACK];
            let key = self.sort_key_raw(source, &mut output).map_err(|e| e.code)?;
            if call.failed() {
                return Err(ffi::U_MEMORY_ALLOCATION_ERROR);
            }
            match key {
                SortKeyWrite::Complete(n) | SortKeyWrite::Required(n) => Ok(n),
            }
        })
    }

    pub fn sort_key_managed_cancellable(
        &self,
        source: &[u16],
        output: &mut [u8],
        operation: &memory_budget::MemoryBudget,
        cancel: &AtomicBool,
    ) -> Result<SortKeyWrite, crate::managed::runtime::RuntimeError> {
        use crate::managed::runtime::{Boundary, enter_cancellable};
        enter_cancellable(operation, cancel, Boundary::SortKey, |call| {
            let key = self.sort_key_raw(source, output).map_err(|e| e.code)?;
            if call.failed() {
                return Err(ffi::U_MEMORY_ALLOCATION_ERROR);
            }
            Ok(key)
        })
    }
}
