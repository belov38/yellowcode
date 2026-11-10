use std::{
    cell::Cell,
    io,
    marker::PhantomData,
    sync::{
        Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
    },
    thread,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Interrupt,
    Break,
    Terminate,
    Hangup,
    Close,
    Logoff,
    Shutdown,
}

impl Kind {
    const ALL: [Kind; 7] = [
        Kind::Interrupt,
        Kind::Break,
        Kind::Terminate,
        Kind::Hangup,
        Kind::Close,
        Kind::Logoff,
        Kind::Shutdown,
    ];

    fn code(self) -> u8 {
        match self {
            Kind::Interrupt => 1,
            Kind::Break => 2,
            Kind::Terminate => 3,
            Kind::Hangup => 4,
            Kind::Close => 5,
            Kind::Logoff => 6,
            Kind::Shutdown => 7,
        }
    }

    fn from_code(code: u8) -> Option<Kind> {
        Self::ALL.into_iter().find(|kind| kind.code() == code)
    }

    pub const fn exit_code(self) -> i32 {
        match self {
            Kind::Interrupt | Kind::Break => 130,
            Kind::Terminate | Kind::Logoff | Kind::Shutdown => 143,
            Kind::Hangup | Kind::Close => 129,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Kind::Interrupt if cfg!(unix) => "SIGINT",
            Kind::Interrupt => "CTRL_C_EVENT",
            Kind::Break => "CTRL_BREAK_EVENT",
            Kind::Terminate => "SIGTERM",
            Kind::Hangup => "SIGHUP",
            Kind::Close => "CTRL_CLOSE_EVENT",
            Kind::Logoff => "CTRL_LOGOFF_EVENT",
            Kind::Shutdown => "CTRL_SHUTDOWN_EVENT",
        }
    }
}

static REQUEST: AtomicU8 = AtomicU8::new(0);

static REQUESTS: AtomicUsize = AtomicUsize::new(0);

static NOTICED: AtomicBool = AtomicBool::new(false);

fn forget_requests() {
    REQUEST.store(0, Ordering::SeqCst);
    REQUESTS.store(0, Ordering::SeqCst);
    NOTICED.store(false, Ordering::SeqCst);
}

struct State {
    watchers: usize,
    saved: Option<platform::Saved>,
}

static STATE: Mutex<State> = Mutex::new(State {
    watchers: 0,
    saved: None,
});

fn state() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(PoisonError::into_inner)
}

fn store(kind: Kind) {
    let _ = REQUEST.compare_exchange(0, kind.code(), Ordering::SeqCst, Ordering::SeqCst);
    REQUESTS.fetch_add(1, Ordering::SeqCst);
}

thread_local! {
    static ARMED: Cell<usize> = const { Cell::new(0) };
}

#[must_use = "the handlers are removed when the watch is dropped"]
pub struct Watch(PhantomData<*const ()>);

pub fn watch() -> io::Result<Watch> {
    let mut state = state();
    if state.watchers == 0 {
        forget_requests();
        state.saved = Some(platform::install()?);
    }
    state.watchers += 1;
    ARMED.with(|armed| armed.set(armed.get() + 1));
    Ok(Watch(PhantomData))
}

impl Drop for Watch {
    fn drop(&mut self) {
        ARMED.with(|armed| armed.set(armed.get().saturating_sub(1)));
        let mut state = state();
        state.watchers = state.watchers.saturating_sub(1);
        if state.watchers == 0 {
            if let Some(saved) = state.saved.take() {
                platform::restore(saved);
            }
            forget_requests();
        }
    }
}

pub fn armed() -> bool {
    ARMED.with(|armed| armed.get() > 0)
}

pub fn requested() -> Option<Kind> {
    Kind::from_code(REQUEST.load(Ordering::SeqCst))
}

pub fn wait_for_request(limit: Duration) -> Option<Kind> {
    let end = Instant::now() + limit;
    loop {
        if let Some(kind) = requested() {
            return Some(kind);
        }
        let now = Instant::now();
        if now >= end {
            return None;
        }
        thread::sleep((end - now).min(Duration::from_millis(5)));
    }
}

pub fn count() -> usize {
    REQUESTS.load(Ordering::SeqCst)
}

pub fn claim_notice() -> bool {
    NOTICED
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    Signal(i32),
    Status(i32),
}

impl Kind {
    pub const fn windows_status(self) -> i32 {
        match self {
            Kind::Interrupt | Kind::Break => 0xC000_013A_u32 as i32,
            other => other.exit_code(),
        }
    }

    pub const fn end(self) -> End {
        #[cfg(unix)]
        {
            match self {
                Kind::Interrupt => End::Signal(libc::SIGINT),
                Kind::Terminate => End::Signal(libc::SIGTERM),
                Kind::Hangup => End::Signal(libc::SIGHUP),
                other => End::Status(other.exit_code()),
            }
        }
        #[cfg(not(unix))]
        {
            End::Status(self.windows_status())
        }
    }
}

pub fn end_process(kind: Kind) -> ! {
    use std::io::Write as _;
    let _ = io::stdout().flush();
    let _ = io::stderr().flush();
    match kind.end() {
        #[cfg(unix)]
        End::Signal(signal) => {
            platform::die_of(signal);
            std::process::exit(kind.exit_code())
        }
        #[cfg(not(unix))]
        End::Signal(_) => std::process::exit(kind.exit_code()),
        End::Status(status) => std::process::exit(status),
    }
}

#[doc(hidden)]
pub fn raise(kind: Kind) {
    if state().watchers > 0 {
        store(kind);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

    fn serial() -> MutexGuard<'static, ()> {
        ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn send_to_self(signal: libc::c_int) {
        assert_eq!(unsafe { libc::raise(signal) }, 0);
    }

    fn disposition(signal: libc::c_int) -> libc::sighandler_t {
        let mut current: libc::sigaction = unsafe { std::mem::zeroed() };
        let result = unsafe { libc::sigaction(signal, std::ptr::null(), &mut current) };
        assert_eq!(result, 0);
        current.sa_sigaction
    }

    fn set_disposition(signal: libc::c_int, handler: libc::sighandler_t) {
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = handler;
        let result = unsafe { libc::sigaction(signal, &action, std::ptr::null_mut()) };
        assert_eq!(result, 0);
    }

    #[test]
    fn the_handlers_restart_the_system_calls_that_a_signal_interrupts() {
        let _serial = serial();
        let _watch = watch().unwrap();
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            let mut current: libc::sigaction = unsafe { std::mem::zeroed() };
            let result = unsafe { libc::sigaction(signal, std::ptr::null(), &mut current) };
            assert_eq!(result, 0);
            assert_ne!(current.sa_flags & libc::SA_RESTART, 0, "signal {signal}");
        }
    }

    #[test]
    fn a_failed_install_puts_back_the_handlers_it_had_set() {
        let _serial = serial();
        let _defaults = DefaultActions;
        let before = disposition(libc::SIGINT);
        let result = platform::install_for(&[
            (libc::SIGINT, Kind::Interrupt),
            (libc::SIGKILL, Kind::Terminate),
        ]);
        assert!(result.is_err(), "SIGKILL cannot be caught");
        assert_eq!(disposition(libc::SIGINT), before);
    }

    #[test]
    fn nothing_is_requested_until_a_signal_arrives() {
        let _serial = serial();
        let _watch = watch().unwrap();
        assert_eq!(requested(), None);
    }

    #[test]
    fn each_signal_is_recorded_as_its_own_kind() {
        let _serial = serial();
        for (signal, kind) in [
            (libc::SIGINT, Kind::Interrupt),
            (libc::SIGTERM, Kind::Terminate),
            (libc::SIGHUP, Kind::Hangup),
        ] {
            let _watch = watch().unwrap();
            send_to_self(signal);
            assert_eq!(requested(), Some(kind), "{signal}");
        }
    }

    #[test]
    fn the_first_request_stays_and_a_second_one_does_not_replace_it() {
        let _serial = serial();
        let _watch = watch().unwrap();
        send_to_self(libc::SIGTERM);
        send_to_self(libc::SIGINT);
        send_to_self(libc::SIGHUP);
        assert_eq!(requested(), Some(Kind::Terminate));
    }

    extern "C" fn some_other_handler(_: libc::c_int) {}

    struct DefaultActions;
    impl Drop for DefaultActions {
        fn drop(&mut self) {
            for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
                set_disposition(signal, libc::SIG_DFL);
            }
        }
    }

    #[test]
    fn a_signal_that_was_ignored_when_the_watch_began_stays_ignored() {
        let _serial = serial();
        let _defaults = DefaultActions;
        set_disposition(libc::SIGHUP, libc::SIG_IGN);
        set_disposition(libc::SIGINT, libc::SIG_IGN);
        {
            let _watch = watch().unwrap();
            assert_eq!(disposition(libc::SIGHUP), libc::SIG_IGN, "SIGHUP");
            assert_eq!(disposition(libc::SIGINT), libc::SIG_IGN, "SIGINT");
            assert_ne!(disposition(libc::SIGTERM), libc::SIG_IGN);
            assert_ne!(disposition(libc::SIGTERM), libc::SIG_DFL);
            send_to_self(libc::SIGHUP);
            send_to_self(libc::SIGINT);
            assert_eq!(requested(), None, "an ignored signal is no request");
            send_to_self(libc::SIGTERM);
            assert_eq!(requested(), Some(Kind::Terminate));
        }
        assert_eq!(disposition(libc::SIGHUP), libc::SIG_IGN);
        assert_eq!(disposition(libc::SIGINT), libc::SIG_IGN);
    }

    #[test]
    fn every_request_is_counted_and_the_first_kind_stays() {
        let _serial = serial();
        let _watch = watch().unwrap();
        assert_eq!(count(), 0);
        send_to_self(libc::SIGTERM);
        assert_eq!(count(), 1);
        send_to_self(libc::SIGINT);
        raise(Kind::Hangup);
        assert_eq!(count(), 3);
        assert_eq!(requested(), Some(Kind::Terminate));
    }

    #[test]
    fn the_count_and_the_notice_start_again_with_the_next_watch() {
        let _serial = serial();
        {
            let _watch = watch().unwrap();
            send_to_self(libc::SIGINT);
            send_to_self(libc::SIGINT);
            assert_eq!(count(), 2);
            assert!(claim_notice(), "the first caller gets the notice");
            assert!(!claim_notice(), "nobody else does");
        }
        assert_eq!(count(), 0);
        let _watch = watch().unwrap();
        assert_eq!(count(), 0);
        assert!(claim_notice(), "a new watch has a notice of its own");
    }

    #[test]
    fn the_end_of_the_watch_puts_the_previous_handlers_back_and_forgets_the_request() {
        let _serial = serial();
        let _defaults = DefaultActions;
        set_disposition(
            libc::SIGHUP,
            some_other_handler as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
        let before = [
            disposition(libc::SIGINT),
            disposition(libc::SIGTERM),
            disposition(libc::SIGHUP),
        ];
        {
            let _watch = watch().unwrap();
            assert_ne!(disposition(libc::SIGINT), before[0]);
            assert_ne!(disposition(libc::SIGTERM), before[1]);
            assert_ne!(disposition(libc::SIGHUP), before[2]);
            send_to_self(libc::SIGINT);
            assert_eq!(requested(), Some(Kind::Interrupt));
        }
        assert_eq!(disposition(libc::SIGINT), before[0]);
        assert_eq!(disposition(libc::SIGTERM), before[1]);
        assert_eq!(disposition(libc::SIGHUP), before[2]);
        assert_eq!(requested(), None);
    }

    #[test]
    fn overlapping_watches_keep_the_handlers_until_the_last_one_ends() {
        let _serial = serial();
        let outer = watch().unwrap();
        let inner = watch().unwrap();
        drop(outer);
        send_to_self(libc::SIGTERM);
        assert_eq!(requested(), Some(Kind::Terminate));
        drop(inner);
        assert_eq!(requested(), None);
        assert_eq!(disposition(libc::SIGTERM), libc::SIG_DFL);
    }

    #[test]
    fn a_request_is_waited_for_and_the_wait_is_bounded() {
        let _serial = serial();
        let _watch = watch().unwrap();
        let started = Instant::now();
        assert_eq!(wait_for_request(Duration::from_millis(40)), None);
        assert!(started.elapsed() < Duration::from_secs(5));
        let sender = thread::spawn(|| {
            thread::sleep(Duration::from_millis(30));
            assert_eq!(unsafe { libc::kill(libc::getpid(), libc::SIGTERM) }, 0);
        });
        let seen = wait_for_request(Duration::from_secs(10));
        assert_eq!(seen, Some(Kind::Terminate));
        sender.join().unwrap();
    }

    #[test]
    fn only_the_thread_that_holds_a_watch_is_armed() {
        let _serial = serial();
        assert!(!armed());
        let watch = watch().unwrap();
        assert!(armed());
        assert!(!thread::spawn(armed).join().unwrap());
        let second = self::watch().unwrap();
        drop(watch);
        assert!(armed(), "the second watch is still alive");
        drop(second);
        assert!(!armed());
    }

    #[test]
    fn the_hook_records_a_request_only_while_a_watch_is_alive() {
        let _serial = serial();
        raise(Kind::Interrupt);
        assert_eq!(requested(), None);
        let _watch = watch().unwrap();
        raise(Kind::Close);
        assert_eq!(requested(), Some(Kind::Close));
    }
}
