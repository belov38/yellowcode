use std::collections::HashMap;
use std::path::{Path, PathBuf};

use bsl_compiler::{
    CompatMode, CompileOptions, ExecContext, Lang as CLang, ModuleKind as CKind, compile,
};
use bsl_syntax::SourceText;
use bsl_testhost::{ConfigScan, ModuleFile};
use mdconf::xml_out::{DumpOptions, Part, paths};
use mdconf::{Configuration, MdObject, Module, ModuleKind};
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::names::{kind_name, module_name};
use super::out::Lang;

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct Diag {
    pub module: String,
    pub line: u32,
    pub col: u32,
    pub context: String,
    pub text: String,
}

pub struct Item<'a> {
    pub name: String,
    pub rel: String,
    pub module: &'a Module,
}

#[derive(Serialize, Debug, Default)]
pub struct Report {
    pub modules: usize,
    pub compiles: usize,
    pub skipped: usize,
    #[serde(rename = "errors")]
    pub diags: Vec<Diag>,
    #[serde(skip)]
    pub warnings: Vec<String>,
}

#[derive(Clone, Copy, Debug)]
pub struct Run {
    pub ctxs: Ctx,
    pub own_contexts: bool,
    pub lang: Lang,
}

/// Execution contexts to compile in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
pub enum Ctx {
    Server,
    ThinClient,
    WebClient,
    All,
}

impl Ctx {
    fn list(self) -> &'static [ExecContext] {
        match self {
            Ctx::Server => &[ExecContext::Server],
            Ctx::ThinClient => &[ExecContext::ThinClient],
            Ctx::WebClient => &[ExecContext::WebClient],
            Ctx::All => &ALL,
        }
    }
}

const ALL: [ExecContext; 3] = [
    ExecContext::Server,
    ExecContext::ThinClient,
    ExecContext::WebClient,
];

fn context_name(c: ExecContext) -> &'static str {
    match c {
        ExecContext::Server => "Server",
        ExecContext::ThinClient => "ThinClient",
        ExecContext::WebClient => "WebClient",
        _ => "Other",
    }
}

fn file_of(kind: ModuleKind) -> &'static str {
    match kind {
        ModuleKind::Module => "Module.bsl",
        ModuleKind::ObjectModule => "ObjectModule.bsl",
        ModuleKind::ManagerModule => "ManagerModule.bsl",
        ModuleKind::RecordSetModule => "RecordSetModule.bsl",
        ModuleKind::ValueManagerModule => "ValueManagerModule.bsl",
        ModuleKind::CommandModule => "CommandModule.bsl",
        ModuleKind::FormModule => "Form/Module.bsl",
        ModuleKind::ManagedApplicationModule => "ManagedApplicationModule.bsl",
        ModuleKind::OrdinaryApplicationModule => "OrdinaryApplicationModule.bsl",
        ModuleKind::SessionModule => "SessionModule.bsl",
        ModuleKind::ExternalConnectionModule => "ExternalConnectionModule.bsl",
    }
}

pub fn items(cfg: &Configuration) -> Vec<Item<'_>> {
    let mut out = Vec::new();
    for m in &cfg.modules {
        out.push(Item {
            name: module_name(m.kind, Lang::Ru),
            rel: format!("Ext/{}", file_of(m.kind)),
            module: m,
        });
    }
    for o in &cfg.objects {
        object_items(cfg, o, &mut out);
    }
    out
}

fn object_items<'a>(cfg: &'a Configuration, o: &'a MdObject, out: &mut Vec<Item<'a>>) {
    let dir = paths::object_dir(cfg, o);
    let kind = kind_name(o.kind, Lang::Ru);
    let base = format!("{kind}.{}", o.name);
    for m in &o.modules {
        out.push(Item {
            name: format!("{base}.{}", module_name(m.kind, Lang::Ru)),
            rel: format!("{dir}/Ext/{}", file_of(m.kind)),
            module: m,
        });
    }
    for c in &o.children {
        if let Some(m) = &c.module {
            out.push(Item {
                name: format!("{base}.Команда.{}.МодульКоманды", c.name),
                rel: format!("{dir}/Commands/{}/Ext/{}", c.name, file_of(m.kind)),
                module: m,
            });
        }
    }
    for s in &o.forms {
        if let Some(m) = &s.module {
            out.push(Item {
                name: format!("{base}.Форма.{}.Форма", s.name),
                rel: format!("{}/Ext/Form/Module.bsl", paths::sub_dir(cfg, o, s)),
                module: m,
            });
        }
    }
    for r in &o.recalculations {
        if let Some(m) = &r.module {
            out.push(Item {
                name: format!("{base}.Перерасчет.{}.МодульНабораЗаписей", r.name),
                rel: format!("{}/Ext/RecordSetModule.bsl", paths::recalc_dir(cfg, o, r)),
                module: m,
            });
        }
    }
}

pub fn compat_of(code: u32) -> CompatMode {
    let (a, b, c) = (code / 10000, (code / 100) % 100, code % 100);
    match (u8::try_from(a), u8::try_from(b), u8::try_from(c)) {
        (Ok(8), Ok(3), Ok(c)) if c >= 10 => CompatMode::Version(8, 3, c),
        _ => CompatMode::DontUse,
    }
}

fn has_context(scan: &ConfigScan, m: &ModuleFile, ctx: ExecContext) -> bool {
    match ctx {
        ExecContext::Server => scan.server_contexts(m),
        ExecContext::ThinClient | ExecContext::WebClient => match m.kind {
            CKind::Common(f) => f.client_managed,
            CKind::Form | CKind::Command | CKind::ManagedApplication => true,
            _ => false,
        },
        _ => false,
    }
}

fn fallback_kind(k: ModuleKind) -> Option<CKind> {
    Some(match k {
        ModuleKind::Module => return None,
        ModuleKind::ObjectModule => CKind::Object,
        ModuleKind::ManagerModule => CKind::Manager,
        ModuleKind::RecordSetModule => CKind::RecordSet,
        ModuleKind::ValueManagerModule => CKind::ValueManager,
        ModuleKind::CommandModule => CKind::Command,
        ModuleKind::FormModule => CKind::Form,
        ModuleKind::ManagedApplicationModule => CKind::ManagedApplication,
        ModuleKind::OrdinaryApplicationModule => CKind::OrdinaryApplication,
        ModuleKind::SessionModule => CKind::Session,
        ModuleKind::ExternalConnectionModule => CKind::ExternalConnection,
    })
}

fn compose(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let prev = out.chars().next_back();
        let composed = match (prev, c) {
            (Some('и'), '\u{306}') => Some('й'),
            (Some('И'), '\u{306}') => Some('Й'),
            (Some('е'), '\u{308}') => Some('ё'),
            (Some('Е'), '\u{308}') => Some('Ё'),
            _ => None,
        };
        if let Some(x) = composed {
            out.pop();
            out.push(x);
        } else {
            out.push(c);
        }
    }
    out
}

fn rel_key(root: &Path, p: &Path) -> Option<String> {
    let rel = p.strip_prefix(root).ok()?;
    let parts: Option<Vec<String>> = rel
        .iter()
        .map(|c| c.to_str().map(|s| compose(s).to_lowercase()))
        .collect();
    Some(parts?.join("/"))
}

fn lang_of(l: Lang) -> CLang {
    match l {
        Lang::Ru => CLang::Ru,
        Lang::En => CLang::En,
    }
}

pub fn check_items(
    compat: CompatMode,
    names_dir: &Path,
    items: &[Item<'_>],
    run: Run,
) -> crate::Result<Report> {
    let Run {
        ctxs,
        own_contexts,
        lang,
    } = run;
    let scan = ConfigScan::load(names_dir).map_err(|e| e.to_string())?;
    let by_rel: HashMap<String, &ModuleFile> = scan
        .modules()
        .iter()
        .filter_map(|m| Some((rel_key(names_dir, &m.path)?, m)))
        .collect();
    let catalog = platform_catalog::catalog();
    let mut rep = Report::default();
    for it in items {
        let key = compose(&it.rel).to_lowercase();
        let fallback;
        let mf: &ModuleFile = match by_rel.get(&key) {
            Some(m) => m,
            None => match fallback_kind(it.module.kind) {
                Some(kind) => {
                    fallback = ModuleFile {
                        path: PathBuf::from(&it.rel),
                        name: it.name.clone(),
                        kind,
                        owner: None,
                    };
                    &fallback
                }
                None => {
                    rep.skipped += 1;
                    continue;
                }
            },
        };
        let applicable = |list: &[ExecContext]| -> Vec<ExecContext> {
            list.iter()
                .copied()
                .filter(|&c| has_context(&scan, mf, c))
                .collect()
        };
        let mut contexts = applicable(ctxs.list());
        if contexts.is_empty() && own_contexts {
            contexts = applicable(&ALL);
            let names: Vec<&str> = contexts.iter().map(|&c| context_name(c)).collect();
            if names.is_empty() {
                rep.warnings.push(format!(
                    "{}: the module has no execution context, nothing to compile",
                    it.name
                ));
            } else {
                rep.warnings.push(format!(
                    "{}: the module has no part in the requested context, compiled in {}",
                    it.name,
                    names.join(", ")
                ));
            }
        }
        if contexts.is_empty() {
            continue;
        }
        rep.modules += 1;
        let src = SourceText::from_utf8(mf.name.as_str(), &it.module.source);
        let first = rep.diags.len();
        for ctx in contexts {
            rep.compiles += 1;
            let o = CompileOptions {
                module_name: &mf.name,
                kind: mf.kind,
                context: ctx,
                compat,
                catalog,
                app: &scan,
                extension: None,
            };
            if let Err(e) = compile(&src, &o) {
                let text = e.cause_text(&mf.name, lang_of(lang));
                let same = rep.diags.get_mut(first..).and_then(|d| {
                    d.iter_mut()
                        .find(|d| d.line == e.line && d.col == e.col && d.text == text)
                });
                match same {
                    Some(d) => {
                        d.context.push(',');
                        d.context.push_str(context_name(ctx));
                    }
                    None => rep.diags.push(Diag {
                        module: mf.name.clone(),
                        line: e.line,
                        col: e.col,
                        context: context_name(ctx).to_string(),
                        text,
                    }),
                }
            }
        }
    }
    Ok(rep)
}

fn cache_dir() -> crate::Result<PathBuf> {
    let var = |n: &str| std::env::var_os(n).filter(|d| !d.is_empty());
    if let Some(d) = var("ONEC_CFG_CACHE") {
        return Ok(PathBuf::from(d));
    }
    if let Some(d) = var("XDG_CACHE_HOME") {
        return Ok(PathBuf::from(d).join("onec/cfg"));
    }
    let home = var("HOME").ok_or("cannot find the cache directory: HOME is not set")?;
    Ok(PathBuf::from(home).join(".cache/onec/cfg"))
}

fn sha256_of(p: &Path) -> crate::Result<String> {
    let mut f = std::fs::File::open(p).map_err(|e| format!("{}: {e}", p.display()))?;
    let mut h = Sha256::new();
    std::io::copy(&mut f, &mut h).map_err(|e| format!("{}: {e}", p.display()))?;
    Ok(format!("{:x}", h.finalize()))
}

const DONE: &str = ".onec-done";
const FORMS_WARNING: &str = "forms contexts are incomplete, give --names";

const DUMP_FORMAT: u32 = 2;

pub fn names_dir(
    cfg: &Configuration,
    source: &Path,
    names: Option<&Path>,
) -> crate::Result<(PathBuf, Vec<String>)> {
    match names {
        Some(n) => Ok((n.to_path_buf(), Vec::new())),
        None => cached_dump(&cache_dir()?, source, cfg),
    }
}

#[derive(Default)]
pub struct CheckCache {
    pub names: std::sync::Mutex<Option<(PathBuf, Vec<String>)>>,
    pub answers: std::sync::Mutex<HashMap<String, std::sync::Arc<serde_json::Value>>>,
    dir: Option<PathBuf>,
    key: std::sync::Mutex<Option<String>>,
    write: std::sync::Mutex<()>,
    writes: std::sync::atomic::AtomicUsize,
}

fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl CheckCache {
    pub fn with_dir(dir: PathBuf) -> CheckCache {
        CheckCache {
            dir: Some(dir),
            ..CheckCache::default()
        }
    }

    fn cache(&self) -> crate::Result<PathBuf> {
        match &self.dir {
            Some(d) => Ok(d.clone()),
            None => cache_dir(),
        }
    }

    fn key(&self, source: &Path) -> crate::Result<String> {
        let mut k = lock(&self.key);
        if let Some(k) = k.as_ref() {
            return Ok(k.clone());
        }
        let v = dump_key(source)?;
        *k = Some(v.clone());
        Ok(v)
    }

    pub fn known(
        &self,
        source: &Path,
        names: Option<&Path>,
    ) -> crate::Result<Option<(PathBuf, Vec<String>)>> {
        if let Some(n) = names {
            return Ok(Some((n.to_path_buf(), Vec::new())));
        }
        if let Some(v) = lock(&self.names).as_ref() {
            return Ok(Some(v.clone()));
        }
        let found = find_cached(&self.cache()?, &self.key(source)?)?;
        if let Some(v) = &found {
            *lock(&self.names) = Some(v.clone());
        }
        Ok(found)
    }

    pub fn names_dir(
        &self,
        cfg: &Configuration,
        source: &Path,
        names: Option<&Path>,
    ) -> crate::Result<(PathBuf, Vec<String>)> {
        if let Some(v) = self.known(source, names)? {
            return Ok(v);
        }
        let _one = lock(&self.write);
        if let Some(v) = self.known(source, names)? {
            return Ok(v);
        }
        self.writes
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let v = write_cached(&self.cache()?, &self.key(source)?, cfg)?;
        *lock(&self.names) = Some(v.clone());
        Ok(v)
    }
}

fn read_done(dir: &Path) -> Option<Vec<String>> {
    let s = std::fs::read_to_string(dir.join(DONE)).ok()?;
    Some(s.lines().map(String::from).collect())
}

fn incomplete(dir: &Path) -> crate::Error {
    format!(
        "{}: the cached dump is incomplete (an interrupted run?), remove this directory and retry",
        dir.display()
    )
    .into()
}

fn cached_dump(
    cache: &Path,
    source: &Path,
    cfg: &Configuration,
) -> crate::Result<(PathBuf, Vec<String>)> {
    write_cached(cache, &dump_key(source)?, cfg)
}

const DUMP_VERSION: &str = "0.1.0";

fn dump_key(source: &Path) -> crate::Result<String> {
    Ok(format!(
        "{}-f{DUMP_FORMAT}-v{DUMP_VERSION}",
        sha256_of(source)?
    ))
}

fn find_cached(cache: &Path, key: &str) -> crate::Result<Option<(PathBuf, Vec<String>)>> {
    find_cached_with_exists(cache, key, Path::exists)
}

fn find_cached_with_exists(
    cache: &Path,
    key: &str,
    exists: impl FnOnce(&Path) -> bool,
) -> crate::Result<Option<(PathBuf, Vec<String>)>> {
    let dir = cache.join(key);
    if !exists(&dir) {
        return Ok(None);
    }
    if let Some(w) = read_done(&dir) {
        return Ok(Some((dir, w)));
    }
    Err(incomplete(&dir))
}

fn write_cached(
    cache: &Path,
    key: &str,
    cfg: &Configuration,
) -> crate::Result<(PathBuf, Vec<String>)> {
    if let Some(found) = find_cached(cache, key)? {
        return Ok(found);
    }
    let dir = cache.join(key);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let tmp = cache.join(format!("{key}.tmp-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let warnings = match write_dump(cfg, &tmp) {
        Ok(w) => w,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(e);
        }
    };
    match std::fs::rename(&tmp, &dir) {
        Ok(()) => Ok((dir, warnings)),
        Err(e) => {
            let _ = std::fs::remove_dir_all(&tmp);
            match read_done(&dir) {
                Some(w) => Ok((dir, w)),
                None if dir.exists() => Err(incomplete(&dir)),
                None => Err(format!("{}: {e}", dir.display()).into()),
            }
        }
    }
}

fn write_dump(cfg: &Configuration, dir: &Path) -> crate::Result<Vec<String>> {
    let opts = DumpOptions::only(&[
        Part::Configuration,
        Part::Objects,
        Part::Modules,
        Part::Predefined,
        Part::Forms,
    ]);
    let r = mdconf::xml_out::dump(cfg, dir, &opts)
        .map_err(|e| format!("cannot dump the names of the configuration: {e}"))?;
    let mut warnings = Vec::new();
    if r.not_written.contains(&Part::Forms) {
        warnings.push(FORMS_WARNING.to_string());
    }
    let marker = dir.join(DONE);
    std::fs::write(&marker, warnings.join("\n"))
        .map_err(|e| format!("{}: {e}", marker.display()))?;
    Ok(warnings)
}

pub fn check(
    cfg: &Configuration,
    source: &Path,
    items: &[Item<'_>],
    names: Option<&Path>,
    run: Run,
) -> crate::Result<Report> {
    let (dir, warnings) = names_dir(cfg, source, names)?;
    let compat = compat_of(cfg.settings().compatibility_mode.0);
    let mut rep = check_items(compat, &dir, items, run)?;
    let mut all = warnings;
    all.append(&mut rep.warnings);
    rep.warnings = all;
    Ok(rep)
}

pub fn render_text(rep: &Report, out: &mut dyn std::io::Write) -> crate::Result<()> {
    for d in &rep.diags {
        writeln!(out, "{}", d.text)?;
    }
    writeln!(
        out,
        "modules {}, compiles {}, skipped {}, errors {}",
        rep.modules,
        rep.compiles,
        rep.skipped,
        rep.diags.len()
    )?;
    Ok(())
}
