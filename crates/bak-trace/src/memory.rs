pub fn peak_resident_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|status| parse_vm_hwm(&status))
    }
    #[cfg(windows)]
    {
        windows::peak_working_set()
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        None
    }
}

#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
pub(crate) fn parse_vm_hwm(status: &str) -> Option<u64> {
    let value = status
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))?;
    let mut fields = value.split_whitespace();
    let kilobytes: u64 = fields.next()?.parse().ok()?;
    (fields.next() == Some("kB") && fields.next().is_none())
        .then(|| kilobytes.checked_mul(1024))
        .flatten()
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod windows {
    use windows_sys::Win32::System::{
        ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
        Threading::GetCurrentProcess,
    };

    pub(super) fn peak_working_set() -> Option<u64> {
        let mut counters = std::mem::MaybeUninit::<PROCESS_MEMORY_COUNTERS>::zeroed();
        let ok = unsafe {
            K32GetProcessMemoryInfo(
                GetCurrentProcess(),
                counters.as_mut_ptr(),
                std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
            )
        };
        if ok == 0 {
            return None;
        }
        let counters = unsafe { counters.assume_init() };
        u64::try_from(counters.PeakWorkingSetSize).ok()
    }
}
