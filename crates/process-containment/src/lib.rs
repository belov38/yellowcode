use std::{
    fs::File,
    io,
    process::{Child, Command, ExitStatus},
    time::{Duration, Instant},
};

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;
#[cfg(unix)]
use unix as platform;
#[cfg(windows)]
use windows as platform;

#[derive(Debug)]
pub struct SpawnError {
    pub started: bool,
    pub error: io::Error,
}

pub struct Pipe(File);

impl io::Read for Pipe {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        platform::read(&mut self.0, bytes)
    }
}
impl io::Write for Pipe {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        platform::write(&mut self.0, bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub struct Process {
    child: Child,
    owner: platform::Owner,
    pinned: bool,
    deadline: Instant,
}

pub struct Spawned {
    pub process: Process,
    pub stdin: Pipe,
    pub stdout: Pipe,
    pub stderr: Pipe,
}

pub fn spawn(command: &mut Command, deadline: Instant) -> Result<Spawned, SpawnError> {
    platform::spawn(command, deadline)
}

impl Process {
    fn stop_and_reap(&mut self) -> io::Result<ExitStatus> {
        self.exited()?;
        let signal = self.terminate();
        loop {
            if self.exited()? {
                let status = self.reap()?;
                return signal.map(|()| status);
            }
            let remaining = self.deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(io::Error::other(format!(
                    "{}root reap unconfirmed at lifecycle deadline",
                    signal
                        .err()
                        .map(|e| format!("termination failed: {e}; "))
                        .unwrap_or_default()
                )));
            }
            std::thread::sleep(remaining.min(Duration::from_millis(5)));
        }
    }

    fn setup_failure(&mut self, error: io::Error, started: bool) -> SpawnError {
        let error = match self.stop_and_reap() {
            Ok(_) => error,
            Err(closure) => io::Error::other(format!(
                "{error}; closing client after setup failure: {closure}"
            )),
        };
        SpawnError { started, error }
    }

    pub fn exited(&mut self) -> io::Result<bool> {
        platform::exited(self)
    }

    pub fn terminate(&mut self) -> io::Result<()> {
        if !self.pinned {
            return Ok(());
        }
        platform::terminate(self)
    }

    pub fn reap(&mut self) -> io::Result<ExitStatus> {
        if !self.exited()? {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "root exit not observed",
            ));
        }
        let status = self.child.wait()?;
        self.pinned = false;
        Ok(status)
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if self.pinned {
            let _ = self.stop_and_reap();
        }
    }
}
