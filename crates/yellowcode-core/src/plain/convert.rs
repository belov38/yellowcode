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

pub mod external;
pub mod form_renderer;

#[derive(Clone, Copy)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
pub enum Format {
    Cf,
    Dt,
    #[cfg_attr(feature = "clap", value(name = "1cd"))]
    Infobase,
    Xml,
    Epf,
    Erf,
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

const RAW_CHUNK: usize = 1 << 20;

struct RawData<'a> {
    source: &'a dyn EntrySource,
    names: Vec<String>,
    lens: Vec<u64>,
    ranged: bool,
}
impl RawData<'_> {
    fn copy_ranges(&self, name: &str, len: u64, out: &mut dyn Write) -> Result<()> {
        let mut offset = 0u64;
        while offset < len {
            let chunk = self
                .source
                .stored_range_owned(name, offset, RAW_CHUNK)?
                .ok_or_else(|| format!("CONFIG entry disappeared: {name}"))?;
            if chunk.is_empty() {
                return Err(format!("CONFIG entry shorter than its size: {name}").into());
            }
            out.write_all(&chunk)?;
            offset += u64::try_from(chunk.len())?;
        }
        Ok(())
    }
}
impl v8cont::DataSource for RawData<'_> {
    fn write_data(&mut self, index: usize, out: &mut dyn Write) -> v8cont::Result<()> {
        let (Some(name), Some(&len)) = (self.names.get(index), self.lens.get(index)) else {
            return Err(v8cont::Error::LayoutMismatch("CONFIG entry index".into()));
        };
        if self.ranged {
            return self
                .copy_ranges(name, len, out)
                .map_err(|e| v8cont::Error::Io(std::io::Error::other(e)));
        }
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

fn raw_cf(
    source: &Path,
    kind: SourceKind,
    memory: Option<&source::OpenMemory>,
    out: &mut dyn Write,
) -> Result<()> {
    let ranged = kind == SourceKind::Dt;
    let consume = |entries: &dyn EntrySource| {
        let list = entries.list_owned()?;
        let mut specs = Vec::new();
        let mut lens = Vec::new();
        for meta in list.iter() {
            let missing = || format!("CONFIG missing entry: {}", meta.name);
            let len = if ranged {
                entries.stored_len(&meta.name)?.ok_or_else(missing)?
            } else {
                u64::try_from(entries.stored_owned(&meta.name)?.ok_or_else(missing)?.len())?
            };
            specs.push(v8cont::EntrySpec {
                name: meta.name.clone(),
                ctime: meta.ctime,
                mtime: meta.mtime,
                attrs: meta.attrs,
                data_len: Some(len),
            });
            lens.push(len);
        }
        if specs.is_empty() {
            return Err("CONFIG has no configuration entries".into());
        }
        let mut data = RawData {
            source: entries,
            names: specs.iter().map(|s| s.name.clone()).collect(),
            lens,
            ranged,
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

pub fn interrupted(error: &crate::Error) -> Option<process_containment::interrupt::Kind> {
    error
        .downcast_ref::<external::Interrupted>()
        .map(|error| error.kind)
}

fn display_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else {
        text.strip_prefix(r"\\?\").unwrap_or(&text).to_owned()
    }
}

fn external_limits(budget: &memory_budget::MemoryBudget) {
    budget.set_limits(memory_budget::Limits::new(budget.snapshot().admission));
}

fn run_external(args: &Request, out: &mut dyn Write) -> Result<i32> {
    external::validate(args)?;
    let (context, _reserve) =
        crate::service::memory::Settings::memory_only(args.memory_limit.clone()).resolve()?;
    let budget = context.ledger();
    external_limits(budget);
    let _control = budget.try_reserve(memory_budget::Category::Execution, 1 << 20)?;
    let _watch = process_containment::interrupt::watch()?;
    let executable = std::env::current_exe()?;
    let spill = tempfile::Builder::new()
        .prefix("yellowcode-external-")
        .tempdir()?;
    let spill_path = std::fs::canonicalize(spill.path())?;
    let discovery = crate::platform::DiscoverySnapshot::capture(budget)?;
    let (catalog, _catalog_owner) = crate::platform::catalog_with_memory(budget)?;
    let renderer = form_renderer::SupervisedFormRenderer {
        executable: &executable,
        spill_dir: &spill_path,
        platform: catalog.platform(),
        platform_resources: discovery.roots().override_root,
    };
    let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let bridge = external::InterruptBridge::new(cancelled.clone(), budget)?;
    let result = external::run(
        args,
        &external::Options {
            budget,
            cancelled,
            form_renderer: &renderer,
        },
    );
    drop(bridge);
    if let Err(source) = result {
        if let Some(kind) = process_containment::interrupt::requested() {
            return Err(Box::new(external::Interrupted { kind, source }));
        }
        return Err(source);
    }
    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Published {
        schema_version: u32,
        output: String,
        complete: bool,
    }
    let status = Published {
        schema_version: 1,
        output: display_path(&args.output),
        complete: true,
    };
    if let Err(error) = serde_json::to_writer(&mut *out, &status) {
        return if error.io_error_kind() == Some(std::io::ErrorKind::BrokenPipe) {
            Ok(0)
        } else {
            Err(error.into())
        };
    }
    match writeln!(out) {
        Err(error) if error.kind() != std::io::ErrorKind::BrokenPipe => Err(error.into()),
        _ => Ok(0),
    }
}

pub fn run(args: Request, out: &mut dyn Write, err: &mut dyn Write) -> Result<i32> {
    if matches!(args.to, Format::Xml | Format::Epf | Format::Erf) {
        return run_external(&args, out);
    }
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
        return Err("1CD to DT requires --users-layout users8|users-spr11".into());
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
            file(&args.output, |out| {
                raw_cf(&source, kind, opening.as_ref(), out)
            })?;
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
        Format::Xml | Format::Epf | Format::Erf => {
            return Err("unsupported conversion direction".into());
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
    if let Some((r, _)) = &report {
        for reduced in &r.reduced_indexes {
            let _ = writeln!(err, "{}", reduced_warning(reduced, true));
        }
    }
    if let (Some((r, seconds)), Some(map)) = (&report, status.as_object_mut()) {
        map.insert("rows".into(), r.rows.into());
        map.insert("files".into(), r.files.into());
        map.insert("tables".into(), r.tables.into());
        map.insert("usersDropped".into(), r.users_dropped.into());
        map.insert("sourceSha256".into(), r.source_sha256.clone().into());
        map.insert("seconds".into(), (*seconds).into());
        map.insert(
            "indexConstraints".into(),
            serde_json::json!({
                "duplicateEntries": r.index_constraints.duplicate_entries,
                "duplicateGroups": r.index_constraints.duplicate_groups,
                "indexesWithDuplicates": r.index_constraints.indexes_with_duplicates,
                "omittedIndexes": r.index_constraints.omitted_indexes,
            }),
        );
        map.insert("reducedIndexes".into(), reduced_json(&r.reduced_indexes));
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

fn reduced_warning(r: &infobase::restore::ReducedIndexes, done: bool) -> String {
    const GIB: f64 = (1u64 << 30) as f64;
    let kept = if r.clustered_kept {
        "с кластерным и служебными индексами"
    } else {
        "со служебными индексами"
    };
    let table = match reduced_object(r) {
        Some(o) => format!("{} ({o})", r.table),
        None => r.table.clone(),
    };
    let (loaded, dropped) = if done {
        ("загружена", "сняты")
    } else {
        ("будет загружена", "будут сняты")
    };
    let mut line = format!(
        "предупреждение: таблица {table}, {} строк: индексы не помещаются в файловую базу \
         (оценка {:.1} ГиБ при пределе {:.1} ГиБ); {loaded} только {kept} \
         (оценка {:.1} ГиБ), {dropped}: {}",
        r.rows,
        r.estimate_bytes as f64 / GIB,
        r.cap_bytes as f64 / GIB,
        r.kept_estimate_bytes as f64 / GIB,
        r.dropped.join(", ")
    );
    if done {
        line.push_str(
            ". Чтение и запись в 1С работают, но отборы и соединения по снятым индексам идут \
             просмотром всей таблицы и медленнее; -ReIndex в 1С снятые индексы не восстанавливает, \
             полный набор индексов даёт только клиент-серверная база",
        );
    }
    line
}

fn md_name_ru(md_name: &str) -> String {
    md_name
        .split('.')
        .enumerate()
        .map(|(i, part)| match (i, part) {
            (0, "Catalog") => "Справочник",
            (0, "Document") => "Документ",
            (0, "DocumentJournal") => "ЖурналДокументов",
            (0, "Enum") => "Перечисление",
            (0, "Constant") => "Константа",
            (0, "ChartOfCharacteristicTypes") => "ПланВидовХарактеристик",
            (0, "ChartOfAccounts") => "ПланСчетов",
            (0, "ChartOfCalculationTypes") => "ПланВидовРасчета",
            (0, "InformationRegister") => "РегистрСведений",
            (0, "AccumulationRegister") => "РегистрНакопления",
            (0, "AccountingRegister") => "РегистрБухгалтерии",
            (0, "CalculationRegister") => "РегистрРасчета",
            (0, "BusinessProcess") => "БизнесПроцесс",
            (0, "Task") => "Задача",
            (0, "ExchangePlan") => "ПланОбмена",
            (0, "Sequence") => "Последовательность",
            (i, "TabularSection") if i % 2 == 0 => "ТабличнаяЧасть",
            (i, "Recalculation") if i % 2 == 0 => "Перерасчет",
            (_, other) => other,
        })
        .collect::<Vec<_>>()
        .join(".")
}

fn reduced_object(r: &infobase::restore::ReducedIndexes) -> Option<String> {
    let name = md_name_ru(r.object.as_deref()?);
    Some(match &r.service {
        Some(purpose) => format!("{name}, служебная таблица {purpose}"),
        None => name,
    })
}

fn reduced_json(rs: &[infobase::restore::ReducedIndexes]) -> serde_json::Value {
    rs.iter()
        .map(|r| {
            serde_json::json!({
                "table": r.table,
                "object": reduced_object(r),
                "rows": r.rows,
                "kept": r.kept,
                "dropped": r.dropped,
                "estimateBytes": r.estimate_bytes,
                "keptEstimateBytes": r.kept_estimate_bytes,
                "capBytes": r.cap_bytes,
            })
        })
        .collect::<Vec<_>>()
        .into()
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
        "yellowcode: memory mode={mode} B={budget} cache={} dirty={} row={} schema-nodes={} metadata={}",
        limits.cache_bytes,
        limits.max_dirty_bytes,
        limits.row_bytes,
        limits.schema_nodes,
        limits.metadata_bytes
    )?;
    let started = std::time::Instant::now();
    let mut last: Option<(Phase, std::time::Instant)> = None;
    let mut write_error = None;
    let mut sink = |event: &RestoreEvent| {
        let line = match event {
            RestoreEvent::Note(note) => format!("yellowcode: restore: {note}"),
            RestoreEvent::Reduced(r) => {
                format!("yellowcode: restore: {}", reduced_warning(r, false))
            }
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
                    Phase::Fit => "fit",
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
        fit: Default::default(),
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
