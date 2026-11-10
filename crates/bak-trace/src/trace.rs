use crate::{
    events::{BakHeader, ConfigStats, InterruptKind, Outcome, Refusal, RunShape, StepRecord},
    facts::ServerFacts,
    hasher::Salt,
    literals::{LiteralKind, LiteralSet},
    memory::peak_resident_bytes,
    paths::classify,
    redact::Redactor,
    render::{end_line, header_line, scrub_across, scrub_line},
    ring::{Ring, RingLimits},
    scan::{HEAD_MESSAGES, MAX_MESSAGES, MessageSource},
    sink::{self, Destination, WriteFailure},
    tempdb::TempDbName,
    value::{Event, Hashed, Redacted},
};
use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
    fs::File,
    io,
    marker::PhantomData,
    path::PathBuf,
    sync::{
        Arc, Mutex, PoisonError, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};

const MAX_EVENT_BYTES: usize = 16 * 1024;
const MAX_MESSAGE_CHARS: usize = 512;
const MAX_FAILURE_CHARS: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub cap_bytes: usize,
    pub head_events: usize,
    pub max_events: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            cap_bytes: 8 * 1024 * 1024,
            head_events: 64,
            max_events: 100_000,
        }
    }
}

impl Limits {
    pub(crate) fn reserve(&self) -> usize {
        (16 * 1024).min(self.cap_bytes / 3)
    }

    pub(crate) fn ring(&self) -> RingLimits {
        let budget = self.cap_bytes.saturating_sub(self.reserve());
        RingLimits {
            budget_bytes: budget,
            head_events: self.head_events,
            head_bytes: budget / 4,
            max_events: self.max_events,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rendered {
    pub text: String,
    pub leak_scrubbed: u64,
    pub dropped_events: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finished {
    pub path: Option<PathBuf>,
    pub failure: Option<WriteFailure>,
    pub leak_scrubbed: u64,
}

#[derive(Clone, Default)]
struct StepTotals {
    count: u64,
    total_ms: u64,
    max_ms: u64,
    failed: u64,
}

struct Core {
    ring: Ring,
    seq: u64,
}

struct Inner {
    cli: &'static str,
    commit: &'static str,
    limits: Limits,
    redactor: Redactor,
    start: Instant,
    start_unix: i64,
    core: Mutex<Core>,
    totals: Mutex<BTreeMap<&'static str, StepTotals>>,
    destination: Mutex<(Destination, Option<File>)>,
    sql_started: AtomicBool,
    finished: AtomicBool,
    told_databases: Mutex<Vec<String>>,
    config: Mutex<Option<ConfigStats>>,
}

#[derive(Clone)]
pub struct Trace {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Trace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Trace(..)")
    }
}

pub struct Scope {
    trace: Trace,
    _not_send: PhantomData<*const ()>,
}

thread_local! {
    static CURRENT: RefCell<Vec<Trace>> = const { RefCell::new(Vec::new()) };
}

static ACTIVE: Mutex<Vec<Weak<Inner>>> = Mutex::new(Vec::new());

impl Drop for Scope {
    fn drop(&mut self) {
        CURRENT.with(|stack| {
            let mut stack = stack.borrow_mut();
            if let Some(at) = stack
                .iter()
                .rposition(|t| Arc::ptr_eq(&t.inner, &self.trace.inner))
            {
                stack.remove(at);
            }
        });
        let mut active = lock(&ACTIVE);
        if let Some(at) = active
            .iter()
            .rposition(|w| w.as_ptr() == Arc::as_ptr(&self.trace.inner))
        {
            active.remove(at);
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Trace {
    pub fn new(cli: &'static str, commit: &'static str) -> Trace {
        Trace::with_limits(cli, commit, Limits::default())
    }

    pub fn with_limits(cli: &'static str, commit: &'static str, limits: Limits) -> Trace {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
        Trace::with_parts(cli, commit, limits, Salt::random(), now)
    }

    #[doc(hidden)]
    pub fn with_parts(
        cli: &'static str,
        commit: &'static str,
        limits: Limits,
        salt: Salt,
        start_unix: i64,
    ) -> Trace {
        Trace {
            inner: Arc::new(Inner {
                cli,
                commit,
                limits,
                redactor: Redactor::new(salt),
                start: Instant::now(),
                start_unix,
                core: Mutex::new(Core {
                    ring: Ring::new(limits.ring()),
                    seq: 0,
                }),
                totals: Mutex::new(BTreeMap::new()),
                destination: Mutex::new((Destination::default(), None)),
                sql_started: AtomicBool::new(false),
                finished: AtomicBool::new(false),
                told_databases: Mutex::new(Vec::new()),
                config: Mutex::new(None),
            }),
        }
    }

    pub fn hash(&self, text: &str) -> Hashed {
        self.inner.redactor.hash(text)
    }

    pub fn redactor(&self) -> Redactor {
        self.inner.redactor.clone()
    }

    pub fn redact(&self, text: &str) -> Redacted {
        self.inner.redactor.redact(text)
    }

    pub fn sensitive(&self, kind: LiteralKind, value: &str) {
        self.inner.redactor.add_literal(kind, value);
    }

    pub fn elapsed_ms(&self) -> u64 {
        u64::try_from(self.inner.start.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    pub fn emit(&self, event: Event) {
        let mut core = lock(&self.inner.core);
        let seq = core.seq;
        core.seq += 1;
        let t = self.elapsed_ms();
        core.ring.push(bounded_line(&event, seq, t));
    }

    pub fn run(&self, shape: &RunShape<'_>) {
        self.sensitive(LiteralKind::Path, shape.source);
        self.sensitive(LiteralKind::Path, shape.output);
        self.sensitive(LiteralKind::Server, shape.server);
        if let Some(user) = shape.user {
            self.sensitive(LiteralKind::Name, user);
        }
        if let Some(sqlcmd) = shape.sqlcmd {
            self.sensitive_client_path(sqlcmd);
        }
        let event = Event::new("run")
            .tag("cmd", shape.command)
            .tag("to", shape.to)
            .unum("backup_set", u64::from(shape.backup_set))
            .tag("auth", if shape.trusted_auth { "trusted" } else { "sql" })
            .flag("trust_cert", shape.trust_server_certificate)
            .opt_unum("timeout_s", shape.lifecycle_timeout_secs)
            .flag("memory_limit", shape.memory_limit_given)
            .opt_flag("raw", shape.raw)
            .flag("trace", shape.trace_requested)
            .opt_unum(
                "cpus",
                std::thread::available_parallelism()
                    .ok()
                    .map(|n| n.get() as u64),
            )
            .tag("source_kind", classify(shape.source).tag())
            .hash("source", self.hash(shape.source))
            .tag("output_kind", classify(shape.output).tag())
            .hash("output", self.hash(shape.output))
            .hash("server", self.hash(shape.server))
            .tag(
                "sqlcmd",
                if shape.sqlcmd.is_some() {
                    "given"
                } else {
                    "default"
                },
            );
        let event = match shape.sqlcmd {
            Some(path) => event
                .tag("sqlcmd_kind", classify(path).tag())
                .hash("sqlcmd_path", self.hash(path)),
            None => event.null("sqlcmd_kind").null("sqlcmd_path"),
        };
        self.emit(event);
    }

    fn sensitive_client_path(&self, path: &str) {
        let (directory, tool) = match path.rfind(['/', '\\']) {
            Some(at) => (
                path.get(..=at).unwrap_or_default(),
                path.get(at + 1..).unwrap_or_default(),
            ),
            None => ("", path),
        };
        if matches!(
            tool.to_lowercase().as_str(),
            "sqlcmd" | "sqlcmd.exe" | "sqlcmd.cmd" | "sqlcmd.bat"
        ) {
            self.sensitive(LiteralKind::Name, path);
            if !directory.is_empty() {
                self.sensitive(LiteralKind::Path, directory);
            }
        } else {
            self.sensitive(LiteralKind::Path, path);
        }
    }

    pub fn temp_db(&self, db: &TempDbName) {
        {
            let mut told = lock(&self.inner.told_databases);
            if told.iter().any(|name| name == db.as_str()) {
                return;
            }
            told.push(db.as_str().to_owned());
        }
        self.emit(Event::new("temp_db").text("name", self.redact(db.as_str())));
    }

    pub fn cleanup_error(&self, step: &'static str, sql_error: u32) {
        self.emit(
            Event::new("cleanup_error")
                .tag("step", step)
                .unum("error", u64::from(sql_error)),
        );
    }

    pub fn server(&self, facts: &ServerFacts, data_path: Option<&str>) {
        let event = Event::new("server")
            .opt_num("product_year", facts.product_year)
            .opt_num("major", facts.major)
            .opt_num("minor", facts.minor)
            .opt_num("build", facts.build)
            .opt_num("revision", facts.revision)
            .opt_num("cu", facts.cumulative_update)
            .opt_num("kb", facts.kb)
            .tag("arch", facts.arch)
            .tag("os", facts.os)
            .tag("edition", facts.edition)
            .tag("collation", facts.collation);
        self.emit(match data_path {
            Some(path) => event.hash("data_path", self.hash(path)),
            None => event.null("data_path"),
        });
    }

    pub fn bak_header(&self, header: &BakHeader) {
        self.emit(
            Event::new("bak_header")
                .opt_num("backup_type", header.backup_type)
                .opt_flag("compressed", header.compressed)
                .opt_flag("encrypted", header.encrypted)
                .opt_flag("snapshot", header.snapshot)
                .opt_flag("damaged", header.damaged)
                .opt_unum("sets", header.sets)
                .opt_unum("position", header.position)
                .opt_unum("family_count", header.family_count)
                .opt_num("software_major", header.software_major)
                .opt_num("software_minor", header.software_minor)
                .opt_num("software_build", header.software_build)
                .opt_num("database_version", header.database_version)
                .opt_num("compatibility_level", header.compatibility_level)
                .opt_unum("backup_bytes", header.backup_bytes)
                .opt_unum("compressed_bytes", header.compressed_bytes)
                .opt_tag("recovery_model", header.recovery_model),
        );
    }

    pub fn step(&self, record: &StepRecord) {
        {
            let mut totals = lock(&self.inner.totals);
            let total = totals.entry(record.step).or_default();
            total.count += 1;
            total.total_ms = total.total_ms.saturating_add(record.duration_ms);
            total.max_ms = total.max_ms.max(record.duration_ms);
            total.failed += u64::from(!record.ok);
        }
        let all = &record.stderr.messages;
        let tail = MAX_MESSAGES - HEAD_MESSAGES;
        let kept: Vec<_> = if all.len() <= MAX_MESSAGES {
            all.iter().collect()
        } else {
            all.iter()
                .take(HEAD_MESSAGES)
                .chain(all.iter().skip(all.len() - tail))
                .collect()
        };
        let omitted = record.stderr.omitted_messages + (all.len() - kept.len()) as u64;
        self.emit(
            Event::new("step")
                .tag("step", record.step)
                .flag("nested", record.nested)
                .flag("ok", record.ok)
                .flag("started", record.started)
                .unum("start_ms", record.started_ms)
                .unum("dur_ms", record.duration_ms)
                .opt_num("exit", record.exit_code.map(i64::from))
                .opt_num("signal", record.signal.map(i64::from))
                .flag("timed_out", record.timed_out)
                .unum("stdout_bytes", record.stdout_bytes)
                .unum("stdout_lines", record.stdout_lines)
                .unum("stderr_bytes", record.stderr.bytes)
                .unum("stderr_lines", record.stderr.lines)
                .unum("notices", record.stderr.notice_lines)
                .opt_unum("percent", record.stderr.percent.map(u64::from))
                .unum("msgs", kept.len() as u64)
                .unum("msgs_omitted", omitted)
                .tag(
                    "auth",
                    if record.command.trusted {
                        "trusted"
                    } else {
                        "sql"
                    },
                )
                .flag("trust_cert", record.command.trust_certificate)
                .unum("query_timeout_s", record.command.query_timeout_secs)
                .opt_num("os_error", record.os_error.map(i64::from))
                .opt_tag("io_kind", record.io_kind),
        );
        for (index, message) in kept.iter().enumerate() {
            let (text, truncated) = if message.text.is_empty() {
                (None, false)
            } else {
                let done = self
                    .inner
                    .redactor
                    .redact_full(&message.text, Some(MAX_MESSAGE_CHARS));
                (Some(done.text), done.truncated)
            };
            let event = Event::new("msg")
                .tag("step", record.step)
                .unum("n", index as u64)
                .tag(
                    "source",
                    match message.source {
                        MessageSource::Server => "server",
                        MessageSource::Client => "client",
                    },
                )
                .opt_unum("msg", message.msg.map(u64::from))
                .opt_unum("level", message.level.map(u64::from))
                .opt_unum("state", message.state.map(u64::from))
                .opt_unum("line", message.line.map(u64::from))
                .opt_num("os_error", message.os_error);
            let event = match text {
                Some(text) => event.text("text", text),
                None => event.null("text"),
            };
            self.emit(event.flag("truncated", truncated));
        }
    }

    pub fn config_file(&self, parts: u64, bytes: u64) {
        let mut counted = lock(&self.inner.config);
        let stats = counted.get_or_insert_with(ConfigStats::default);
        stats.rows = stats.rows.saturating_add(parts);
        stats.bytes = stats.bytes.saturating_add(bytes);
        stats.files += 1;
        stats.multipart_files += u64::from(parts > 1);
        stats.max_parts = stats.max_parts.max(parts);
    }

    pub fn config_done(&self) {
        let counted = lock(&self.inner.config).take();
        if let Some(stats) = counted {
            self.config_stats(&stats);
        }
    }

    pub fn config_stats(&self, stats: &ConfigStats) {
        self.emit(
            Event::new("config_stats")
                .unum("rows", stats.rows)
                .unum("bytes", stats.bytes)
                .unum("files", stats.files)
                .unum("multipart_files", stats.multipart_files)
                .unum("max_parts", stats.max_parts)
                .opt_unum("peak_rss_bytes", peak_resident_bytes()),
        );
    }

    pub fn refusal(&self, refusal: &Refusal) {
        self.emit(
            Event::new("refusal")
                .tag("class", refusal.class)
                .tag("field", refusal.field)
                .opt_num("part", refusal.part)
                .opt_num("expected", refusal.expected)
                .opt_num("observed", refusal.observed),
        );
    }

    pub fn failure(&self, class: &'static str, stage: &'static str, display: &str) {
        let done = self
            .inner
            .redactor
            .redact_full(display, Some(MAX_FAILURE_CHARS));
        self.emit(
            Event::new("failure")
                .tag("class", class)
                .tag("stage", stage)
                .text("text", done.text)
                .flag("truncated", done.truncated),
        );
    }

    pub fn cleanup(
        &self,
        db: &TempDbName,
        size_mb: Option<u64>,
        dropped: bool,
        fallback_sql_given: bool,
    ) {
        self.emit(
            Event::new("cleanup")
                .text("db", self.redact(db.as_str()))
                .opt_unum("size_mb", size_mb)
                .flag("dropped", dropped)
                .flag("fallback_sql", fallback_sql_given),
        );
    }

    pub fn mark_sql_started(&self) {
        self.inner.sql_started.store(true, Ordering::Release);
    }

    pub fn sql_started(&self) -> bool {
        self.inner.sql_started.load(Ordering::Acquire)
    }

    pub fn set_destination(&self, destination: Destination) -> io::Result<()> {
        let file = match &destination.explicit {
            Some(path) => {
                let file = sink::create_new(path)?;
                self.sensitive(LiteralKind::Path, &path.to_string_lossy());
                Some(file)
            }
            None => None,
        };
        *lock(&self.inner.destination) = (destination, file);
        Ok(())
    }

    pub fn render(&self, outcome: Outcome, exit_code: i32) -> Rendered {
        self.render_with_secret(outcome, exit_code, &secret_from_environment())
    }

    pub fn render_with_secret(&self, outcome: Outcome, exit_code: i32, secret: &[u8]) -> Rendered {
        let literals = self.inner.redactor.scan_set();
        let cap = self.inner.limits.cap_bytes;
        let (head_raw, tail_raw, events, ring_dropped_events, ring_dropped_bytes) = {
            let core = lock(&self.inner.core);
            let (dropped_events, dropped_bytes) = core.ring.dropped();
            (
                core.ring.head().map(str::to_owned).collect::<Vec<String>>(),
                core.ring
                    .tail()
                    .map(str::to_owned)
                    .collect::<VecDeque<String>>(),
                core.seq,
                dropped_events,
                dropped_bytes,
            )
        };
        let mut hits = 0_u64;
        let mut head_raw = head_raw;
        let mut tail_raw: Vec<String> = tail_raw.into_iter().collect();
        let mut across = literals.clone();
        if !secret.is_empty() {
            across.add(LiteralKind::Secret, &String::from_utf8_lossy(secret));
        }
        hits += scrub_across(&mut head_raw, &across);
        hits += scrub_across(&mut tail_raw, &across);
        let tail_raw: VecDeque<String> = tail_raw.into();
        let header = scrubbed(
            &header_line(self.inner.cli, self.inner.commit, self.inner.start_unix),
            &literals,
            secret,
            &mut hits,
        );
        let head: Vec<String> = head_raw
            .iter()
            .map(|line| scrubbed(line, &literals, secret, &mut hits))
            .collect();
        let mut tail: VecDeque<String> = tail_raw
            .iter()
            .map(|line| scrubbed(line, &literals, secret, &mut hits))
            .collect();
        let totals: Vec<String> = lock(&self.inner.totals)
            .iter()
            .map(|(step, total)| {
                Event::new("timing")
                    .tag("step", step)
                    .unum("count", total.count)
                    .unum("total_ms", total.total_ms)
                    .unum("max_ms", total.max_ms)
                    .unum("failed", total.failed)
                    .render_bare()
            })
            .map(|line| scrubbed(&line, &literals, secret, &mut hits))
            .collect();
        let peak = peak_resident_bytes();
        let elapsed = self.elapsed_ms();
        let (mut dropped_events, mut dropped_bytes) = (ring_dropped_events, ring_dropped_bytes);
        loop {
            let mut fixed_hits = 0_u64;
            let gap = (dropped_events > 0).then(|| {
                scrubbed(
                    &Event::new("gap")
                        .unum("dropped_events", dropped_events)
                        .unum("dropped_bytes", dropped_bytes)
                        .render_bare(),
                    &literals,
                    secret,
                    &mut fixed_hits,
                )
            });
            let summary = scrubbed(
                &Event::new("summary")
                    .unum("elapsed_ms", elapsed)
                    .unum("events", events)
                    .unum("dropped_events", dropped_events)
                    .unum("dropped_bytes", dropped_bytes)
                    .opt_unum("peak_rss_bytes", peak)
                    .render_bare(),
                &literals,
                secret,
                &mut fixed_hits,
            );
            let end = end_line(outcome, exit_code, hits + fixed_hits);
            let fixed = header.len()
                + 1
                + gap.as_ref().map_or(0, |line| line.len() + 1)
                + summary.len()
                + 1
                + end.len()
                + 1;
            let body = size_of_lines(head.iter()) + size_of_lines(tail.iter());
            if !fits(fixed, body, cap)
                && let Some(old) = tail.pop_front()
            {
                dropped_events += 1;
                dropped_bytes += old.len() as u64 + 1;
                continue;
            }
            let mut room = cap.saturating_sub(fixed + body);
            let mut text = String::with_capacity(fixed + body + 1024);
            let mut push = |line: &str| {
                text.push_str(line);
                text.push('\n');
            };
            push(&header);
            head.iter().for_each(|line| push(line));
            if let Some(gap) = &gap {
                push(gap);
            }
            tail.iter().for_each(|line| push(line));
            for line in &totals {
                if line.len() < room {
                    room -= line.len() + 1;
                    push(line);
                }
            }
            push(&summary);
            push(&end);
            return Rendered {
                text,
                leak_scrubbed: hits + fixed_hits,
                dropped_events,
            };
        }
    }

    pub fn finish(&self, outcome: Outcome, exit_code: i32) -> Finished {
        let nothing = Finished {
            path: None,
            failure: None,
            leak_scrubbed: 0,
        };
        if self.inner.finished.swap(true, Ordering::AcqRel) {
            return nothing;
        }
        let (explicit, output) = {
            let destination = lock(&self.inner.destination);
            (
                destination.0.explicit.is_some(),
                destination.0.output.clone(),
            )
        };
        let wanted = if outcome == Outcome::Ok {
            explicit
        } else {
            explicit || self.sql_started()
        };
        if !wanted {
            return nothing;
        }
        let rendered = self.render(outcome, exit_code);
        let result = {
            let mut destination = lock(&self.inner.destination);
            match (destination.0.explicit.clone(), destination.1.take()) {
                (Some(path), Some(file)) => sink::write_to(file, &rendered.text)
                    .map(|()| path)
                    .map_err(|error| WriteFailure::of(&error)),
                _ => {
                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
                    sink::write_in(
                        &sink::candidate_dirs(output.as_deref()),
                        now,
                        &rendered.text,
                    )
                }
            }
        };
        match result {
            Ok(path) => Finished {
                path: Some(path),
                failure: None,
                leak_scrubbed: rendered.leak_scrubbed,
            },
            Err(failure) => Finished {
                path: None,
                failure: Some(failure),
                leak_scrubbed: rendered.leak_scrubbed,
            },
        }
    }

    pub fn finish_interrupted(&self, kind: InterruptKind) -> Finished {
        if !self.inner.finished.load(Ordering::Acquire) {
            self.emit(Event::new("interrupt").tag("kind", kind.tag()));
        }
        self.finish(Outcome::Interrupted, kind.exit_code())
    }

    pub fn finish_panic(&self, message: &str, file: &str, line: u32, column: u32) -> Finished {
        if !self.inner.finished.load(Ordering::Acquire) {
            let message = self
                .inner
                .redactor
                .redact_full(message, Some(MAX_MESSAGE_CHARS));
            self.emit(
                Event::new("panic")
                    .text("message", message.text)
                    .text("file", self.inner.redactor.redact_source_path(file))
                    .unum("line", u64::from(line))
                    .unum("col", u64::from(column)),
            );
        }
        self.finish(Outcome::Panic, 101)
    }

    pub fn enter(&self) -> Scope {
        CURRENT.with(|stack| stack.borrow_mut().push(self.clone()));
        lock(&ACTIVE).push(Arc::downgrade(&self.inner));
        Scope {
            trace: self.clone(),
            _not_send: PhantomData,
        }
    }

    pub fn current() -> Option<Trace> {
        CURRENT.with(|stack| stack.borrow().last().cloned())
    }

    pub(crate) fn sole_active() -> Option<Trace> {
        let mut active = lock(&ACTIVE);
        active.retain(|w| w.strong_count() > 0);
        match active.as_slice() {
            [only] => only.upgrade().map(|inner| Trace { inner }),
            _ => None,
        }
    }
}

fn scrubbed(line: &str, literals: &LiteralSet, secret: &[u8], hits: &mut u64) -> String {
    let (clean, found) = scrub_line(line, literals, secret);
    *hits += found;
    clean
}

fn bounded_line(event: &Event, seq: u64, t_ms: u64) -> String {
    let line = event.render(seq, t_ms);
    if line.len() > MAX_EVENT_BYTES {
        return Event::new("oversized")
            .tag("of", event.kind())
            .unum("bytes", line.len() as u64)
            .render(seq, t_ms);
    }
    line
}

fn fits(fixed: usize, body: usize, cap: usize) -> bool {
    fixed + body <= cap
}

fn size_of_lines<'a>(lines: impl Iterator<Item = &'a String>) -> usize {
    lines.map(|line| line.len() + 1).sum()
}

fn secret_from_environment() -> Vec<u8> {
    let Some(value) = std::env::var_os("SQLCMDPASSWORD") else {
        return Vec::new();
    };
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        value.as_bytes().to_vec()
    }
    #[cfg(not(unix))]
    {
        value.to_string_lossy().into_owned().into_bytes()
    }
}
