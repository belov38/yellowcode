use memory_budget::profile::{Phase, ProgressObserver, ProgressSnapshot};
use std::{
    ffi::OsStr,
    io::Write,
    sync::{Arc, Mutex, mpsc},
    thread::JoinHandle,
    time::{Duration, Instant},
};

pub(crate) const ENV: &str = "YELLOWCODE_PROGRESS";

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum Mode {
    Auto,
    Always,
    Never,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Style {
    Line,
    Log,
}

pub(crate) fn style(
    flag: Option<Mode>,
    env: Option<&OsStr>,
    stderr_terminal: bool,
) -> Result<Option<Style>, String> {
    let mode = match (flag, env) {
        (Some(mode), _) => mode,
        (None, None) => Mode::Auto,
        (None, Some(value)) => match value.to_str() {
            Some("auto") => Mode::Auto,
            Some("always") => Mode::Always,
            Some("never") => Mode::Never,
            _ => return Err(format!("{ENV} must be auto, always or never")),
        },
    };
    Ok(match mode {
        Mode::Auto if stderr_terminal => Some(Style::Line),
        Mode::Auto | Mode::Never => None,
        Mode::Always => Some(Style::Log),
    })
}

const TICK: Duration = Duration::from_millis(250);
const LINE_AFTER: Duration = Duration::from_secs(1);
const LINE_WIDTH: usize = 79;
const LOG_EVERY: Duration = Duration::from_secs(10);

fn describe(snapshot: &ProgressSnapshot) -> String {
    let label = match snapshot.phase {
        Phase::Startup => "working",
        phase => phase.name(),
    };
    if snapshot.total > 0 {
        format!(
            "{label} {}/{}",
            snapshot.done.min(snapshot.total),
            snapshot.total
        )
    } else {
        label.to_owned()
    }
}

fn elapsed(time: Duration) -> String {
    let s = time.as_secs();
    match (s / 3600, s / 60 % 60, s % 60) {
        (0, 0, s) => format!("{s}s"),
        (0, m, s) => format!("{m}m{s:02}s"),
        (h, m, s) => format!("{h}h{m:02}m{s:02}s"),
    }
}

struct Painter {
    style: Style,
    logged: Option<(u64, Duration)>,
}

impl Painter {
    fn new(style: Style) -> Self {
        Self {
            style,
            logged: None,
        }
    }
    fn look(&mut self, snapshot: &ProgressSnapshot, at: Duration) -> Option<String> {
        match self.style {
            Style::Line => (at >= LINE_AFTER).then(|| {
                format!("yellowcode: {}, {}", describe(snapshot), elapsed(at))
                    .chars()
                    .take(LINE_WIDTH)
                    .collect()
            }),
            Style::Log => {
                let due = match self.logged {
                    None => true,
                    Some((transitions, when)) => {
                        transitions != snapshot.transitions || at.saturating_sub(when) >= LOG_EVERY
                    }
                };
                due.then(|| {
                    self.logged = Some((snapshot.transitions, at));
                    format!(
                        "yellowcode: progress: {}, {}",
                        describe(snapshot),
                        elapsed(at)
                    )
                })
            }
        }
    }
    fn last(&self, at: Duration) -> Option<String> {
        (self.style == Style::Log)
            .then(|| format!("yellowcode: progress: finished, {}", elapsed(at)))
    }
}

struct Sink {
    w: Box<dyn Write + Send>,
    shown: usize,
    stopped: bool,
}

impl Sink {
    fn line(&mut self, text: &str) {
        let len = text.chars().count();
        let pad = self.shown.saturating_sub(len);
        let _ = write!(self.w, "\r{text}{:pad$}", "");
        let _ = self.w.flush();
        self.shown = len;
    }
    fn clear(&mut self) {
        if self.shown > 0 {
            let _ = write!(self.w, "\r{:width$}\r", "", width = self.shown);
            let _ = self.w.flush();
            self.shown = 0;
        }
    }
    fn log(&mut self, text: &str) {
        let _ = writeln!(self.w, "{text}");
        let _ = self.w.flush();
    }
}

pub(crate) struct Reporter {
    sink: Arc<Mutex<Sink>>,
    stop: Mutex<Option<mpsc::Sender<()>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    style: Style,
}

impl Reporter {
    pub(crate) fn start(style: Style, w: Box<dyn Write + Send>) -> Option<Arc<Self>> {
        let observer = ProgressObserver::install()?;
        let sink = Arc::new(Mutex::new(Sink {
            w,
            shown: 0,
            stopped: false,
        }));
        let (tx, rx) = mpsc::channel::<()>();
        let thread_sink = sink.clone();
        let worker = std::thread::Builder::new()
            .name("progress".into())
            .spawn(move || {
                let start = Instant::now();
                let mut painter = Painter::new(style);
                while let Err(mpsc::RecvTimeoutError::Timeout) = rx.recv_timeout(TICK) {
                    let text = painter.look(&observer.snapshot(), start.elapsed());
                    let mut sink = thread_sink.lock().unwrap_or_else(|e| e.into_inner());
                    match text {
                        _ if sink.stopped => break,
                        Some(text) if style == Style::Line => sink.line(&text),
                        Some(text) => sink.log(&text),
                        None => {}
                    }
                }
                let mut sink = thread_sink.lock().unwrap_or_else(|e| e.into_inner());
                sink.clear();
                if let Some(text) = painter.last(start.elapsed()) {
                    sink.log(&text);
                }
                drop(observer);
            })
            .ok()?;
        Some(Arc::new(Self {
            sink,
            stop: Mutex::new(Some(tx)),
            worker: Mutex::new(Some(worker)),
            style,
        }))
    }

    pub(crate) fn stop(&self) {
        self.sink.lock().unwrap_or_else(|e| e.into_inner()).stopped = true;
        drop(self.stop.lock().unwrap_or_else(|e| e.into_inner()).take());
        if let Some(worker) = self.worker.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = worker.join();
        }
    }

    fn clear_line(&self) {
        self.sink.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}

impl Drop for Reporter {
    fn drop(&mut self) {
        self.stop();
    }
}

pub(crate) struct StopOnWrite<'a> {
    w: &'a mut dyn Write,
    reporter: Option<Arc<Reporter>>,
}

impl<'a> StopOnWrite<'a> {
    pub(crate) fn new(
        w: &'a mut dyn Write,
        reporter: Option<&Arc<Reporter>>,
        stdout_terminal: bool,
    ) -> Self {
        let reporter = reporter
            .filter(|r| stdout_terminal && r.style == Style::Line)
            .cloned();
        Self { w, reporter }
    }
}

impl Write for StopOnWrite<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Some(reporter) = self.reporter.take() {
            reporter.stop();
        }
        self.w.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.w.flush()
    }
}

pub(crate) struct ClearOnWrite<'a> {
    w: &'a mut dyn Write,
    reporter: Option<Arc<Reporter>>,
}

impl<'a> ClearOnWrite<'a> {
    pub(crate) fn new(w: &'a mut dyn Write, reporter: Option<&Arc<Reporter>>) -> Self {
        let reporter = reporter.filter(|r| r.style == Style::Line).cloned();
        Self { w, reporter }
    }
}

impl Write for ClearOnWrite<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Some(reporter) = &self.reporter {
            reporter.clear_line();
        }
        self.w.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.w.flush()
    }
}
