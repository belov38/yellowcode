use crate::model::{AppModel, KINDS, ObjId, ObjKind};
use crate::names::AppNamesImpl;
use bsl_compiler::{CompileError, CompileOptions, CompiledModule, ModuleKind, compile};
use bsl_prim::{CompatMode, fold_name};
use bsl_syntax::{ExecContext, SourceText};
use bsl_vm::{Exception, Execution, ModuleContext, ModuleInstanceRef, ModuleRef};
use platform_catalog::messages::Lang;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

type Compiled = Result<Arc<CompiledModule>, CompileError>;
type Got = Result<Arc<CompiledModule>, ModuleError>;

#[derive(PartialEq, Eq, Hash)]
struct Key {
    text: [u8; 32],
    names: [u8; 32],
    module: String,
    kind: [u8; 10],
    context: ExecContext,
    compat: CompatMode,
}

fn kind_bytes(k: ModuleKind) -> [u8; 10] {
    let (tag, f) = match k {
        ModuleKind::Common(f) => (0, Some(f)),
        ModuleKind::Object => (1, None),
        ModuleKind::Manager => (2, None),
        ModuleKind::RecordSet => (3, None),
        ModuleKind::ValueManager => (4, None),
        ModuleKind::Command => (5, None),
        ModuleKind::Form => (6, None),
        ModuleKind::Session => (7, None),
        ModuleKind::ManagedApplication => (8, None),
        ModuleKind::OrdinaryApplication => (9, None),
        ModuleKind::ExternalConnection => (10, None),
        ModuleKind::HttpService => (11, None),
        ModuleKind::WebService => (12, None),
    };
    let f = f.unwrap_or_default();
    [
        tag,
        f.server as u8,
        f.client_managed as u8,
        f.client_ordinary as u8,
        f.external_connection as u8,
        f.server_call as u8,
        f.global as u8,
        f.privileged as u8,
        f.reuse as u8,
        0,
    ]
}

#[derive(Clone, Debug)]
pub enum ModuleError {
    InvalidRef(ModuleRef),
    Compile(CompileError),
}

#[derive(Default)]
pub struct CompileCache {
    map: Mutex<HashMap<Key, Compiled>>,
    runs: AtomicU64,
}

impl CompileCache {
    pub fn compilations(&self) -> u64 {
        self.runs.load(Ordering::Relaxed)
    }

    fn get_or_compile(&self, key: Key, run: impl FnOnce() -> Compiled) -> Compiled {
        let Ok(mut map) = self.map.lock() else {
            self.runs.fetch_add(1, Ordering::Relaxed);
            return run();
        };
        if let Some(r) = map.get(&key) {
            return r.clone();
        }
        self.runs.fetch_add(1, Ordering::Relaxed);
        let r = run();
        map.insert(key, r.clone());
        r
    }
}

struct Entry {
    at: usize,
    name: String,
    kind: ModuleKind,
    owner: Option<ObjId>,
    compiled: OnceLock<Compiled>,
}

pub struct Modules {
    names: Arc<AppNamesImpl>,
    cache: Arc<CompileCache>,
    compat: CompatMode,
    entries: Vec<Entry>,
    by_name: HashMap<String, u32>,
    managers_ok: [OnceLock<()>; KINDS.len()],
}

fn words() -> Vec<(String, String)> {
    let mut w: Vec<(String, String)> = KINDS
        .iter()
        .map(|k| (k.name(Lang::Ru).to_string(), k.name(Lang::En).to_string()))
        .collect();
    for (r, e) in [
        ("ОбщийМодуль", "CommonModule"),
        ("Модуль", "Module"),
        ("МодульОбъекта", "ObjectModule"),
        ("МодульМенеджера", "ManagerModule"),
        ("МодульНабораЗаписей", "RecordSetModule"),
        ("МодульМенеджераЗначения", "ValueManagerModule"),
        ("Команда", "Command"),
        ("МодульКоманды", "CommandModule"),
        ("МодульСеанса", "SessionModule"),
        ("HTTPСервис", "HTTPService"),
        ("WebСервис", "WebService"),
        ("ИнтеграционныйСервис", "IntegrationService"),
        ("ОбщаяКоманда", "CommonCommand"),
        ("ОбщаяФорма", "CommonForm"),
        ("Форма", "Form"),
        ("ХранилищеНастроек", "SettingsStorage"),
        ("КритерийОтбора", "FilterCriterion"),
    ] {
        w.push((r.to_string(), e.to_string()));
    }
    w
}

fn md_kind(k: md_model::ModuleKind) -> Option<ModuleKind> {
    use md_model::ModuleKind as M;
    Some(match k {
        M::ObjectModule => ModuleKind::Object,
        M::ManagerModule => ModuleKind::Manager,
        M::RecordSetModule => ModuleKind::RecordSet,
        M::ValueManagerModule => ModuleKind::ValueManager,
        M::CommandModule => ModuleKind::Command,
        M::SessionModule => ModuleKind::Session,
        M::ManagedApplicationModule => ModuleKind::ManagedApplication,
        M::OrdinaryApplicationModule => ModuleKind::OrdinaryApplication,
        M::ExternalConnectionModule => ModuleKind::ExternalConnection,
        M::FormModule => ModuleKind::Form,
        M::Module => return None,
    })
}

impl Modules {
    pub fn new(names: Arc<AppNamesImpl>, cache: Arc<CompileCache>, compat: CompatMode) -> Modules {
        let words = words();
        let mut entries = Vec::new();
        let mut by_name = HashMap::new();
        for i in 0..names.module_count() {
            let Some(m) = names.module_at(i) else {
                continue;
            };
            let idx = entries.len() as u32;
            by_name.entry(fold_name(&m.name)).or_insert(idx);
            let en: Vec<&str> = m
                .name
                .split('.')
                .enumerate()
                .map(|(position, seg)| {
                    if !matches!(position, 0 | 2 | 4) {
                        return seg;
                    }
                    words
                        .iter()
                        .find(|(r, _)| r == seg)
                        .map_or(seg, |(_, e)| e.as_str())
                })
                .collect();
            by_name.entry(fold_name(&en.join("."))).or_insert(idx);
            entries.push(Entry {
                at: i,
                name: m.name,
                kind: m.kind,
                owner: m.owner,
                compiled: OnceLock::new(),
            });
        }
        Modules {
            names,
            cache,
            compat,
            entries,
            by_name,
            managers_ok: std::array::from_fn(|_| OnceLock::new()),
        }
    }

    pub fn compat(&self) -> CompatMode {
        self.compat
    }

    pub fn names(&self) -> &Arc<AppNamesImpl> {
        &self.names
    }

    pub fn cache(&self) -> &Arc<CompileCache> {
        &self.cache
    }

    pub fn module_ref(&self, name: &str) -> Option<ModuleRef> {
        self.by_name.get(&fold_name(name)).map(|&i| ModuleRef(i))
    }

    fn entry(&self, m: ModuleRef) -> Option<&Entry> {
        self.entries.get(m.0 as usize)
    }

    pub fn name_of(&self, m: ModuleRef) -> Option<&str> {
        self.entry(m).map(|e| e.name.as_str())
    }

    pub fn compiled(&self, m: ModuleRef) -> Got {
        let e = self.entry(m).ok_or(ModuleError::InvalidRef(m))?;
        e.compiled
            .get_or_init(|| self.compile_entry(e))
            .clone()
            .map_err(ModuleError::Compile)
    }

    fn compile_entry(&self, e: &Entry) -> Compiled {
        let text = self.names.module_at(e.at).map_or(&[][..], |s| s.text);
        let hash: [u8; 32] = Sha256::digest(text).into();
        let ctx = ExecContext::Server;
        let key = Key {
            text: hash,
            names: self.names.fingerprint(),
            module: e.name.clone(),
            kind: kind_bytes(e.kind),
            context: ctx,
            compat: self.compat,
        };
        self.cache.get_or_compile(key, || {
            let o = CompileOptions {
                module_name: &e.name,
                kind: e.kind,
                context: ctx,
                compat: self.compat,
                catalog: platform_catalog::catalog(),
                app: &*self.names,
                extension: None,
            };
            let src = SourceText::from_utf8(&e.name, text);
            compile(&src, &o)
        })
    }

    pub fn compiled_or_fatal(
        &self,
        m: ModuleRef,
        lang: Lang,
    ) -> Result<Arc<CompiledModule>, Exception> {
        match self.compiled(m) {
            Ok(c) => Ok(c),
            Err(ModuleError::Compile(err)) => match self.entry(m) {
                Some(e) => Err(bsl_vm::module_init_error(&e.name, &err, lang)),
                None => Err(invalid_ref(m)),
            },
            Err(ModuleError::InvalidRef(r)) => Err(invalid_ref(r)),
        }
    }

    pub fn instance(
        &self,
        ex: &mut Execution,
        m: ModuleRef,
    ) -> Result<ModuleInstanceRef, Exception> {
        let c = self.compiled_or_fatal(m, ex.lang())?;
        ex.instantiate(&c, ModuleContext::None)
    }

    pub fn compile_managers(
        &self,
        model: &AppModel,
        kind: ObjKind,
        lang: Lang,
    ) -> Result<(), Exception> {
        let done = self.managers_ok.get(kind.index());
        if done.is_some_and(|d| d.get().is_some()) {
            return Ok(());
        }
        for (i, e) in self.entries.iter().enumerate() {
            let of_kind = e.owner.is_some_and(|o| model.obj(o).kind == kind);
            if e.kind == ModuleKind::Manager && of_kind {
                self.compiled_or_fatal(ModuleRef(i as u32), lang)?;
            }
        }
        if let Some(d) = done {
            let _ = d.set(());
        }
        Ok(())
    }

    pub fn global_modules(&self, ctx: ExecContext) -> Vec<ModuleRef> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                matches!(e.kind, ModuleKind::Common(f) if f.global
                && match ctx {
                    ExecContext::Server => f.server,
                    ExecContext::ExternalConnection => f.server && f.external_connection,
                    _ => false,
                })
            })
            .map(|(i, _)| ModuleRef(i as u32))
            .collect()
    }

    pub fn session_module(&self) -> Option<ModuleRef> {
        self.entries
            .iter()
            .position(|e| e.kind == ModuleKind::Session)
            .map(|i| ModuleRef(i as u32))
    }

    pub fn of_object(&self, obj: ObjId, kind: md_model::ModuleKind) -> Option<ModuleRef> {
        let want = md_kind(kind)?;
        self.entries
            .iter()
            .position(|e| e.owner == Some(obj) && e.kind == want)
            .map(|i| ModuleRef(i as u32))
    }
}

fn invalid_ref(m: ModuleRef) -> Exception {
    internal_error(format!("internal: invalid module reference {}", m.0))
}

pub(crate) fn internal_error(text: String) -> Exception {
    Exception::Fatal(std::rc::Rc::new(bsl_vm::FatalError {
        kind: bsl_vm::FatalKind::Internal,
        text,
        cause: None,
    }))
}
