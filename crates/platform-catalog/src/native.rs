use memory_budget::{BudgetError, Category, MemoryBudget, Reservation};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};
#[derive(Debug)]
pub(crate) enum Error {
    Memory(BudgetError),
    Io(std::io::Error),
}
impl From<BudgetError> for Error {
    fn from(e: BudgetError) -> Self {
        Self::Memory(e)
    }
}
impl From<memory_budget::MemoryLimit> for Error {
    fn from(e: memory_budget::MemoryLimit) -> Self {
        Self::Memory(e.into())
    }
}
impl From<std::io::ErrorKind> for Error {
    fn from(e: std::io::ErrorKind) -> Self {
        Self::Io(e.into())
    }
}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
pub(crate) struct PaidPath {
    pub(crate) value: PathBuf,
    pub(crate) _charge: Reservation,
}
impl PaidPath {
    pub(crate) fn join(base: &Path, part: &str, budget: &MemoryBudget) -> Result<Self, Error> {
        #[cfg(windows)]
        {
            use std::path::Component;
            let incoming = Path::new(part);
            if !part.is_empty()
                && matches!(base.components().next(), Some(Component::Prefix(p)) if p.kind().is_verbatim())
                && !incoming.is_absolute()
                && !matches!(incoming.components().next(), Some(Component::Prefix(_)))
            {
                return join_verbatim(base, incoming, budget);
            }
        }
        let bytes = base
            .as_os_str()
            .len()
            .checked_add(part.len())
            .and_then(|n| n.checked_add(2))
            .ok_or(BudgetError::Overflow)?;
        let charge = budget.try_reserve(Category::Metadata, bytes as u64)?;
        let mut value = PathBuf::new();
        value
            .try_reserve_exact(bytes)
            .map_err(|_| BudgetError::AllocationFailed)?;
        value.push(base);
        value.push(part);
        Ok(Self {
            value,
            _charge: charge,
        })
    }
}
#[cfg(windows)]
fn join_verbatim(base: &Path, part: &Path, budget: &MemoryBudget) -> Result<PaidPath, Error> {
    use std::path::Component;
    let slots = base
        .components()
        .count()
        .checked_add(part.components().count())
        .ok_or(BudgetError::Overflow)?;
    let bytes = slots
        .checked_mul(std::mem::size_of::<Component<'_>>())
        .ok_or(BudgetError::Overflow)?;
    let scratch_charge = budget.try_reserve(
        Category::Metadata,
        u64::try_from(bytes).map_err(|_| BudgetError::Overflow)?,
    )?;
    let mut components = Vec::new();
    components
        .try_reserve_exact(slots)
        .map_err(|_| BudgetError::AllocationFailed)?;
    let mut components = crate::owned::Admitted {
        value: components,
        charge: scratch_charge,
    };
    components.value.extend(base.components());
    for component in part.components() {
        match component {
            Component::RootDir => {
                components.value.truncate(1);
                components.value.push(component);
            }
            Component::ParentDir
                if matches!(components.value.last(), Some(Component::Normal(_))) =>
            {
                components.value.pop();
            }
            Component::CurDir | Component::ParentDir => {}
            _ => components.value.push(component),
        }
    }
    let mut bytes = 0usize;
    let mut need_sep = false;
    for component in components.value.iter().copied() {
        bytes = bytes
            .checked_add(usize::from(need_sep && component != Component::RootDir))
            .and_then(|n| n.checked_add(component.as_os_str().len()))
            .ok_or(BudgetError::Overflow)?;
        need_sep = component_needs_separator(component);
    }
    let charge = budget.try_reserve(
        Category::Metadata,
        u64::try_from(bytes).map_err(|_| BudgetError::Overflow)?,
    )?;
    let mut value = OsString::new();
    value
        .try_reserve_exact(bytes)
        .map_err(|_| BudgetError::AllocationFailed)?;
    let mut need_sep = false;
    for component in components.value.iter().copied() {
        if need_sep && component != Component::RootDir {
            value.push(std::path::MAIN_SEPARATOR_STR);
        }
        value.push(component.as_os_str());
        need_sep = component_needs_separator(component);
    }
    Ok(PaidPath {
        value: PathBuf::from(value),
        _charge: charge,
    })
}
#[cfg(windows)]
fn component_needs_separator(component: std::path::Component<'_>) -> bool {
    use std::path::{Component, Prefix};
    match component {
        Component::RootDir => false,
        Component::Prefix(prefix) => {
            !matches!(prefix.kind(), Prefix::Disk(_)) && !prefix.as_os_str().is_empty()
        }
        _ => true,
    }
}
struct Buffer<T> {
    value: Vec<T>,
    _charge: Reservation,
}
impl<T: Copy + Default> Buffer<T> {
    fn new(n: usize, budget: &MemoryBudget) -> Result<Self, Error> {
        let bytes = n
            .checked_mul(std::mem::size_of::<T>())
            .ok_or(BudgetError::Overflow)?;
        let charge = budget.try_reserve(Category::Metadata, bytes as u64)?;
        let mut value = Vec::new();
        value
            .try_reserve_exact(n)
            .map_err(|_| BudgetError::AllocationFailed)?;
        value.resize(n, T::default());
        Ok(Self {
            value,
            _charge: charge,
        })
    }
}
pub(crate) fn metadata(path: &Path, budget: &MemoryBudget) -> Result<std::fs::Metadata, Error> {
    fn call(path: &Path) -> std::io::Result<std::fs::Metadata> {
        std::fs::metadata(path)
    }
    with_filesystem_path(path, budget, call)
}
pub(crate) fn open(path: &Path, budget: &MemoryBudget) -> Result<std::fs::File, Error> {
    fn call(path: &Path) -> std::io::Result<std::fs::File> {
        std::fs::File::open(path)
    }
    with_filesystem_path(path, budget, call)
}
#[cfg(unix)]
fn with_filesystem_path<T>(
    path: &Path,
    budget: &MemoryBudget,
    operation: impl FnOnce(&Path) -> std::io::Result<T>,
) -> Result<T, Error> {
    let len = path.as_os_str().len();
    let bytes = if len >= 384 {
        len.checked_add(1).ok_or(BudgetError::Overflow)?
    } else {
        0
    };
    let _scratch = budget.try_reserve(Category::Metadata, bytes as u64)?;
    operation(path).map_err(Error::Io)
}
#[cfg(windows)]
fn with_filesystem_path<T>(
    path: &Path,
    budget: &MemoryBudget,
    operation: impl FnOnce(&Path) -> std::io::Result<T>,
) -> Result<T, Error> {
    let normalized;
    let path = if windows_needs_normalization(path) {
        normalized = normalize_windows(path, budget)?;
        &normalized.value
    } else {
        path
    };
    let bytes = path
        .as_os_str()
        .len()
        .checked_add(1)
        .and_then(|n| n.checked_mul(std::mem::size_of::<u16>()))
        .ok_or(BudgetError::Overflow)?;
    let _scratch = budget.try_reserve(Category::Metadata, bytes as u64)?;
    operation(path).map_err(Error::Io)
}
#[cfg(not(any(unix, windows)))]
fn with_filesystem_path<T>(
    _path: &Path,
    _budget: &MemoryBudget,
    _operation: impl FnOnce(&Path) -> std::io::Result<T>,
) -> Result<T, Error> {
    Err(std::io::ErrorKind::Unsupported.into())
}
#[cfg(windows)]
fn windows_needs_normalization(path: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt;
    let mut first = [0u16; 4];
    for (slot, unit) in first.iter_mut().zip(path.as_os_str().encode_wide()) {
        *slot = unit;
    }
    let [a, b, c, d] = first;
    let slash = |unit| unit == u16::from(b'\\') || unit == u16::from(b'/');
    if (a == 92 && b == 92 && c == 63 && d == 92)
        || (a == 92 && b == 63 && c == 63 && d == 92)
        || path.as_os_str().is_empty()
    {
        return false;
    }
    let len = path.as_os_str().encode_wide().count();
    !(len < 247
        && ((b == u16::from(b':') && !slash(a) && (c == 0 || slash(c))) || (slash(a) && slash(b))))
}
#[cfg(windows)]
fn normalize_windows(path: &Path, budget: &MemoryBudget) -> Result<PaidPath, Error> {
    use std::os::windows::ffi::OsStrExt;
    if path.as_os_str().encode_wide().any(|unit| unit == 0) {
        return Err(std::io::ErrorKind::InvalidInput.into());
    }
    let n = path
        .as_os_str()
        .encode_wide()
        .count()
        .checked_add(1)
        .ok_or(BudgetError::Overflow)?;
    let mut input = Buffer::<u16>::new(n, budget)?;
    for (slot, unit) in input.value.iter_mut().zip(path.as_os_str().encode_wide()) {
        *slot = unit;
    }
    absolute_windows(budget, |output| {
        let n = u32::try_from(output.len()).map_err(|_| BudgetError::Overflow)?;
        let size = unsafe {
            GetFullPathNameW(
                input.value.as_ptr(),
                n,
                output.as_mut_ptr(),
                std::ptr::null_mut(),
            )
        };
        if size == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(size as usize)
    })
}
#[cfg(windows)]
fn absolute_windows(
    budget: &MemoryBudget,
    mut call: impl FnMut(&mut [u16]) -> Result<usize, Error>,
) -> Result<PaidPath, Error> {
    let mut stack = [0u16; 512];
    let size = call(&mut stack)?;
    if size < stack.len() {
        return verbatim_windows(stack.get(..size).ok_or(BudgetError::Overflow)?, budget);
    }
    let mut required = size.checked_add(1).ok_or(BudgetError::Overflow)?;
    loop {
        u32::try_from(required).map_err(|_| BudgetError::Overflow)?;
        let mut buffer = Buffer::<u16>::new(required, budget)?;
        let size = call(&mut buffer.value)?;
        if size < buffer.value.len() {
            return verbatim_windows(
                buffer.value.get(..size).ok_or(BudgetError::Overflow)?,
                budget,
            );
        }
        required = size.checked_add(1).ok_or(BudgetError::Overflow)?;
    }
}
#[cfg(windows)]
fn verbatim_windows(absolute: &[u16], budget: &MemoryBudget) -> Result<PaidPath, Error> {
    const VERBATIM: &[u16] = &[92, 92, 63, 92];
    const UNC: &[u16] = &[92, 92, 63, 92, 85, 78, 67, 92];
    let (prefix, absolute) = match absolute {
        [_, 58, 92, ..] => (VERBATIM, absolute),
        [92, 92, 46, 92, rest @ ..] => (VERBATIM, rest),
        [92, 92, 63, 92, ..] | [92, 63, 63, 92, ..] => (&[][..], absolute),
        [92, 92, rest @ ..] => (UNC, rest),
        _ => return Err(std::io::ErrorKind::InvalidData.into()),
    };
    let n = prefix
        .len()
        .checked_add(absolute.len())
        .ok_or(BudgetError::Overflow)?;
    let mut buffer = Buffer::<u16>::new(n, budget)?;
    let (head, tail) = buffer.value.split_at_mut(prefix.len());
    head.copy_from_slice(prefix);
    tail.copy_from_slice(absolute);
    from_wide(&buffer.value, budget)
}
#[cfg(unix)]
pub(crate) fn override_root(budget: &MemoryBudget) -> Result<Option<PaidPath>, Error> {
    let Some(value) = std::env::var_os("YELLOWCODE_PLATFORM_RESOURCES") else {
        return Ok(None);
    };
    let bytes = u64::try_from(value.capacity()).map_err(|_| BudgetError::Overflow)?;
    let charge = budget.try_reserve(Category::Metadata, bytes)?;
    Ok(Some(PaidPath {
        value: PathBuf::from(value),
        _charge: charge,
    }))
}
#[cfg(target_os = "linux")]
pub(crate) fn executable(budget: &MemoryBudget) -> Result<PaidPath, Error> {
    use std::os::unix::ffi::OsStringExt;
    unsafe extern "C" {
        fn readlink(
            path: *const std::ffi::c_char,
            buffer: *mut std::ffi::c_char,
            size: usize,
        ) -> isize;
    }
    let mut n = 256usize;
    loop {
        let mut buffer = Buffer::<u8>::new(n, budget)?;
        let size = unsafe {
            readlink(
                c"/proc/self/exe".as_ptr(),
                buffer.value.as_mut_ptr().cast(),
                n,
            )
        };
        if size < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let size = usize::try_from(size).map_err(|_| BudgetError::Overflow)?;
        if size >= n {
            n = n.checked_mul(2).ok_or(BudgetError::Overflow)?;
            continue;
        }
        buffer.value.truncate(size);
        return Ok(PaidPath {
            value: PathBuf::from(OsString::from_vec(buffer.value)),
            _charge: buffer._charge,
        });
    }
}
#[cfg(target_os = "macos")]
pub(crate) fn executable(budget: &MemoryBudget) -> Result<PaidPath, Error> {
    use std::os::unix::ffi::OsStringExt;
    unsafe extern "C" {
        fn _NSGetExecutablePath(buffer: *mut std::ffi::c_char, size: *mut u32) -> i32;
    }
    let mut n = 0u32;
    let _ = unsafe { _NSGetExecutablePath(std::ptr::null_mut(), &mut n) };
    loop {
        if n == 0 {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
        let mut buffer = Buffer::<u8>::new(n as usize, budget)?;
        let mut required = n;
        let status =
            unsafe { _NSGetExecutablePath(buffer.value.as_mut_ptr().cast(), &mut required) };
        if status != 0 {
            n = required.max(n.checked_add(1).ok_or(BudgetError::Overflow)?);
            continue;
        }
        let end = buffer
            .value
            .iter()
            .position(|b| *b == 0)
            .ok_or(std::io::ErrorKind::InvalidData)?;
        buffer.value.truncate(end);
        return Ok(PaidPath {
            value: PathBuf::from(OsString::from_vec(buffer.value)),
            _charge: buffer._charge,
        });
    }
}
#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetEnvironmentVariableW(name: *const u16, buffer: *mut u16, size: u32) -> u32;
    fn GetModuleFileNameW(module: *mut std::ffi::c_void, buffer: *mut u16, size: u32) -> u32;
    fn SetLastError(error: u32);
    fn GetFullPathNameW(
        path: *const u16,
        size: u32,
        buffer: *mut u16,
        file_part: *mut *mut u16,
    ) -> u32;
}
#[cfg(windows)]
fn from_wide(units: &[u16], budget: &MemoryBudget) -> Result<PaidPath, Error> {
    use std::os::windows::ffi::OsStringExt;
    let mut capacity = units.len();
    let mut length = 0usize;
    let mut peak = capacity;
    for scalar in char::decode_utf16(units.iter().copied()) {
        let additional = scalar.map_or(3, |c| c.len_utf8());
        let required = length
            .checked_add(additional)
            .ok_or(BudgetError::Overflow)?;
        if required > capacity {
            let next = capacity
                .checked_mul(2)
                .ok_or(BudgetError::Overflow)?
                .max(required)
                .max(8);
            peak = peak.max(capacity.checked_add(next).ok_or(BudgetError::Overflow)?);
            capacity = next;
        }
        length = required;
    }
    let charge = budget.try_reserve(Category::Metadata, peak as u64)?;
    Ok(PaidPath {
        value: PathBuf::from(OsString::from_wide(units)),
        _charge: charge,
    })
}
#[cfg(windows)]
pub(crate) fn override_root(budget: &MemoryBudget) -> Result<Option<PaidPath>, Error> {
    let mut name = [0u16; 32];
    for (slot, unit) in name
        .iter_mut()
        .zip("YELLOWCODE_PLATFORM_RESOURCES".encode_utf16())
    {
        *slot = unit;
    }
    let mut n = 0u32;
    loop {
        let mut buffer = Buffer::<u16>::new(n as usize, budget)?;
        let size = unsafe {
            SetLastError(0);
            GetEnvironmentVariableW(name.as_ptr(), buffer.value.as_mut_ptr(), n)
        };
        if size == 0 {
            let error = std::io::Error::last_os_error();
            return match error.raw_os_error() {
                Some(203) => Ok(None),
                Some(0) => from_wide(&[], budget).map(Some),
                _ => Err(error.into()),
            };
        }
        if size >= n {
            n = size.checked_add(1).ok_or(BudgetError::Overflow)?;
            continue;
        }
        buffer.value.truncate(size as usize);
        return from_wide(&buffer.value, budget).map(Some);
    }
}
#[cfg(windows)]
pub(crate) fn executable(budget: &MemoryBudget) -> Result<PaidPath, Error> {
    let mut n = 256u32;
    loop {
        let mut buffer = Buffer::<u16>::new(n as usize, budget)?;
        let size =
            unsafe { GetModuleFileNameW(std::ptr::null_mut(), buffer.value.as_mut_ptr(), n) };
        if size == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        if size >= n {
            n = n.checked_mul(2).ok_or(BudgetError::Overflow)?;
            continue;
        }
        buffer.value.truncate(size as usize);
        return from_wide(&buffer.value, budget);
    }
}
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub(crate) fn executable(_budget: &MemoryBudget) -> Result<PaidPath, Error> {
    Err(std::io::ErrorKind::Unsupported.into())
}

#[cfg(all(test, windows))]
#[path = "../tests/support/windows_join_memory.rs"]
mod join_memory_tests;
