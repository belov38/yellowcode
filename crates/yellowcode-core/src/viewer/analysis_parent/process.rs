use super::AnalysisError;
use std::process::{Child, Command, ExitStatus};

pub(super) struct OwnedChild {
    child: Child,
    group: Group,
    reaped: bool,
    closed: bool,
    observed_exited: bool,
}
impl OwnedChild {
    pub(super) fn spawn(command: &mut Command) -> Result<Self, AnalysisError> {
        Group::prepare(command);
        let mut child = command.spawn().map_err(|_| AnalysisError::Io)?;
        match Group::attach(&mut child) {
            Ok(group) => Ok(Self {
                child,
                group,
                reaped: false,
                closed: false,
                observed_exited: false,
            }),
            Err(error) => {
                let mut child = child;
                kill_reap(&mut child);
                Err(error)
            }
        }
    }
    pub(super) fn exited(&mut self) -> Result<bool, AnalysisError> {
        let exited = self.group.exited(&mut self.child)?;
        self.observed_exited |= exited;
        Ok(exited)
    }
    pub(super) fn finish(&mut self) -> Result<ExitStatus, AnalysisError> {
        self.group.terminate(self.observed_exited)?;
        self.closed = true;
        let status = self.child.wait().map_err(|_| AnalysisError::Io)?;
        self.reaped = true;
        Ok(status)
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !self.closed {
            let _cleanup = self.group.terminate(self.observed_exited);
            self.closed = true;
        }
        if !self.reaped {
            kill_reap(&mut self.child);
            self.reaped = true;
        }
    }
}
fn kill_reap(child: &mut Child) {
    let _kill = child.kill();
    if child.wait().is_err() {
        std::process::abort();
    }
}

#[cfg(unix)]
struct Group(rustix::process::Pid);
#[cfg(unix)]
impl Group {
    fn prepare(command: &mut Command) {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    fn attach(child: &mut Child) -> Result<Self, AnalysisError> {
        rustix::process::Pid::from_raw(child.id() as i32)
            .map(Self)
            .ok_or(AnalysisError::Io)
    }
    fn exited(&self, _child: &mut Child) -> Result<bool, AnalysisError> {
        use rustix::process::{WaitId, WaitIdOptions, waitid};
        waitid(
            WaitId::Pid(self.0),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        )
        .map(|status| status.is_some())
        .map_err(|_| AnalysisError::Io)
    }
    fn terminate(&self, _exited: bool) -> Result<(), AnalysisError> {
        match rustix::process::kill_process_group(self.0, rustix::process::Signal::KILL) {
            Ok(()) | Err(rustix::io::Errno::SRCH) => Ok(()),
            #[cfg(target_os = "macos")]
            Err(rustix::io::Errno::PERM) if _exited => Ok(()),
            Err(_) => Err(AnalysisError::Io),
        }
    }
}

#[cfg(windows)]
struct Group(Option<std::os::windows::io::OwnedHandle>);
#[cfg(windows)]
#[allow(
    unsafe_code,
    reason = "small OS Job Object lifetime boundary; child handle is owned by std"
)]
impl Group {
    fn prepare(command: &mut Command) {
        use std::os::windows::process::CommandExt;
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    }
    fn attach(child: &mut Child) -> Result<Self, AnalysisError> {
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        use windows_sys::Win32::System::JobObjects::*;
        let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if raw.is_null() {
            return Err(AnalysisError::Io);
        }
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = unsafe {
            SetInformationJobObject(
                handle.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&limits).cast(),
                std::mem::size_of_val(&limits) as u32,
            ) != 0
        };
        if !configured {
            return Err(AnalysisError::Io);
        }
        if unsafe { AssignProcessToJobObject(handle.as_raw_handle(), child.as_raw_handle()) } == 0 {
            return Self::assignment_failure(child);
        }
        Ok(Self(Some(handle)))
    }
    fn assignment_failure(child: &mut Child) -> Result<Self, AnalysisError> {
        if child.try_wait().map_err(|_| AnalysisError::Io)?.is_some() {
            Ok(Self(None))
        } else {
            Err(AnalysisError::Io)
        }
    }
    fn exited(&self, child: &mut Child) -> Result<bool, AnalysisError> {
        child
            .try_wait()
            .map(|status| status.is_some())
            .map_err(|_| AnalysisError::Io)
    }
    fn terminate(&self, _exited: bool) -> Result<(), AnalysisError> {
        use std::os::windows::io::AsRawHandle;
        let Some(handle) = &self.0 else {
            return Ok(());
        };
        if unsafe {
            windows_sys::Win32::System::JobObjects::TerminateJobObject(handle.as_raw_handle(), 1)
        } == 0
        {
            Err(AnalysisError::Io)
        } else {
            Ok(())
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use memory_budget::{Category, Limits, MemoryBudget};
    use std::{
        io::Write,
        time::{Duration, Instant},
    };
    const CHILD: &str = "viewer::analysis_parent::process::tests::windows_attachment_probe_child";
    const OUTPUT: &str = "YC_ANALYSIS_ATTACH_PROBE_PATH";

    #[test]
    fn windows_attachment_probe_child() {
        let Some(path) = std::env::var_os(OUTPUT) else {
            return;
        };
        let mut file = std::fs::File::create(path).unwrap();
        file.write_all(&11u64.to_le_bytes()).unwrap();
        file.write_all(b"{\"ok\":true}").unwrap();
        if std::env::var_os("YC_ANALYSIS_ATTACH_PROBE_HANG").is_some() {
            loop {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    #[test]
    fn windows_completed_helper_survives_late_job_attachment() {
        let budget = MemoryBudget::new(Limits::new(1 << 30));
        let reservation = budget
            .try_reserve(
                Category::Execution,
                memory_budget::sealed_heap::ANALYSIS_ADMISSION_BYTES,
            )
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("response");
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", CHILD, "--test-threads=1"])
            .env(OUTPUT, &output)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        Group::prepare(&mut command);
        let mut child = command.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                kill_reap(&mut child);
                panic!("attachment probe child timed out");
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(status.success());
        let group =
            Group::attach(&mut child).expect("completed helper is not a live attachment failure");
        group.terminate(true).unwrap();
        assert!(child.wait().unwrap().success());
        let mut output = std::fs::File::open(output).unwrap();
        let body = super::super::read_body(&mut output, &budget, || Ok(())).unwrap();
        assert_eq!(body.as_ref(), b"{\"ok\":true}");
        assert!(budget.snapshot().current >= reservation.bytes());
        drop(reservation);
        assert!(budget.snapshot().current > 0);
        drop(body);
        assert_eq!(budget.snapshot().current, 0);
    }

    #[test]
    fn windows_live_attachment_failure_refuses_and_reaps() {
        let budget = MemoryBudget::new(Limits::new(1 << 30));
        let reservation = budget
            .try_reserve(
                Category::Execution,
                memory_budget::sealed_heap::ANALYSIS_ADMISSION_BYTES,
            )
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("response");
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", CHILD, "--test-threads=1"])
            .env(OUTPUT, &output)
            .env("YC_ANALYSIS_ATTACH_PROBE_HANG", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        Group::prepare(&mut command);
        let mut child = command.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        while !output.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        if !output.exists() {
            kill_reap(&mut child);
            panic!("attachment probe child failed to start");
        }
        let refused = matches!(
            Group::assignment_failure(&mut child),
            Err(AnalysisError::Io)
        );
        kill_reap(&mut child);
        assert!(refused);
        assert!(!child.wait().unwrap().success());
        assert_eq!(budget.snapshot().current, reservation.bytes());
        drop(reservation);
        assert_eq!(budget.snapshot().current, 0);
    }
}
