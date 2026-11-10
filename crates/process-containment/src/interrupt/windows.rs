use super::{Kind, store};
use std::{
    io,
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{FALSE, TRUE},
    System::Console::{
        CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
        SetConsoleCtrlHandler,
    },
};

const HOLD: Duration = Duration::from_secs(60);

static RELEASED: AtomicBool = AtomicBool::new(false);

pub(super) struct Saved;

fn kind_of(event: u32) -> Option<Kind> {
    match event {
        CTRL_C_EVENT => Some(Kind::Interrupt),
        CTRL_BREAK_EVENT => Some(Kind::Break),
        CTRL_CLOSE_EVENT => Some(Kind::Close),
        CTRL_LOGOFF_EVENT => Some(Kind::Logoff),
        CTRL_SHUTDOWN_EVENT => Some(Kind::Shutdown),
        _ => None,
    }
}

unsafe extern "system" fn handler(event: u32) -> i32 {
    let Some(kind) = kind_of(event) else {
        return FALSE;
    };
    store(kind);
    if matches!(kind, Kind::Close | Kind::Logoff | Kind::Shutdown) {
        let end = Instant::now() + HOLD;
        while !RELEASED.load(Ordering::SeqCst) && Instant::now() < end {
            thread::sleep(Duration::from_millis(10));
        }
    }
    TRUE
}

pub(super) fn install() -> io::Result<Saved> {
    RELEASED.store(false, Ordering::SeqCst);
    if unsafe { SetConsoleCtrlHandler(Some(handler), TRUE) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Saved)
}

pub(super) fn restore(_saved: Saved) {
    RELEASED.store(true, Ordering::SeqCst);
    unsafe { SetConsoleCtrlHandler(Some(handler), FALSE) };
}
