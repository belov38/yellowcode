#[cfg(target_os = "windows")]
use crate::discovery::Visibility;

fn limit(flags: u32, process: usize, job: usize) -> Option<u64> {
    let process = (flags & 0x100 != 0).then_some(process as u64);
    let job = (flags & 0x200 != 0).then_some(job as u64);
    match (process, job) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

#[cfg(target_os = "windows")]
pub(super) fn discover() -> (Option<u64>, Visibility) {
    use std::ffi::c_void;
    #[repr(C)]
    #[derive(Default)]
    struct Basic {
        process_time: i64,
        job_time: i64,
        flags: u32,
        minimum_working_set: usize,
        maximum_working_set: usize,
        active_processes: u32,
        affinity: usize,
        priority: u32,
        scheduling: u32,
    }
    #[repr(C)]
    #[derive(Default)]
    struct Extended {
        basic: Basic,
        io: [u64; 6],
        process_memory: usize,
        job_memory: usize,
        peak_process_memory: usize,
        peak_job_memory: usize,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn IsProcessInJob(process: *mut c_void, job: *mut c_void, result: *mut i32) -> i32;
        fn QueryInformationJobObject(
            job: *mut c_void,
            class: i32,
            information: *mut c_void,
            length: u32,
            returned: *mut u32,
        ) -> i32;
    }
    let mut in_job = 0;
    if unsafe { IsProcessInJob(GetCurrentProcess(), std::ptr::null_mut(), &mut in_job) } == 0 {
        return (None, Visibility::Unknown);
    }
    if in_job == 0 {
        return (None, Visibility::Visible);
    }
    let mut info = Extended::default();
    let mut returned = 0;
    let success = unsafe {
        QueryInformationJobObject(
            std::ptr::null_mut(),
            9,
            (&mut info as *mut Extended).cast(),
            std::mem::size_of::<Extended>() as u32,
            &mut returned,
        )
    };
    if success == 0 || returned as usize != std::mem::size_of::<Extended>() {
        return (None, Visibility::Unknown);
    }
    (
        limit(info.basic.flags, info.process_memory, info.job_memory),
        Visibility::Partial,
    )
}
