use super::*;
use std::{
    io::{Read, Write},
    os::{
        fd::{AsRawFd, OwnedFd},
        unix::process::CommandExt,
    },
    process::Stdio,
};

pub(super) struct Owner;

fn nonblocking(file: File) -> io::Result<Pipe> {
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Pipe(file))
}

pub(super) fn spawn(command: &mut Command, deadline: Instant) -> Result<Spawned, SpawnError> {
    spawn_with_setup(command, deadline, prepare_pipes)
}

fn spawn_with_setup(
    command: &mut Command,
    deadline: Instant,
    setup: impl FnOnce(&mut Process) -> io::Result<(Pipe, Pipe, Pipe)>,
) -> Result<Spawned, SpawnError> {
    let child = command
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| SpawnError {
            started: false,
            error,
        })?;
    let mut process = Process {
        child,
        owner: Owner,
        pinned: true,
        deadline,
    };
    let pipes = setup(&mut process);
    let (stdin, stdout, stderr) = pipes.map_err(|error| process.setup_failure(error, true))?;
    Ok(Spawned {
        process,
        stdin,
        stdout,
        stderr,
    })
}

fn prepare_pipes(process: &mut Process) -> io::Result<(Pipe, Pipe, Pipe)> {
    let missing = || io::Error::other("sqlcmd pipe unavailable");
    let stdin = nonblocking(File::from(OwnedFd::from(
        process.child.stdin.take().ok_or_else(missing)?,
    )))?;
    let stdout = nonblocking(File::from(OwnedFd::from(
        process.child.stdout.take().ok_or_else(missing)?,
    )))?;
    let stderr = nonblocking(File::from(OwnedFd::from(
        process.child.stderr.take().ok_or_else(missing)?,
    )))?;
    Ok((stdin, stdout, stderr))
}

pub(super) fn exited(process: &mut Process) -> io::Result<bool> {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            process.child.id(),
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result < 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ECHILD) {
            process.pinned = false;
        }
        return Err(error);
    }
    Ok(unsafe { info.si_pid() } != 0)
}

pub(super) fn terminate(process: &mut Process) -> io::Result<()> {
    let _ = &process.owner;
    let id =
        i32::try_from(process.child.id()).map_err(|_| io::Error::other("invalid owned PGID"))?;
    if unsafe { libc::kill(-id, libc::SIGKILL) } < 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            #[cfg(target_os = "macos")]
            if error.raw_os_error() == Some(libc::EPERM) {
                match root_is_only_group_member(process) {
                    Ok(true) => return Ok(()),
                    Ok(false) => {}
                    Err(lookup) => {
                        return Err(io::Error::other(format!(
                            "owned group signal failed: {error}; membership unconfirmed: {lookup}"
                        )));
                    }
                }
            }
            return Err(error);
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn root_is_only_group_member(process: &mut Process) -> io::Result<bool> {
    if !exited(process)? {
        return Ok(false);
    }
    membership_with(process.child.id(), |members| {
        let bytes = std::mem::size_of_val(members);
        unsafe {
            *libc::__error() = 0;
        }
        let count = unsafe {
            libc::proc_listpids(
                2,
                process.child.id(),
                members.as_mut_ptr().cast(),
                bytes as i32,
            )
        };
        (
            count,
            io::Error::last_os_error().raw_os_error().unwrap_or(0),
        )
    })
}

#[cfg(any(target_os = "macos", test))]
fn membership_with(
    root: u32,
    enumerate: impl FnOnce(&mut [i32]) -> (i32, i32),
) -> io::Result<bool> {
    let mut members = [0_i32; 1024];
    let bytes = std::mem::size_of_val(&members);
    let (count, error) = enumerate(&mut members);
    if count <= 0 || error != 0 {
        return Err(if error != 0 {
            io::Error::from_raw_os_error(error)
        } else {
            io::Error::other("Darwin membership enumeration returned no verifiable records")
        });
    }
    let count = count as usize;
    if count >= bytes || !count.is_multiple_of(std::mem::size_of::<i32>()) {
        return Err(io::Error::other(
            "cannot verify completed Darwin process group membership",
        ));
    }
    let entries = count / std::mem::size_of::<i32>();
    let (reported, unreported) = members.split_at(entries);
    if unreported.iter().any(|&pid| pid != 0)
        || reported.iter().any(|&pid| pid < 0)
        || reported.iter().filter(|&&pid| pid as u32 == root).count() != 1
    {
        return Err(io::Error::other(
            "inconsistent Darwin membership enumeration",
        ));
    }
    Ok(reported.iter().all(|&pid| pid == 0 || pid as u32 == root))
}

pub(super) fn read(file: &mut File, bytes: &mut [u8]) -> io::Result<usize> {
    file.read(bytes)
}
pub(super) fn write(file: &mut File, bytes: &[u8]) -> io::Result<usize> {
    file.write(bytes)
}
