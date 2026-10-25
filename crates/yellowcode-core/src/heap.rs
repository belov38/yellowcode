#[allow(
    unsafe_code,
    reason = "the only FFI calls of the crate: the allocator's own release of free memory"
)]
pub fn trim() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    unsafe {
        libc::malloc_trim(0);
    }
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
        }
        unsafe {
            malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
        }
    }
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetProcessHeap() -> *mut std::ffi::c_void;
            fn HeapCompact(heap: *mut std::ffi::c_void, flags: u32) -> usize;
        }
        unsafe {
            HeapCompact(GetProcessHeap(), 0);
        }
    }
}
