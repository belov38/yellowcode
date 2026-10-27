use crate::{
    Result,
    viewer::source::{self, SourceKind},
};
use infobase::{
    dump::{UsersLayout, write_dt},
    raw::RawIb,
};
use mdconf::EntrySource;
use std::{
    io::{BufWriter, Write},
    path::{Path, PathBuf},
};

#[derive(Clone, Copy)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
pub enum Format {
    Cf,
    Dt,
    #[cfg_attr(feature = "clap", value(name = "1cd"))]
    Infobase,
}
#[derive(Clone, Copy)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
pub enum Layout {
    Users8,
    UsersSpr11,
}
pub struct Request {
    pub source: PathBuf,
    pub to: Format,
    pub output: PathBuf,
    pub users_layout: Option<Layout>,
    pub memory_limit: Option<String>,
    pub without_users: bool,
}

fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn file(output: &Path, write: impl FnOnce(&mut dyn Write) -> Result<()>) -> Result<()> {
    let mut temp = tempfile::Builder::new()
        .prefix(".yellowcode-")
        .tempfile_in(parent(output))?;
    {
        let mut buffer = BufWriter::new(temp.as_file_mut());
        write(&mut buffer)?;
        buffer.flush()?;
    }
    temp.as_file().sync_all()?;
    temp.persist_noclobber(output).map_err(|error| {
        let tempfile::PersistError { error, file } = error;
        drop(file);
        error
    })?;
    Ok(())
}

struct RawData<'a> {
    source: &'a dyn EntrySource,
    names: Vec<String>,
}
impl v8cont::DataSource for RawData<'_> {
    fn write_data(&mut self, index: usize, out: &mut dyn Write) -> v8cont::Result<()> {
        let name = self
            .names
            .get(index)
            .ok_or_else(|| v8cont::Error::LayoutMismatch("CONFIG entry index".into()))?;
        let bytes = self
            .source
            .stored_owned(name)
            .map_err(|e| v8cont::Error::Io(std::io::Error::other(e)))?
            .ok_or_else(|| {
                v8cont::Error::LayoutMismatch(format!("CONFIG entry disappeared: {name}"))
            })?;
        out.write_all(&bytes)?;
        Ok(())
    }
}

fn raw_cf(source: &Path, memory: Option<&source::OpenMemory>, out: &mut dyn Write) -> Result<()> {
    let consume = |entries: &dyn EntrySource| {
        let list = entries.list_owned()?;
        let mut specs = Vec::new();
        for meta in list.iter() {
            let data = entries
                .stored_owned(&meta.name)?
                .ok_or_else(|| format!("CONFIG missing entry: {}", meta.name))?;
            specs.push(v8cont::EntrySpec {
                name: meta.name.clone(),
                ctime: meta.ctime,
                mtime: meta.mtime,
                attrs: meta.attrs,
                data_len: Some(u64::try_from(data.len())?),
            });
        }
        if specs.is_empty() {
            return Err("CONFIG has no configuration entries".into());
        }
        let mut data = RawData {
            source: entries,
            names: specs.iter().map(|s| s.name.clone()).collect(),
        };
        v8cont::write_container(
            out,
            &specs,
            &mut data,
            &v8cont::WriteOptions::top_level(v8cont::Format::F16),
        )?;
        Ok(())
    };
    match memory {
        Some(memory) => source::with_config_entries_with_memory(source, memory, consume),
        None => source::with_config_entries(source, consume),
    }
}

pub fn run(args: Request, out: &mut dyn Write, err: &mut dyn Write) -> Result<i32> {
    let source = source::resolve(&args.source);
    let kind = source::detect(&source)?;
    match (kind, args.to) {
        (SourceKind::XmlDir, _) => {
            return Err("XML conversion is deferred until the complete feature is ready".into());
        }
        (SourceKind::Dt, Format::Infobase) => {}
        (_, Format::Infobase) => {
            return Err("CF to 1CD needs D6 (configuration acceptance)".into());
        }
        (SourceKind::Dt | SourceKind::Infobase, Format::Cf)
        | (SourceKind::Infobase, Format::Dt) => {}
        _ => return Err("unsupported conversion direction".into()),
    }
    if matches!(args.to, Format::Dt) && args.users_layout.is_none() {
        return Err(
            "1CD to DT requires --users-layout users8|users-spr11; automatic layout awaits D4b"
                .into(),
        );
    }
    if !matches!(args.to, Format::Dt) && args.users_layout.is_some() {
        return Err("--users-layout applies only to 1CD to DT".into());
    }
    let restore = kind == SourceKind::Dt && matches!(args.to, Format::Infobase);
    if !restore && args.without_users {
        return Err("--without-users applies only to DT to 1CD".into());
    }
    if kind != SourceKind::Dt && args.memory_limit.is_some() {
        return Err("--memory-limit applies only to DT conversion".into());
    }
    let memory = if kind == SourceKind::Dt {
        Some(crate::service::memory::Settings::memory_only(args.memory_limit.clone()).resolve()?)
    } else {
        None
    };
    match std::fs::symlink_metadata(&args.output) {
        Ok(_) => return Err(format!("output already exists: {}", args.output.display()).into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    if kind == SourceKind::Dt && !matches!(args.to, Format::Infobase) {
        writeln!(
            err,
            "yellowcode: DT metadata prefix only; later folders and tail are not validated"
        )?;
    }
    let mut report = None;
    match args.to {
        Format::Cf => {
            let opening = memory
                .as_ref()
                .map(|(context, _)| source::OpenMemory::from_context(context));
            file(&args.output, |out| raw_cf(&source, opening.as_ref(), out))?;
        }
        Format::Dt => {
            let layout = match args.users_layout.ok_or("missing --users-layout")? {
                Layout::Users8 => UsersLayout::Users8,
                Layout::UsersSpr11 => UsersLayout::UsersSpr11,
            };
            let mut opts = onecd::OpenOptions::read_only();
            opts.locking = onecd::Locking::None;
            let raw = RawIb::open_with(&source, opts)?;
            let txn = raw.db().read();
            file(&args.output, |out| {
                write_dt(&txn, layout, out)?;
                Ok(())
            })?;
        }
        Format::Infobase => {
            let (context, _reserve) = memory.ok_or("memory settings")?;
            report = Some(restore_dt(
                &source,
                &args.output,
                &context,
                args.without_users,
                err,
            )?);
        }
    };
    let code = 0;
    let mut status =
        serde_json::json!({"schemaVersion": 1, "output": args.output, "complete": true});
    if let (Some((r, seconds)), Some(map)) = (&report, status.as_object_mut()) {
        map.insert("rows".into(), r.rows.into());
        map.insert("files".into(), r.files.into());
        map.insert("tables".into(), r.tables.into());
        map.insert("usersDropped".into(), r.users_dropped.into());
        map.insert("sourceSha256".into(), r.source_sha256.clone().into());
        map.insert("seconds".into(), (*seconds).into());
        map.insert("warningCount".into(), r.warnings.len().into());
        map.insert(
            "warnings".into(),
            r.warnings
                .iter()
                .map(|w| serde_json::json!({"kind": w.kind, "items": w.items}))
                .collect::<Vec<_>>()
                .into(),
        );
    }
    if let Err(error) = serde_json::to_writer(&mut *out, &status) {
        return if error.io_error_kind() == Some(std::io::ErrorKind::BrokenPipe) {
            Ok(code)
        } else {
            Err(error.into())
        };
    }
    match writeln!(out) {
        Err(error) if error.kind() != std::io::ErrorKind::BrokenPipe => Err(error.into()),
        _ => Ok(code),
    }
}

fn restore_dt(
    source: &Path,
    output: &Path,
    memory: &crate::service::memory::Context,
    without_users: bool,
    err: &mut dyn Write,
) -> Result<(infobase::restore::RestoreReport, f64)> {
    use infobase::restore::{
        Phase, RestoreEvent, RestoreLimits, RestoreOptions, restore_dt_new_file_with,
    };
    let (budget, mode, ledger) = memory.restore_budget();
    let limits = RestoreLimits::from_budget(budget);
    writeln!(
        err,
        "yellowcode: memory mode={mode} B={budget} cache={} dirty={} row={} schema-nodes={}",
        limits.cache_bytes, limits.max_dirty_bytes, limits.row_bytes, limits.schema_nodes
    )?;
    let started = std::time::Instant::now();
    let mut last: Option<(Phase, std::time::Instant)> = None;
    let mut write_error = None;
    let mut sink = |event: &RestoreEvent| {
        let line = match event {
            RestoreEvent::Note(note) => format!("yellowcode: restore: {note}"),
            RestoreEvent::Progress(p) => {
                let now = std::time::Instant::now();
                if let Some((phase, at)) = last
                    && phase == p.phase
                    && now.duration_since(at) < std::time::Duration::from_secs(5)
                {
                    return;
                }
                last = Some((p.phase, now));
                let phase = match p.phase {
                    Phase::Read => "read",
                    Phase::Metadata => "metadata",
                    Phase::Sync => "sync",
                    Phase::Verify => "verify",
                    Phase::Publish => "publish",
                };
                let percent = (p.source_read * 100).checked_div(p.source_len).unwrap_or(0);
                let secs = p.elapsed.as_secs();
                format!(
                    "yellowcode: restore: {phase} {percent}% ({}/{} MB of DT), table {}, {} rows, {} files, {:.1} GB written, {}m{:02}s",
                    p.source_read / 1_000_000,
                    p.source_len / 1_000_000,
                    if p.table.is_empty() { "-" } else { &p.table },
                    p.rows,
                    p.files,
                    p.written as f64 / 1e9,
                    secs / 60,
                    secs % 60
                )
            }
        };
        if write_error.is_none()
            && let Err(e) = writeln!(err, "{line}")
        {
            write_error = Some(e);
        }
    };
    let mut opts = RestoreOptions {
        limits,
        memory_budget: Some(ledger.clone()),
        without_users,
        events: Some(&mut sink),
    };
    let report = restore_dt_new_file_with(source, output, &mut opts);
    drop(opts);
    let report = report?;
    let peak = ledger.snapshot().peak;
    let _ = writeln!(
        err,
        "yellowcode: memory peak ledger={peak} batch={}",
        report.peak_pending_bytes
    );
    if report.users_dropped > 0 {
        let _ = writeln!(
            err,
            "yellowcode: restore: WARNING: {} users not transferred (--without-users), nor the legacy user list \
             Params/users.usr. The base has no users: \
             anyone with the file opens it without login and with full rights, and users of the БСП \
             (SSL) catalog lose their infobase accounts",
            report.users_dropped
        );
    }
    Ok((report, started.elapsed().as_secs_f64()))
}
