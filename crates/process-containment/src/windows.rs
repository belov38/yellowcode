use super::*;
use std::{
    io::Write,
    os::windows::{
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
        process::CommandExt,
    },
    process::Stdio,
};
use windows_sys::Win32::{
    Foundation::{ERROR_BROKEN_PIPE, ERROR_NO_DATA, INVALID_HANDLE_VALUE},
    Storage::FileSystem::{
        CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
        OPEN_EXISTING, PIPE_ACCESS_INBOUND, PIPE_ACCESS_OUTBOUND, ReadFile,
    },
    System::{
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject, TerminateJobObject,
        },
        Pipes::{
            CreateNamedPipeW, PIPE_NOWAIT, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_TYPE_BYTE,
        },
        Threading::{CREATE_SUSPENDED, WaitForSingleObject},
    },
};

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtResumeProcess(process: windows_sys::Win32::Foundation::HANDLE) -> i32;
}

pub(super) struct Owner(OwnedHandle);

fn job() -> io::Result<Owner> {
    let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    let owner = Owner(unsafe { OwnedHandle::from_raw_handle(handle) });
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    if unsafe {
        SetInformationJobObject(
            owner.0.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of_val(&limits) as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(owner)
}

fn pipe(input: bool) -> io::Result<(Pipe, File)> {
    let nonce = getrandom::u64().map_err(io::Error::other)?;
    let name: Vec<u16> = format!(
        r"\\.\pipe\yellowcode-sql-{}-{nonce:016x}",
        std::process::id()
    )
    .encode_utf16()
    .chain(Some(0))
    .collect();
    let access = if input {
        PIPE_ACCESS_OUTBOUND
    } else {
        PIPE_ACCESS_INBOUND
    };
    let handle = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            access | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            65536,
            65536,
            0,
            std::ptr::null(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let parent = unsafe { File::from_raw_handle(handle) };
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            if input {
                FILE_GENERIC_READ
            } else {
                FILE_GENERIC_WRITE
            },
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let child = unsafe { File::from_raw_handle(handle) };
    Ok((Pipe(parent), child))
}

pub(super) fn spawn(command: &mut Command, deadline: Instant) -> Result<Spawned, SpawnError> {
    let owner = job().map_err(|error| SpawnError {
        started: false,
        error,
    })?;
    spawn_owned(command, owner, deadline)
}

fn spawn_owned(
    command: &mut Command,
    owner: Owner,
    deadline: Instant,
) -> Result<Spawned, SpawnError> {
    let prepare = || {
        let (stdin, input) = pipe(true)?;
        let (stdout, output) = pipe(false)?;
        let (stderr, diagnostic) = pipe(false)?;
        Ok::<_, io::Error>((stdin, input, stdout, output, stderr, diagnostic))
    };
    let (stdin, input, stdout, output, stderr, diagnostic) =
        prepare().map_err(|error| SpawnError {
            started: false,
            error,
        })?;
    let child = command
        .creation_flags(CREATE_SUSPENDED)
        .stdin(Stdio::from(input))
        .stdout(Stdio::from(output))
        .stderr(Stdio::from(diagnostic))
        .spawn()
        .map_err(|error| SpawnError {
            started: false,
            error,
        })?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut process = Process {
        child,
        owner,
        pinned: true,
        deadline,
    };
    if unsafe {
        AssignProcessToJobObject(
            process.owner.0.as_raw_handle(),
            process.child.as_raw_handle(),
        )
    } == 0
    {
        let error = io::Error::last_os_error();
        let error = match process.child.kill() {
            Ok(()) => error,
            Err(_) if process.exited().unwrap_or(false) => error,
            Err(kill) => io::Error::other(format!("{error}; stopping suspended root: {kill}")),
        };
        return Err(process.setup_failure(error, false));
    }
    let status = unsafe { NtResumeProcess(process.child.as_raw_handle()) };
    if status < 0 {
        return Err(process.setup_failure(
            io::Error::other(format!("resume sqlcmd NTSTATUS {status:#x}")),
            true,
        ));
    }
    Ok(Spawned {
        process,
        stdin,
        stdout,
        stderr,
    })
}

pub(super) fn exited(process: &mut Process) -> io::Result<bool> {
    match unsafe { WaitForSingleObject(process.child.as_raw_handle(), 0) } {
        0 => Ok(true),
        258 => Ok(false),
        _ => Err(io::Error::last_os_error()),
    }
}
pub(super) fn terminate(process: &mut Process) -> io::Result<()> {
    if unsafe { TerminateJobObject(process.owner.0.as_raw_handle(), 1) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
pub(super) fn read(file: &mut File, bytes: &mut [u8]) -> io::Result<usize> {
    if bytes.is_empty() {
        return Ok(0);
    }
    let mut count = 0;
    let result = unsafe {
        ReadFile(
            file.as_raw_handle(),
            bytes.as_mut_ptr(),
            u32::try_from(bytes.len()).unwrap_or(u32::MAX),
            &mut count,
            std::ptr::null_mut(),
        )
    };
    if result != 0 {
        return Ok(count as usize);
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(code) if code == ERROR_NO_DATA as i32 => Err(io::ErrorKind::WouldBlock.into()),
        Some(code) if code == ERROR_BROKEN_PIPE as i32 => Ok(0),
        _ => Err(error),
    }
}
pub(super) fn write(file: &mut File, bytes: &[u8]) -> io::Result<usize> {
    match file.write(bytes) {
        Ok(0) if !bytes.is_empty() => Err(io::ErrorKind::WouldBlock.into()),
        result => result,
    }
}
