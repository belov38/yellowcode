use super::{Kind, store};
use std::io;

const SIGNALS: [(libc::c_int, Kind); 3] = [
    (libc::SIGINT, Kind::Interrupt),
    (libc::SIGTERM, Kind::Terminate),
    (libc::SIGHUP, Kind::Hangup),
];

pub(super) struct Saved(Vec<(libc::c_int, libc::sigaction)>);

extern "C" fn handler(signal: libc::c_int) {
    if let Some((_, kind)) = SIGNALS.iter().find(|(number, _)| *number == signal) {
        store(*kind);
    }
}

pub(super) fn install() -> io::Result<Saved> {
    install_for(&SIGNALS)
}

pub(super) fn install_for(signals: &[(libc::c_int, Kind)]) -> io::Result<Saved> {
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = handler as extern "C" fn(libc::c_int) as libc::sighandler_t;
    action.sa_flags = libc::SA_RESTART;
    unsafe { libc::sigemptyset(&mut action.sa_mask) };
    let mut saved: Vec<(libc::c_int, libc::sigaction)> = Vec::new();
    for (signal, _) in signals {
        let mut current: libc::sigaction = unsafe { std::mem::zeroed() };
        if unsafe { libc::sigaction(*signal, std::ptr::null(), &mut current) } != 0 {
            let error = io::Error::last_os_error();
            put_back(&saved);
            return Err(error);
        }
        if current.sa_sigaction == libc::SIG_IGN {
            continue;
        }
        let mut previous: libc::sigaction = unsafe { std::mem::zeroed() };
        let installed = unsafe { libc::sigaction(*signal, &action, &mut previous) };
        if installed != 0 {
            let error = io::Error::last_os_error();
            put_back(&saved);
            return Err(error);
        }
        saved.push((*signal, previous));
    }
    Ok(Saved(saved))
}

pub(super) fn restore(saved: Saved) {
    put_back(&saved.0);
}

fn put_back(saved: &[(libc::c_int, libc::sigaction)]) {
    for (signal, previous) in saved {
        unsafe { libc::sigaction(*signal, previous, std::ptr::null_mut()) };
    }
}

pub(super) fn die_of(signal: libc::c_int) {
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = libc::SIG_DFL;
    unsafe { libc::sigemptyset(&mut action.sa_mask) };
    unsafe { libc::sigaction(signal, &action, std::ptr::null_mut()) };
    let mut set: libc::sigset_t = unsafe { std::mem::zeroed() };
    unsafe {
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, signal);
        libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
        libc::raise(signal);
    }
}
