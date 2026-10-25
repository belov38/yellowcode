use crate::ffi;
use std::ffi::CStr;
use std::ptr::NonNull;

#[derive(Clone, Copy, Debug, Default)]
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
}
unsafe impl Send for Regex {}

impl Regex {
    pub fn new(pattern: &[u16], flags: RegexFlags) -> Result<Self, RegexError> {
        crate::init_status().map_err(|e| RegexError::new(e.code))?;
        let len = length(pattern.len())?;
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
            return Err(RegexError::new(code));
        }
        let mut regex = Self {
            raw: NonNull::new(raw)
                .ok_or_else(|| RegexError::new(ffi::U_MEMORY_ALLOCATION_ERROR))?,
            text: Vec::new(),
        };
        regex.set_text(&[])?;
        Ok(regex)
    }

    pub fn set_limits(&mut self, time_limit: i32, stack_limit: i32) -> Result<(), RegexError> {
        if time_limit < 0 || stack_limit < 0 {
            return Err(RegexError::new(1));
        }
        let mut code = 0;
        unsafe {
            ffi::uregex_setTimeLimit_46(self.raw.as_ptr(), time_limit, &mut code);
            ffi::uregex_setStackLimit_46(self.raw.as_ptr(), stack_limit, &mut code);
        }
        status(code)
    }

    fn set_text(&mut self, text: &[u16]) -> Result<(), RegexError> {
        let len = length(text.len())?;
        let mut owned = Vec::new();
        owned
            .try_reserve_exact(text.len().saturating_add(1))
            .map_err(|_| RegexError::new(7))?;
        owned.extend_from_slice(text);
        owned.push(0);
        let mut code = 0;
        unsafe { ffi::uregex_setText_46(self.raw.as_ptr(), owned.as_ptr(), len, &mut code) };
        self.text = owned;
        status(code)
    }

    pub fn find(&mut self, text: &[u16], from: usize) -> Result<Option<Match>, RegexError> {
        let from_i = length(from)?;
        if from > text.len() {
            return Err(RegexError::new(1));
        }
        self.set_text(text)?;
        let mut code = 0;
        let found = unsafe { ffi::uregex_find_46(self.raw.as_ptr(), from_i, &mut code) };
        status(code)?;
        if found == 0 {
            return Ok(None);
        }
        let count = unsafe { ffi::uregex_groupCount_46(self.raw.as_ptr(), &mut code) };
        status(code)?;
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
            status(code)?;
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
        self.set_text(text)?;
        let mut code = 0;
        let matched = unsafe { ffi::uregex_matches_46(self.raw.as_ptr(), 0, &mut code) };
        status(code)?;
        Ok(matched != 0)
    }

    pub fn replace_all(
        &mut self,
        text: &[u16],
        replacement: &[u16],
    ) -> Result<Vec<u16>, RegexError> {
        let len = length(replacement.len())?;
        self.set_text(text)?;
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
            status(code)?;
        }
        let need = usize::try_from(need).map_err(|_| RegexError::new(7))?;
        let cap = need.checked_add(1).ok_or_else(|| RegexError::new(7))?;
        let cap_i = length(cap)?;
        let mut out = Vec::new();
        out.try_reserve_exact(cap).map_err(|_| RegexError::new(7))?;
        out.resize(cap, 0);
        code = 0;
        let written = unsafe {
            ffi::uregex_replaceAll_46(
                self.raw.as_ptr(),
                replacement.as_ptr(),
                len,
                out.as_mut_ptr(),
                cap_i,
                &mut code,
            )
        };
        status(code)?;
        out.truncate(usize::try_from(written).map_err(|_| RegexError::new(5))?);
        Ok(out)
    }
}

impl Drop for Regex {
    fn drop(&mut self) {
        unsafe { ffi::uregex_close_46(self.raw.as_ptr()) };
    }
}
