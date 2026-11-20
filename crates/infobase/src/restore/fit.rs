use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use brace::binary::ReadLimits;
use dtfile::{DtReader, Event, Source, TableHead};
use ibschema::columns::ColType;
use ibschema::logical::Table;
use ibschema::values::ColumnValue;
use onecd::format::layout::value_size;
use onecd::{BlobVal, Database, FormatInfo, RecordLayout, Row, TableDescr, Value};

use super::{
    Batches, FILES_MAX, FOLDERS, Notes, PAGE, Phase, Plan, Progress, RestoreEvent, RestoreLimits,
    RestoreOptions, TABLES_MAX, bad, check_row, counted_file, database_options, discard_file,
    expect, extensions, folder_file_name, folder_header, folder_header_limits, folder_length,
    io as io_err, open_f3, probe_locale, read_preamble, read_schema, row_limit,
};
use crate::Result;
use crate::physical::{NameCase, descrs};

pub(super) const MIN_SAMPLE: u64 = 1_000;
const PROGRESS_ROWS: u64 = 65_536;
const PLAN_PREFIX: &str = ".yellowcode-fit-";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FitOptions {
    pub cap: Option<u64>,
    pub window: u64,
    #[doc(hidden)]
    pub index_cap: Option<u64>,
}

impl Default for FitOptions {
    fn default() -> Self {
        FitOptions {
            cap: None,
            window: 200_000,
            index_cap: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Full,
    ClusteredOnly { dropped: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TablePlan {
    pub table: String,
    pub rows: u64,
    pub data_bytes: u64,
    pub blob_bytes: u64,
    pub index_estimate: u64,
    pub kept_estimate: u64,
    pub kept: Vec<String>,
    pub decision: Decision,
    pub cap_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReducedIndexes {
    pub table: String,
    pub rows: u64,
    pub kept: Vec<String>,
    pub dropped: Vec<String>,
    pub estimate_bytes: u64,
    pub kept_estimate_bytes: u64,
    pub cap_bytes: u64,
    pub clustered_kept: bool,
    pub object: Option<String>,
    pub service: Option<String>,
}

impl ReducedIndexes {
    pub(crate) fn of(plans: &[TablePlan]) -> Vec<ReducedIndexes> {
        plans
            .iter()
            .filter_map(|p| match &p.decision {
                Decision::Full => None,
                Decision::ClusteredOnly { dropped } => {
                    let upper = p.table.to_ascii_uppercase();
                    Some(ReducedIndexes {
                        table: p.table.clone(),
                        rows: p.rows,
                        kept: p.kept.clone(),
                        dropped: dropped.clone(),
                        estimate_bytes: p.index_estimate,
                        kept_estimate_bytes: p.kept_estimate,
                        cap_bytes: p.cap_bytes,
                        clustered_kept: p.kept.iter().any(|k| named_index(&upper, k).is_some()),
                        object: None,
                        service: None,
                    })
                }
            })
            .collect()
    }
}

pub(crate) fn loaded_estimate(p: &TablePlan) -> u64 {
    match p.decision {
        Decision::Full => p.index_estimate,
        Decision::ClusteredOnly { .. } => p.kept_estimate,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TablePlanInput {
    pub table: String,
    pub rows: u64,
    pub data_bytes: u64,
    pub blob_bytes: u64,
    pub index_estimate: u64,
    pub kept_estimate: u64,
    pub all: Vec<String>,
    pub kept: Vec<String>,
}

pub(super) fn gib(n: u64) -> String {
    format!("{:.1}", n as f64 / (1u64 << 30) as f64)
}

pub(crate) fn decide_caps(
    p: &TablePlanInput,
    cap: u64,
    index_cap: u64,
) -> std::result::Result<Decision, String> {
    let head = format!("table {}: {} rows", p.table, p.rows);
    let tail = format!(
        "does not fit a file base (internal file limit {} GiB); use a client-server base",
        gib(cap)
    );
    if p.data_bytes > cap {
        return Err(format!("{head}, data {} GiB {tail}", gib(p.data_bytes)));
    }
    if p.blob_bytes > cap {
        return Err(format!("{head}, blobs {} GiB {tail}", gib(p.blob_bytes)));
    }
    if p.index_estimate <= index_cap {
        return Ok(Decision::Full);
    }
    if p.kept.is_empty() {
        return Err(format!(
            "{head}, indexes estimated {} GiB, no clustered or implicit index to keep: {tail}",
            gib(p.index_estimate)
        ));
    }
    let dropped: Vec<String> = p
        .all
        .iter()
        .filter(|n| !p.kept.contains(n))
        .cloned()
        .collect();
    if p.kept_estimate <= index_cap && !dropped.is_empty() {
        return Ok(Decision::ClusteredOnly { dropped });
    }
    Err(format!(
        "{head}, indexes estimated {} GiB, clustered index {} GiB: {tail}",
        gib(p.index_estimate),
        gib(p.kept_estimate)
    ))
}

pub(crate) fn blob_len(value_len: u64) -> u64 {
    value_len.div_ceil(250) * 256
}

pub(crate) fn data_len(layout: &RecordLayout, rows: u64) -> u64 {
    u64::from(layout.record_len) * (rows + 1)
}

pub(crate) fn worst_index_bytes_per_row(d: &TableDescr, indexes: &[String]) -> Result<u64> {
    let mut total = 0u64;
    for n in indexes {
        let i = d
            .indexes
            .iter()
            .find(|i| i.name == *n)
            .ok_or_else(|| bad(format!("table {}: no index {n}", d.name)))?;
        let mut key = 0u64;
        for f in &i.fields {
            let field = d
                .fields
                .iter()
                .find(|x| x.name == f.name)
                .or_else(|| {
                    d.fields
                        .iter()
                        .find(|x| x.name.eq_ignore_ascii_case(&f.name))
                })
                .ok_or_else(|| {
                    bad(format!(
                        "table {}: index {n} has no field {}",
                        d.name, f.name
                    ))
                })?;
            key += u64::from(value_size(field));
        }
        total += (key + 8) * 2;
    }
    Ok(total)
}

pub(crate) fn scale(per_row: f64, rows: u64) -> u64 {
    (per_row * rows as f64 * 11.0 / 10.0).ceil() as u64
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Sample {
    pub rows: u64,
    pub full_per_row: f64,
    pub kept_per_row: f64,
}

fn row_estimate(row: &Row, indexes: usize) -> usize {
    row.iter()
        .map(|v| match v {
            Value::Char(s) | Value::VarChar(s) => s.len().saturating_mul(2),
            Value::Binary(b) | Value::Invalid(b) => b.len(),
            Value::Text(BlobVal::Data(b))
            | Value::NText(BlobVal::Data(b))
            | Value::Image(BlobVal::Data(b)) => b.len(),
            _ => 16,
        })
        .fold(0usize, usize::saturating_add)
        .saturating_add(indexes.saturating_add(2).saturating_mul(4 * PAGE))
}

fn without_blobs(mut row: Row) -> Row {
    for v in &mut row {
        if let Value::Text(b) | Value::NText(b) | Value::Image(b) = v {
            *b = BlobVal::Data(Vec::new());
        }
    }
    row
}

fn create_scratch(
    path: &Path,
    d: &TableDescr,
    locale: &str,
    limits: &RestoreLimits,
) -> Result<Database> {
    let db = Database::create(
        path,
        onecd::CreateOptions {
            format: onecd::Format::V838,
            page_size: PAGE as u32,
            locale: locale.into(),
            open: database_options(limits),
        },
    )?;
    {
        let mut tx = db.write()?;
        tx.create_table(d)?;
        tx.commit()?;
    }
    Ok(db)
}

self_cell::self_cell!(
    struct Scratch {
        owner: Database,
        #[not_covariant]
        dependent: Batches,
    }
);

pub(crate) struct Sampler {
    table: String,
    indexes: [usize; 3],
    scratch: Option<tempfile::TempDir>,
    paths: Vec<PathBuf>,
    dbs: Vec<Scratch>,
    rows: u64,
}

impl Sampler {
    pub(crate) fn new(
        dir: &Path,
        d: &TableDescr,
        kept: &[String],
        locale: &str,
        limits: &RestoreLimits,
    ) -> Result<Sampler> {
        let descrs = [reduce(d, &[]), d.clone(), reduce(d, kept)];
        let scratch = io_err(tempfile::Builder::new().prefix(".fit-").tempdir_in(dir))?;
        let base = scratch.path().to_path_buf();
        let mut s = Sampler {
            table: d.name.clone(),
            indexes: descrs.each_ref().map(|d| d.indexes.len()),
            scratch: Some(scratch),
            paths: Vec::with_capacity(3),
            dbs: Vec::with_capacity(3),
            rows: 0,
        };
        let limits = RestoreLimits {
            cache_bytes: limits.cache_bytes / 3,
            max_dirty_bytes: limits.max_dirty_bytes / 3,
            ..*limits
        };
        for (name, descr) in ["none", "full", "kept"].iter().zip(&descrs) {
            let path = base.join(format!("{name}.1CD"));
            let db = create_scratch(&path, descr, locale, &limits)?;
            s.paths.push(path);
            s.dbs.push(Scratch::new(db, |db| Batches::new(db, &limits)));
        }
        Ok(s)
    }

    pub(crate) fn push(&mut self, row: Row) -> Result<()> {
        let row = without_blobs(row);
        for (db, &indexes) in self.dbs.iter_mut().zip(&self.indexes) {
            let table = &self.table;
            db.with_dependent_mut(|_, batches| -> Result<()> {
                let tx = batches.unit(row_estimate(&row, indexes), "a sample row")?;
                tx.table_for_restore(table)?.insert(&row)?;
                Ok(())
            })?;
        }
        self.rows += 1;
        Ok(())
    }

    pub(crate) fn finish(mut self) -> Result<Sample> {
        for db in &mut self.dbs {
            db.with_dependent_mut(|_, batches| batches.commit())?;
        }
        self.dbs.clear();
        let mut len = [0u64; 3];
        for (l, path) in len.iter_mut().zip(&self.paths) {
            *l = io_err(fs::metadata(path))?.len();
        }
        let _ = self.remove();
        if self.rows == 0 {
            return Err(bad(format!("table {}: empty sample", self.table)));
        }
        let [none, full, kept] = len;
        let per_row = |l: u64| l.saturating_sub(none) as f64 / self.rows as f64;
        Ok(Sample {
            rows: self.rows,
            full_per_row: per_row(full),
            kept_per_row: per_row(kept),
        })
    }

    fn remove(&mut self) -> io::Result<()> {
        for path in self.paths.drain(..) {
            match fs::remove_file(&path) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        match self.scratch.take() {
            Some(d) => d.close(),
            None => Ok(()),
        }
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        self.dbs.clear();
        let _ = self.remove();
    }
}

fn named_index(upper: &str, name: &str) -> Option<usize> {
    fn ordinal(table: &str, index: &str) -> Option<usize> {
        let rest = index.strip_prefix(table)?.strip_prefix('_')?;
        let digits = rest.trim_end_matches('@');
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        digits.parse().ok()
    }
    ordinal(upper, name).or_else(|| ordinal(upper.strip_suffix("X1")?, name.strip_suffix("X1")?))
}

pub(crate) fn kept_indexes(t: &Table, owner: Option<&Table>, d: &TableDescr) -> Vec<String> {
    let base = ibschema::columns::phys_table_name(t, owner).to_ascii_uppercase();
    let extension = d.name == format!("{base}X1");
    let upper = if extension {
        base
    } else {
        d.name.to_ascii_uppercase()
    };
    d.indexes
        .iter()
        .filter(|i| {
            match named_index(
                &upper,
                if extension {
                    i.name.strip_suffix("X1").unwrap_or(&i.name)
                } else {
                    &i.name
                },
            ) {
                Some(n) => n >= 1 && t.indexes.get(n - 1).is_some_and(|x| x.clustered),
                None => true,
            }
        })
        .map(|i| i.name.clone())
        .collect()
}

pub(crate) fn reduce(d: &TableDescr, kept: &[String]) -> TableDescr {
    let mut r = d.clone();
    r.indexes.retain(|i| kept.contains(&i.name));
    r
}

struct Counted<R> {
    inner: R,
    read: Arc<AtomicU64>,
}
impl<R: Read> Read for Counted<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

struct Ticks {
    started: Instant,
    source_len: u64,
    read: Arc<AtomicU64>,
    table: String,
    rows: u64,
    last: u64,
}
impl Ticks {
    fn send(&self, notes: &mut Notes<'_, '_>) {
        notes.send(RestoreEvent::Progress(Progress {
            phase: Phase::Fit,
            source_read: self.read.load(Ordering::Relaxed),
            source_len: self.source_len,
            table: self.table.clone(),
            rows: self.rows,
            files: 0,
            written: 0,
            elapsed: self.started.elapsed(),
        }));
    }
    fn tick(&mut self, notes: &mut Notes<'_, '_>) {
        if self.rows >= self.last + PROGRESS_ROWS {
            self.last = self.rows;
            self.send(notes);
        }
    }
}

pub(crate) fn prepass(
    source: &Path,
    staging: &Path,
    locale: &str,
    opts: &RestoreOptions<'_>,
    notes: &mut Notes<'_, '_>,
    extension_memory: &mut extensions::Memory,
) -> Result<Vec<TablePlan>> {
    prepass_inner(source, staging, locale, opts, notes, extension_memory)
        .map_err(|e| row_limit(e, &opts.limits))
}

fn prepass_inner(
    source: &Path,
    staging: &Path,
    locale: &str,
    opts: &RestoreOptions<'_>,
    notes: &mut Notes<'_, '_>,
    extension_memory: &mut extensions::Memory,
) -> Result<Vec<TablePlan>> {
    let limits = opts.limits;
    let file = io_err(File::open(source))?;
    let source_len = io_err(file.metadata())?.len();
    let read = Arc::new(AtomicU64::new(0));
    let input = Counted {
        inner: BufReader::with_capacity(1 << 20, file),
        read: read.clone(),
    };
    let (mut reader, row_limits) = open_f3(input, &limits)?;
    let mut ticks = Ticks {
        started: Instant::now(),
        source_len,
        read,
        table: String::new(),
        rows: 0,
        last: 0,
    };
    ticks.send(notes);

    let compat = read_preamble(&mut reader)?;
    let (next, cas) = skip_folders(
        &mut reader,
        &limits,
        row_limits,
        opts.without_users,
        FILES_MAX,
    )?;
    skip_users(&mut reader, next)?;

    let schema = read_schema(&mut reader, &limits, row_limits)?;
    let walk = schema.walk();
    if walk.len() > TABLES_MAX {
        return Err(bad("schema table limit"));
    }
    let heads = TableHead::database(&schema)?;
    let all = descrs(&schema, NameCase::Upper)?;
    let mut by_name: HashMap<String, usize> = HashMap::with_capacity(all.len());
    for (j, d) in all.iter().enumerate() {
        by_name.entry(d.name.to_ascii_uppercase()).or_insert(j);
    }
    let cap = opts.fit.cap.unwrap_or_else(|| {
        FormatInfo {
            format: onecd::Format::V838,
            page_size: PAGE as u32,
        }
        .max_ifile_len()
    });
    let index_cap = opts.fit.index_cap.unwrap_or(cap);
    let mut plans = Vec::with_capacity(heads.len());
    for head in heads {
        expect(reader.next_event()?, Event::TableStart(head.clone()))?;
        let j = by_name
            .get(&head.phys_name.to_ascii_uppercase())
            .copied()
            .ok_or_else(|| bad(format!("no description of table {}", head.phys_name)))?;
        let d = all.get(j).ok_or_else(|| bad("schema walk"))?;
        let (t, owner) = walk.get(j).copied().ok_or_else(|| bad("schema walk"))?;
        ticks.table = d.name.clone();
        ticks.send(notes);
        let plan = table_plan(
            &mut reader,
            &head,
            d,
            t,
            owner,
            (cap, index_cap),
            staging,
            locale,
            opts,
            &mut ticks,
            notes,
        )?;
        plans.push(plan);
    }
    drop(walk);
    drop(all);
    drop(by_name);
    match reader.next_event()? {
        Some(Event::DataBaseEnd) => {
            if reader.next_event()?.is_some() {
                return Err(bad("element after DataBase"));
            }
        }
        Some(Event::ExtensionsStart) => {
            extensions::profile(compat, cas, &plans)?;
            let block = extensions::Block::read(
                &mut reader,
                &schema,
                &limits,
                row_limits,
                extension_memory,
                opts.memory_budget.as_ref(),
            )?;
            let walk = block.schema.walk();
            for ((head, descriptor), (t, owner)) in
                block.heads.iter().zip(&block.descriptors).zip(walk)
            {
                expect(reader.next_event()?, Event::TableStart(head.clone()))?;
                ticks.table.clone_from(&descriptor.name);
                ticks.send(notes);
                plans.push(table_plan(
                    &mut reader,
                    head,
                    descriptor,
                    t,
                    owner,
                    (cap, index_cap),
                    staging,
                    locale,
                    opts,
                    &mut ticks,
                    notes,
                )?);
            }
            let verified = block.finish(&mut reader, extension_memory)?;
            drop(reader);
            drop(verified);
            drop(schema);
            extension_memory.retain_plans(&plans)?;
            return Ok(plans);
        }
        _ => return Err(bad("element order, layout or value")),
    }
    Ok(plans)
}

pub(super) fn skip_folders<R: Read>(
    reader: &mut DtReader<R>,
    limits: &RestoreLimits,
    row_limits: ReadLimits,
    without_users: bool,
    file_limit: u64,
) -> Result<(Option<Event>, [u64; 2])> {
    let (_, header_limits) = folder_header_limits(limits, row_limits);
    let mut files = 0u64;
    let mut cas = [0u64; 2];
    let mut next = reader.next_event()?;
    for folder_name in FOLDERS {
        let head = TableHead::system(&Source::Folder(folder_name.into()))
            .ok_or_else(|| bad("system head"))?;
        let expected = Event::TableStart(head.clone());
        if folder_name == "DepotFiles" && next.as_ref() != Some(&expected) {
            continue;
        }
        expect(next.take(), expected)?;
        reader.set_read_limits(header_limits);
        while let Some(metadata) = folder_header(reader, &head)? {
            match folder_name {
                "ConfigCAS" => cas[0] += 1,
                "ConfigCASSave" => cas[1] += 1,
                _ => {}
            }
            let name = folder_file_name(&metadata)?;
            if counted_file(without_users, &head.phys_name, name) {
                files += 1;
                if files > file_limit {
                    return Err(bad("folder file limit"));
                }
            }
            discard_file(reader, folder_length(&metadata)?)?;
        }
        reader.set_read_limits(row_limits);
        next = reader.next_event()?;
    }
    Ok((next, cas))
}

fn skip_users<R: Read>(reader: &mut DtReader<R>, next: Option<Event>) -> Result<()> {
    let users = match next {
        Some(Event::TableStart(h)) if h.source == Source::Users || h.source == Source::UsersSpr => {
            h
        }
        _ => return Err(bad("element order, layout or value")),
    };
    if Some(&users) != TableHead::system(&users.source).as_ref() {
        return Err(bad("element order, layout or value"));
    }
    skip_rows(reader, &users)?;
    for source in [Source::PasswordPolicies, Source::CompromisedPasswords] {
        let head = TableHead::system(&source).ok_or_else(|| bad("system head"))?;
        expect(reader.next_event()?, Event::TableStart(head.clone()))?;
        skip_rows(reader, &head)?;
    }
    Ok(())
}

fn skip_rows<R: Read>(reader: &mut DtReader<R>, head: &TableHead) -> Result<()> {
    loop {
        match reader.next_event()? {
            Some(Event::TableEnd) => return Ok(()),
            Some(Event::Row(row)) => check_row(head, &row)?,
            _ => return Err(bad("missing system table end")),
        }
    }
}

fn blob_value_len(v: &ColumnValue, ty: ColType) -> u64 {
    match (v, ty) {
        (ColumnValue::Bytes(b), ColType::I) => b.len() as u64,
        (ColumnValue::Str(s), ColType::NT) => s.encode_utf16().count() as u64 * 2,
        _ => 0,
    }
}

#[allow(clippy::too_many_arguments)]
fn table_plan<R: Read>(
    reader: &mut DtReader<R>,
    head: &TableHead,
    d: &TableDescr,
    t: &Table,
    owner: Option<&Table>,
    (cap, index_cap): (u64, u64),
    staging: &Path,
    locale: &str,
    opts: &RestoreOptions<'_>,
    ticks: &mut Ticks,
    notes: &mut Notes<'_, '_>,
) -> Result<TablePlan> {
    let plan = Plan::new(d, head)?;
    let layout = RecordLayout::new(d)?;
    let names: Vec<String> = d.indexes.iter().map(|i| i.name.clone()).collect();
    let kept = kept_indexes(t, owner, d);
    let worst = worst_index_bytes_per_row(d, &names)?;
    let worst_kept = worst_index_bytes_per_row(d, &kept)?;
    let blob_columns: Vec<(usize, ColType)> = plan
        .fields
        .iter()
        .flatten()
        .filter(|(_, ty)| matches!(ty, ColType::I | ColType::NT))
        .copied()
        .collect();
    let window = opts.fit.window.max(1);
    let mut rows = 0u64;
    let mut blob_bytes = 0u64;
    let mut sampler: Option<Sampler> = None;
    let mut sample: Option<Sample> = None;
    let mut started = false;
    loop {
        let row = match reader.next_event()? {
            Some(Event::TableEnd) => break,
            Some(Event::Row(row)) => row,
            _ => return Err(bad("missing logical table end")),
        };
        extensions::check_data_head(head)?;
        check_row(head, &row)?;
        for &(i, ty) in &blob_columns {
            if let Some(v) = row.get(i) {
                blob_bytes = blob_bytes.saturating_add(blob_len(blob_value_len(v, ty)));
            }
        }
        if !started && rows.saturating_mul(worst) > index_cap / 2 {
            started = true;
            sampler = Some(Sampler::new(staging, d, &kept, locale, &opts.limits)?);
        }
        if let Some(s) = sampler.as_mut() {
            s.push(plan.row(row)?)?;
            if s.rows >= window {
                sample = sampler.take().map(Sampler::finish).transpose()?;
            }
        }
        rows += 1;
        ticks.rows += 1;
        ticks.tick(notes);
    }
    if let Some(s) = sampler.take() {
        if s.rows >= MIN_SAMPLE {
            sample = Some(s.finish()?);
        }
    }
    let (index_estimate, kept_estimate) = match sample {
        Some(s) => (scale(s.full_per_row, rows), scale(s.kept_per_row, rows)),
        None => (rows.saturating_mul(worst), rows.saturating_mul(worst_kept)),
    };
    let input = TablePlanInput {
        table: d.name.clone(),
        rows,
        data_bytes: data_len(&layout, rows),
        blob_bytes,
        index_estimate,
        kept_estimate,
        all: names,
        kept,
    };
    let decision = decide_caps(&input, cap, index_cap).map_err(bad)?;
    Ok(TablePlan {
        table: input.table,
        rows,
        data_bytes: input.data_bytes,
        blob_bytes,
        index_estimate,
        kept_estimate,
        kept: input.kept,
        decision,
        cap_bytes: index_cap,
    })
}

pub fn plan_dt(source: &Path, opts: &mut RestoreOptions<'_>) -> Result<Vec<TablePlan>> {
    let scratch = io_err(tempfile::Builder::new().prefix(PLAN_PREFIX).tempdir())?;
    let mut extension_memory = extensions::Memory::new(&opts.limits, opts.memory_budget.as_ref())?;
    let mut events = opts.events.take();
    let mut notes = Notes {
        events: &mut events,
    };
    let result = probe_locale(source, &opts.limits, opts.without_users, FILES_MAX)
        .map_err(|e| row_limit(e, &opts.limits))
        .and_then(|locale| {
            prepass(
                source,
                scratch.path(),
                locale,
                opts,
                &mut notes,
                &mut extension_memory,
            )
        });
    opts.events = events;
    let closed = scratch.close();
    let plans = result?;
    io_err(closed)?;
    Ok(plans)
}
