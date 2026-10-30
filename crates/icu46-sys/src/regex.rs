use crate::ffi;
use std::ffi::CStr;
use std::ptr::NonNull;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RegexFlags {
    pub case_insensitive: bool,
    pub multiline: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegexError {
    pub code: i32,
    pub name: &'static str,
}

impl RegexError {
    fn new(code: i32) -> Self {
        let name = unsafe { CStr::from_ptr(ffi::u_errorName_46(code)) };
        Self {
            code,
            name: name.to_str().unwrap_or("U_UNKNOWN_ERROR"),
        }
    }
}

impl std::fmt::Display for RegexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ICU error {} ({})", self.name, self.code)
    }
}
impl std::error::Error for RegexError {}

fn status(code: i32) -> Result<(), RegexError> {
    if code > 0 {
        Err(RegexError::new(code))
    } else {
        Ok(())
    }
}
#[cfg(feature = "managed")]
fn runtime_error(error: crate::managed::runtime::RuntimeError) -> RegexError {
    use crate::managed::runtime::RuntimeError;
    RegexError::new(match error {
        RuntimeError::RestartRequired(_) => ffi::U_MEMORY_ALLOCATION_ERROR,
        RuntimeError::Cancelled => ffi::U_REGEX_STOPPED_BY_CALLER,
        RuntimeError::Native(code) => code,
        _ => ffi::U_INVALID_STATE_ERROR,
    })
}

#[cfg(feature = "managed")]
static NEVER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn length(n: usize) -> Result<i32, RegexError> {
    i32::try_from(n).map_err(|_| RegexError::new(ffi::U_ILLEGAL_ARGUMENT_ERROR))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    pub start: usize,
    pub end: usize,
    pub groups: Vec<Option<(usize, usize)>>,
}

pub struct Regex {
    raw: NonNull<ffi::URegularExpression>,
    text: Vec<u16>,
    limits: Option<(i32, i32)>,
    failed: Option<i32>,
    native: Option<i32>,
    #[cfg(feature = "managed")]
    operation: Option<memory_budget::MemoryBudget>,
    #[cfg(feature = "managed")]
    cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}
unsafe impl Send for Regex {}

impl Regex {
    pub fn new(pattern: &[u16], flags: RegexFlags) -> Result<Self, RegexError> {
        crate::init_status().map_err(|e| RegexError::new(e.code))?;
        Self::open(pattern, flags).map_err(|(error, _)| error)
    }

    #[cfg(feature = "managed")]
    pub fn new_managed(
        pattern: &[u16],
        flags: RegexFlags,
        operation: &memory_budget::MemoryBudget,
    ) -> Result<Self, RegexError> {
        Self::open_managed(pattern, flags, operation, None)
    }

    #[cfg(feature = "managed")]
    pub fn new_managed_cancellable(
        pattern: &[u16],
        flags: RegexFlags,
        operation: &memory_budget::MemoryBudget,
        cancel: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<Self, RegexError> {
        Self::open_managed(pattern, flags, operation, Some(cancel))
    }

    #[cfg(feature = "managed")]
    fn open_managed(
        pattern: &[u16],
        flags: RegexFlags,
        operation: &memory_budget::MemoryBudget,
        cancel: Option<&std::sync::Arc<std::sync::atomic::AtomicBool>>,
    ) -> Result<Self, RegexError> {
        use crate::managed::runtime::{Boundary, enter_cancellable};
        length(pattern.len())?;
        let flag = cancel.map_or(&NEVER, |cancel| &**cancel);
        let opened = enter_cancellable(operation, flag, Boundary::RegexOpen, |call| {
            let result = Self::open(pattern, flags);
            if call.failed() {
                return Err(ffi::U_MEMORY_ALLOCATION_ERROR);
            }
            match result {
                Err((error, true)) => Err(error.code),
                other => Ok(other.map_err(|(error, _)| error)),
            }
        });
        let mut regex = opened.map_err(runtime_error)??;
        regex.operation = Some(operation.clone());
        regex.cancel = cancel.cloned();
        Ok(regex)
    }

    fn call<T>(
        &mut self,
        work: impl FnOnce(&mut Self) -> Result<T, RegexError>,
    ) -> Result<T, RegexError> {
        #[cfg(feature = "managed")]
        if let Some(operation) = self.operation.clone() {
            use crate::managed::runtime::{Boundary, enter_cancellable};
            let cancel = self.cancel.clone();
            let flag = cancel.as_deref().unwrap_or(&NEVER);
            let result = enter_cancellable(&operation, flag, Boundary::RegexCall, |call| {
                self.native = None;
                let result = work(self);
                if call.failed() {
                    return Err(ffi::U_MEMORY_ALLOCATION_ERROR);
                }
                match result {
                    Err(error) if self.native == Some(error.code) => Err(error.code),
                    other => Ok(other),
                }
            });
            return result.map_err(runtime_error)?;
        }
        work(self)
    }

    fn open(pattern: &[u16], flags: RegexFlags) -> Result<Self, (RegexError, bool)> {
        let len = length(pattern.len()).map_err(|error| (error, false))?;
        let flags = (u32::from(flags.case_insensitive) * 2) | (u32::from(flags.multiline) * 8);
        let mut code = 0;
        let raw = unsafe {
            ffi::uregex_open_46(
                pattern.as_ptr(),
                len,
                flags,
                std::ptr::null_mut(),
                &mut code,
            )
        };
        if code > 0 {
            if !raw.is_null() {
                unsafe { ffi::uregex_close_46(raw) };
            }
            return Err((RegexError::new(code), true));
        }
        let mut regex = Self {
            raw: NonNull::new(raw)
                .ok_or_else(|| (RegexError::new(ffi::U_MEMORY_ALLOCATION_ERROR), true))?,
            text: Vec::new(),
            limits: None,
            failed: None,
            native: None,
            #[cfg(feature = "managed")]
            operation: None,
            #[cfg(feature = "managed")]
            cancel: None,
        };
        if let Err(error) = regex.set_text(&[]) {
            let native = regex.native == Some(error.code);
            return Err((error, native));
        }
        Ok(regex)
    }

    pub fn set_limits(&mut self, time_limit: i32, stack_limit: i32) -> Result<(), RegexError> {
        self.call(|regex| regex.set_limits_inner(time_limit, stack_limit))
    }

    fn set_limits_inner(&mut self, time_limit: i32, stack_limit: i32) -> Result<(), RegexError> {
        if time_limit < 0 || stack_limit < 0 {
            return Err(RegexError::new(1));
        }
        let previous = self.limits.replace((time_limit, stack_limit));
        if self.failed.is_some() {
            return Ok(());
        }
        if previous.is_some_and(|(_, stack)| stack == stack_limit) {
            let mut code = 0;
            unsafe { ffi::uregex_setTimeLimit_46(self.raw.as_ptr(), time_limit, &mut code) };
            self.check(code)
        } else {
            self.apply_limits(time_limit, stack_limit)
        }
    }

    fn apply_limits(&mut self, time_limit: i32, stack_limit: i32) -> Result<(), RegexError> {
        let mut code = 0;
        unsafe {
            ffi::uregex_setTimeLimit_46(self.raw.as_ptr(), time_limit, &mut code);
            ffi::uregex_setStackLimit_46(self.raw.as_ptr(), stack_limit, &mut code);
        }
        self.check(code)
    }

    fn check(&mut self, code: i32) -> Result<(), RegexError> {
        if code > 0 {
            self.failed = Some(code);
            self.native = Some(code);
        }
        status(code)
    }

    fn renew(&mut self) -> Result<bool, RegexError> {
        let Some(failed) = self.failed else {
            return Ok(false);
        };
        if failed == ffi::U_MEMORY_ALLOCATION_ERROR {
            return Err(RegexError::new(failed));
        }
        let mut code = 0;
        let raw = unsafe { ffi::uregex_clone_46(self.raw.as_ptr(), &mut code) };
        if code > 0 {
            if !raw.is_null() {
                unsafe { ffi::uregex_close_46(raw) };
            }
            self.failed = Some(code);
            self.native = Some(code);
            return Err(RegexError::new(code));
        }
        let Some(raw) = NonNull::new(raw) else {
            self.failed = Some(ffi::U_MEMORY_ALLOCATION_ERROR);
            self.native = Some(ffi::U_MEMORY_ALLOCATION_ERROR);
            return Err(RegexError::new(ffi::U_MEMORY_ALLOCATION_ERROR));
        };
        unsafe { ffi::uregex_close_46(self.raw.as_ptr()) };
        self.raw = raw;
        self.failed = None;
        Ok(true)
    }

    fn set_text(&mut self, text: &[u16]) -> Result<(), RegexError> {
        let len = length(text.len())?;
        let mut owned = Vec::new();
        owned
            .try_reserve_exact(text.len().saturating_add(1))
            .map_err(|_| RegexError::new(7))?;
        owned.extend_from_slice(text);
        owned.push(0);
        let renewed = self.renew()?;
        let mut code = 0;
        unsafe { ffi::uregex_setText_46(self.raw.as_ptr(), owned.as_ptr(), len, &mut code) };
        self.text = owned;
        self.check(code)?;
        match self.limits {
            Some((time_limit, stack_limit)) if renewed => {
                self.apply_limits(time_limit, stack_limit)
            }
            _ => Ok(()),
        }
    }

    pub fn fork(&mut self) -> Result<Self, RegexError> {
        self.call(|regex| {
            let mut code = 0;
            let raw = unsafe { ffi::uregex_clone_46(regex.raw.as_ptr(), &mut code) };
            if let Err(error) = regex.check(code) {
                if !raw.is_null() {
                    unsafe { ffi::uregex_close_46(raw) };
                }
                return Err(error);
            }
            let mut cloned = Self {
                raw: NonNull::new(raw).ok_or_else(|| RegexError::new(7))?,
                text: Vec::new(),
                limits: regex.limits,
                failed: None,
                native: None,
                #[cfg(feature = "managed")]
                operation: regex.operation.clone(),
                #[cfg(feature = "managed")]
                cancel: regex.cancel.clone(),
            };
            cloned.clear_input_inner()?;
            if let Some((time, stack)) = cloned.limits {
                cloned.apply_limits(time, stack)?;
            }
            Ok(cloned)
        })
    }

    pub fn set_input(&mut self, text: &[u16]) -> Result<(), RegexError> {
        self.call(|regex| regex.set_text(text))
    }

    pub fn input_capacity(&self) -> usize {
        self.text.capacity()
    }

    pub fn clear_input(&mut self) -> Result<(), RegexError> {
        self.call(Self::clear_input_inner)
    }

    fn clear_input_inner(&mut self) -> Result<(), RegexError> {
        static EMPTY: [u16; 1] = [0];
        let renewed = self.renew()?;
        let mut code = 0;
        unsafe { ffi::uregex_setText_46(self.raw.as_ptr(), EMPTY.as_ptr(), 0, &mut code) };
        self.check(code)?;
        self.text = Vec::new();
        if let Some((time, stack)) = self.limits.filter(|_| renewed) {
            self.apply_limits(time, stack)?;
        }
        Ok(())
    }

    pub fn find_from(&mut self, from: usize) -> Result<Option<Match>, RegexError> {
        self.call(|regex| regex.search(from, false))
    }

    pub fn find_next(&mut self) -> Result<Option<Match>, RegexError> {
        self.call(|regex| {
            let mut code = 0;
            let found = unsafe { ffi::uregex_findNext_46(regex.raw.as_ptr(), &mut code) };
            regex.check(code)?;
            regex.capture(found != 0)
        })
    }

    pub fn match_from(&mut self, from: usize) -> Result<Option<Match>, RegexError> {
        self.call(|regex| regex.search(from, true))
    }

    fn search(&mut self, from: usize, anchored: bool) -> Result<Option<Match>, RegexError> {
        if from > self.text.len().saturating_sub(1) {
            return Err(RegexError::new(1));
        }
        let from = length(from)?;
        let mut code = 0;
        let found = unsafe {
            if anchored {
                ffi::uregex_lookingAt_46(self.raw.as_ptr(), from, &mut code)
            } else {
                ffi::uregex_find_46(self.raw.as_ptr(), from, &mut code)
            }
        };
        self.check(code)?;
        self.capture(found != 0)
    }

    pub fn find(&mut self, text: &[u16], from: usize) -> Result<Option<Match>, RegexError> {
        self.call(|regex| regex.find_inner(text, from))
    }

    fn find_inner(&mut self, text: &[u16], from: usize) -> Result<Option<Match>, RegexError> {
        self.set_text(text)?;
        self.search(from, false)
    }

    fn capture(&mut self, found: bool) -> Result<Option<Match>, RegexError> {
        if !found {
            return Ok(None);
        }
        let mut code = 0;
        let count = unsafe { ffi::uregex_groupCount_46(self.raw.as_ptr(), &mut code) };
        self.check(code)?;
        let mut ranges = Vec::new();
        let groups = usize::try_from(count)
            .ok()
            .and_then(|n| n.checked_add(1))
            .ok_or_else(|| RegexError::new(7))?;
        ranges
            .try_reserve_exact(groups)
            .map_err(|_| RegexError::new(7))?;
        for i in 0..=count {
            let (start, end) = unsafe {
                (
                    ffi::uregex_start_46(self.raw.as_ptr(), i, &mut code),
                    ffi::uregex_end_46(self.raw.as_ptr(), i, &mut code),
                )
            };
            self.check(code)?;
            ranges.push(if start < 0 {
                None
            } else {
                Some((start as usize, end as usize))
            });
        }
        let (start, end) = ranges
            .first()
            .copied()
            .flatten()
            .ok_or_else(|| RegexError::new(5))?;
        Ok(Some(Match {
            start,
            end,
            groups: ranges.into_iter().skip(1).collect(),
        }))
    }

    pub fn matches(&mut self, text: &[u16]) -> Result<bool, RegexError> {
        self.call(|regex| regex.matches_inner(text))
    }

    fn matches_inner(&mut self, text: &[u16]) -> Result<bool, RegexError> {
        self.set_text(text)?;
        let mut code = 0;
        let matched = unsafe { ffi::uregex_matches_46(self.raw.as_ptr(), 0, &mut code) };
        self.check(code)?;
        Ok(matched != 0)
    }

    pub fn replace_all(
        &mut self,
        text: &[u16],
        replacement: &[u16],
    ) -> Result<Vec<u16>, RegexError> {
        self.call(|regex| regex.replace_all_inner(text, replacement))
    }

    fn replace_all_inner(
        &mut self,
        text: &[u16],
        replacement: &[u16],
    ) -> Result<Vec<u16>, RegexError> {
        self.set_text(text)?;
        let need = self.replacement_length_inner(replacement)?;
        let cap = need.checked_add(1).ok_or_else(|| RegexError::new(7))?;
        let mut out = Vec::new();
        out.try_reserve_exact(cap).map_err(|_| RegexError::new(7))?;
        out.resize(cap, 0);
        let written = self.replace_into_inner(replacement, &mut out)?;
        out.truncate(written);
        Ok(out)
    }

    pub fn replacement_length(&mut self, replacement: &[u16]) -> Result<usize, RegexError> {
        self.call(|regex| regex.replacement_length_inner(replacement))
    }

    fn replacement_length_inner(&mut self, replacement: &[u16]) -> Result<usize, RegexError> {
        let len = length(replacement.len())?;
        let mut code = 0;
        let need = unsafe {
            ffi::uregex_replaceAll_46(
                self.raw.as_ptr(),
                replacement.as_ptr(),
                len,
                std::ptr::null_mut(),
                0,
                &mut code,
            )
        };
        if code != 15 {
            self.check(code)?;
        }
        usize::try_from(need).map_err(|_| RegexError::new(7))
    }

    pub fn replace_into(
        &mut self,
        replacement: &[u16],
        out: &mut [u16],
    ) -> Result<usize, RegexError> {
        self.call(|regex| regex.replace_into_inner(replacement, out))
    }

    fn replace_into_inner(
        &mut self,
        replacement: &[u16],
        out: &mut [u16],
    ) -> Result<usize, RegexError> {
        let len = length(replacement.len())?;
        let cap = length(out.len())?;
        let mut code = 0;
        let written = unsafe {
            ffi::uregex_replaceAll_46(
                self.raw.as_ptr(),
                replacement.as_ptr(),
                len,
                out.as_mut_ptr(),
                cap,
                &mut code,
            )
        };
        self.check(code)?;
        usize::try_from(written).map_err(|_| RegexError::new(5))
    }
}

impl Drop for Regex {
    fn drop(&mut self) {
        unsafe { ffi::uregex_close_46(self.raw.as_ptr()) };
    }
}
