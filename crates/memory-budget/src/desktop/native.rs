use super::Facts;

pub fn facts() -> Facts {
    let (physical, available) = host();
    #[cfg(not(target_os = "windows"))]
    let (external, external_visibility) = {
        let discovery = crate::discovery::discover();
        (
            discovery.bounds.iter().map(|bound| bound.bytes).min(),
            discovery.visibility,
        )
    };
    #[cfg(target_os = "windows")]
    let (external, external_visibility) = super::windows_job::discover();
    Facts {
        physical,
        available,
        external,
        external_visibility,
    }
}

#[cfg(target_os = "linux")]
fn host() -> (Option<u64>, Option<u64>) {
    use std::io::Read;
    let mut text = String::new();
    let Ok(file) = std::fs::File::open("/proc/meminfo") else {
        return (None, None);
    };
    if file.take(64 * 1024).read_to_string(&mut text).is_err() {
        return (None, None);
    }
    let field = |name: &str| {
        let mut fields = text
            .lines()
            .find(|line| line.starts_with(name))?
            .split_ascii_whitespace();
        if fields.next()? != name {
            return None;
        }
        let value = fields.next()?.parse::<u64>().ok()?;
        if fields.next()? != "kB" {
            return None;
        }
        value.checked_mul(1024)
    };
    (field("MemTotal:"), field("MemAvailable:"))
}

#[cfg(target_os = "macos")]
fn host() -> (Option<u64>, Option<u64>) {
    let mut value = 0u64;
    let mut len = std::mem::size_of_val(&value);
    let rc = unsafe {
        libc::sysctlbyname(
            c"hw.memsize".as_ptr(),
            (&mut value as *mut u64).cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    let physical = (rc == 0 && len == std::mem::size_of_val(&value) && value > 0).then_some(value);
    (physical, None)
}

#[cfg(target_os = "windows")]
fn host() -> (Option<u64>, Option<u64>) {
    #[repr(C)]
    struct Status {
        length: u32,
        load: u32,
        total_physical: u64,
        available_physical: u64,
        total_page: u64,
        available_page: u64,
        total_virtual: u64,
        available_virtual: u64,
        available_extended: u64,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalMemoryStatusEx(status: *mut Status) -> i32;
    }
    let mut status = Status {
        length: std::mem::size_of::<Status>() as u32,
        load: 0,
        total_physical: 0,
        available_physical: 0,
        total_page: 0,
        available_page: 0,
        total_virtual: 0,
        available_virtual: 0,
        available_extended: 0,
    };
    if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 {
        return (None, None);
    }
    (Some(status.total_physical), Some(status.available_physical))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn host() -> (Option<u64>, Option<u64>) {
    (None, None)
}
