#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]

use std::ffi::{CStr, CString};
use std::fmt;
use std::ptr::NonNull;
use std::sync::OnceLock;

mod bounded;
mod ffi;
#[cfg(feature = "managed")]
pub mod managed;
#[cfg(feature = "regex")]
pub mod regex;
pub use bounded::{IcuStatus, SortKeyWrite};
#[cfg(feature = "testing")]
#[doc(hidden)]
pub use managed as testing;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strength {
    Primary = 0,
    Secondary = 1,
    Tertiary = 2,
}

#[derive(Debug, Clone)]
pub struct IcuError {
    pub code: i32,
    pub name: String,
}

impl IcuError {
    fn new(code: ffi::UErrorCode) -> IcuError {
        let name = unsafe { ffi::u_errorName_46(code) };
        let name = if name.is_null() {
            format!("UErrorCode {code}")
        } else {
            unsafe { CStr::from_ptr(name) }
                .to_string_lossy()
                .into_owned()
        };
        IcuError { code, name }
    }
}

impl fmt::Display for IcuError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ICU error {} ({})", self.name, self.code)
    }
}

impl std::error::Error for IcuError {}

#[repr(C, align(16))]
struct Aligned<T: ?Sized>(T);

static DATA: &Aligned<[u8]> = &Aligned(*include_bytes!("../data/icudt46l-coll.dat"));

static INIT: OnceLock<Result<(), IcuStatus>> = OnceLock::new();

fn init() -> Result<(), IcuError> {
    init_status().map_err(|error| IcuError::new(error.code))
}

unsafe fn init_step(step: u8, status: &mut ffi::UErrorCode) {
    unsafe {
        match step {
            1 => ffi::udata_setFileAccess_46(ffi::UDATA_NO_FILES, status),
            2 => ffi::u_setDataDirectory_46(c"".as_ptr()),
            3 => ffi::uloc_setDefault_46(c"root".as_ptr(), status),
            _ => ffi::udata_setCommonData_46(DATA.0.as_ptr().cast(), status),
        }
    }
}

#[cfg(feature = "managed")]
pub(crate) fn init_managed(call: &managed::runtime::NativeCall) -> Result<(), i32> {
    let mut status = ffi::U_ZERO_ERROR;
    for step in 1..=4 {
        call.set_boundary(managed::runtime::Boundary::Init(step));
        unsafe { init_step(step, &mut status) };
        if status > ffi::U_ZERO_ERROR {
            return Err(status);
        }
        if call.failed() {
            return Err(ffi::U_MEMORY_ALLOCATION_ERROR);
        }
    }
    Ok(())
}

#[cfg(feature = "managed")]
fn legacy_entry() -> Result<(), IcuStatus> {
    use managed::runtime::RuntimeError;
    managed::runtime::legacy_entry().map_err(|error| IcuStatus {
        code: match error {
            RuntimeError::RestartRequired(_) => ffi::U_MEMORY_ALLOCATION_ERROR,
            _ => ffi::U_INVALID_STATE_ERROR,
        },
    })
}
#[cfg(not(feature = "managed"))]
fn legacy_entry() -> Result<(), IcuStatus> {
    Ok(())
}

fn init_status() -> Result<(), IcuStatus> {
    legacy_entry()?;
    *INIT.get_or_init(|| {
        let mut status = ffi::U_ZERO_ERROR;
        for step in 1..=4 {
            unsafe { init_step(step, &mut status) };
        }
        if status > ffi::U_ZERO_ERROR {
            Err(IcuStatus { code: status })
        } else {
            Ok(())
        }
    })
}

pub const SORT_KEY_SLACK: usize = 4;

pub struct Collator {
    raw: NonNull<ffi::UCollator>,
}

unsafe impl Send for Collator {}

const _: () = {
    const fn assert_send<T: Send>() {}
    assert_send::<Collator>();
};

impl Collator {
    pub fn new(locale: &str, strength: Strength) -> Result<Collator, IcuError> {
        init()?;
        let loc = CString::new(locale).map_err(|_| IcuError::new(ffi::U_ILLEGAL_ARGUMENT_ERROR))?;
        Self::try_new_cstr(&loc, strength).map_err(|error| IcuError::new(error.code))
    }

    pub fn sort_key(&self, text: &[u16]) -> Vec<u8> {
        self.sort_key_with_capacity(text, text.len().saturating_mul(4).saturating_add(32))
    }

    pub fn sort_key_len(&self, text: &[u16]) -> Option<usize> {
        i32::try_from(text.len()).ok()?;
        let mut src = Vec::with_capacity(text.len() + 1);
        src.extend_from_slice(text);
        src.push(0u16);
        let mut out = [0u8; 1 + SORT_KEY_SLACK];
        match self.try_sort_key_into(&src, &mut out).ok()? {
            SortKeyWrite::Complete(n) | SortKeyWrite::Required(n) => Some(n),
        }
    }

    #[allow(clippy::expect_used, reason = "documented panic: ICU lengths are i32")]
    fn sort_key_with_capacity(&self, text: &[u16], cap: usize) -> Vec<u8> {
        let len = i32::try_from(text.len()).expect("text longer than i32::MAX code units");
        let mut src = Vec::with_capacity(text.len() + 1);
        src.extend_from_slice(text);
        src.push(0u16);
        let mut cap = cap.clamp(1, i32::MAX as usize - SORT_KEY_SLACK);
        if legacy_entry().is_err() {
            return Vec::new();
        }
        loop {
            let mut buf = vec![0u8; cap + SORT_KEY_SLACK];
            let need = unsafe {
                ffi::ucol_getSortKey_46(
                    self.raw.as_ptr(),
                    src.as_ptr(),
                    len,
                    buf.as_mut_ptr(),
                    cap as i32,
                )
            };
            let need = usize::try_from(need).unwrap_or(0);
            if need <= cap {
                return buf[..need].to_vec();
            }
            cap = need;
        }
    }
}

impl Drop for Collator {
    fn drop(&mut self) {
        unsafe { ffi::ucol_close_46(self.raw.as_ptr()) };
    }
}

pub fn version() -> [u8; 4] {
    let mut v = [0u8; 4];
    unsafe { ffi::u_getVersion_46(v.as_mut_ptr()) };
    v
}
