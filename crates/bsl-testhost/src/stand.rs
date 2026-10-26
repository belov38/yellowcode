use crate::kinds::display_name;
use crate::runner::stand_object;
use crate::scan::{ConfigScan, ScanError};
use crate::wrapper::STAND_GLOBAL;
use bsl_compiler::{
    AppGlobal, AppMethodSig, AppNames, CompileError, CompileOptions, CompiledModule, ContextValue,
    ExecContext, ModuleKind, compile,
};
use bsl_prim::{CompatMode, fold_name};
use bsl_stdlib::MemFs;
use bsl_syntax::SourceText;
use bsl_vm::{
    AppHost, Exception, Execution, FileSystem, HostError, Lang, ModuleContext, ModuleInstanceRef,
    ModuleRef, Value, module_init_error,
};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

struct StandNames {
    scan: ConfigScan,
    script_variant: Lang,
}

fn full_name(short: &str, lang: Lang) -> String {
    display_name(&format!("ОбщийМодуль.{short}.Модуль"), lang)
}

impl AppNames for StandNames {
    fn application_variable(
        &self,
        name: &str,
        ctx: ExecContext,
    ) -> Option<bsl_compiler::ApplicationVariable> {
        self.scan.application_variable(name, ctx)
    }

    fn global_value(&self, name: &str, ctx: ExecContext) -> Option<AppGlobal> {
        if fold_name(name) == fold_name(STAND_GLOBAL) {
            return Some(AppGlobal::CommonModule {
                name: STAND_GLOBAL.into(),
            });
        }
        match self.scan.global_value(name, ctx)? {
            AppGlobal::CommonModule { name } => Some(AppGlobal::CommonModule {
                name: full_name(&name, self.script_variant),
            }),
        }
    }
    fn global_method(&self, name: &str, ctx: ExecContext) -> Option<AppMethodSig> {
        self.scan.global_method(name, ctx)
    }
    fn context_value(&self, module: &str, name: &str) -> Option<ContextValue> {
        self.scan.context_value(module, name)
    }
    fn context_method(&self, module: &str, name: &str) -> bool {
        self.scan.context_method(module, name)
    }
}

#[derive(Default)]
struct Instances(HashMap<u32, ModuleInstanceRef>);

#[derive(Clone)]
enum Failure {
    Compile(CompileError),
    Io(String),
}

type Compiled = Result<Arc<CompiledModule>, Failure>;

pub struct StandHost {
    names: StandNames,
    commons: Vec<usize>,
    by_name: HashMap<String, ModuleRef>,
    compiled: Mutex<HashMap<u32, Compiled>>,
    files: MemFs,
    script_variant: Lang,
    config_language: String,
}

fn config_settings(cfg_dir: &Path) -> (Lang, String) {
    let props = crate::xml::properties(&cfg_dir.join("Configuration.xml")).unwrap_or_default();
    let get = |k: &str| props.iter().find(|(n, _)| n == k).map(|(_, v)| v.trim());
    let variant = match get("ScriptVariant") {
        Some("English") => Lang::En,
        _ => Lang::Ru,
    };
    let code = get("DefaultLanguage")
        .and_then(|l| l.strip_prefix("Language."))
        .and_then(|name| {
            let file = cfg_dir.join("Languages").join(format!("{name}.xml"));
            crate::xml::properties(&file).ok()
        })
        .and_then(|p| {
            p.into_iter()
                .find(|(n, _)| n == "LanguageCode")
                .map(|(_, v)| v.trim().to_owned())
        })
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| "ru".to_owned());
    (variant, code)
}

impl StandHost {
    pub fn load(cfg_dir: &Path) -> Result<StandHost, ScanError> {
        let scan = ConfigScan::load(cfg_dir)?;
        let (script_variant, config_language) = config_settings(cfg_dir);
        let mut commons = Vec::new();
        let mut by_name = HashMap::new();
        for (i, m) in scan.modules().iter().enumerate() {
            if matches!(m.kind, ModuleKind::Common(_) | ModuleKind::HttpService) {
                let r = ModuleRef(u32::try_from(commons.len()).unwrap_or(u32::MAX));
                by_name.insert(fold_name(&m.name).to_string(), r);
                by_name.insert(
                    fold_name(&display_name(&m.name, script_variant)).to_string(),
                    r,
                );
                commons.push(i);
            }
        }
        Ok(StandHost {
            script_variant,
            config_language,
            names: StandNames {
                scan,
                script_variant,
            },
            commons,
            by_name,
            compiled: Mutex::new(HashMap::new()),
            files: MemFs::stand(),
        })
    }

    pub fn config_language(&self) -> &str {
        &self.config_language
    }

    pub fn scan(&self) -> &ConfigScan {
        &self.names.scan
    }

    fn compiled(&self, m: ModuleRef) -> Option<(String, Compiled)> {
        let file = self
            .names
            .scan
            .modules()
            .get(*self.commons.get(m.0 as usize)?)?;
        let name = display_name(&file.name, self.script_variant);
        let mut cache = self.compiled.lock().ok()?;
        if let Some(c) = cache.get(&m.0) {
            return Some((name, c.clone()));
        }
        let r = match std::fs::read_to_string(&file.path) {
            Ok(text) => {
                let text = text.strip_prefix('\u{FEFF}').unwrap_or(&text);
                let o = CompileOptions {
                    module_name: &name,
                    kind: file.kind,
                    context: ExecContext::Server,
                    compat: CompatMode::DontUse,
                    catalog: platform_catalog::catalog(),
                    app: &self.names,
                    extension: None,
                };
                compile(&SourceText::from_str(&name, text), &o).map_err(Failure::Compile)
            }
            Err(e) => Err(Failure::Io(format!("{}: {e}", file.path.display()))),
        };
        cache.insert(m.0, r.clone());
        Some((name, r))
    }
}

impl AppHost for StandHost {
    fn names(&self) -> &dyn AppNames {
        &self.names
    }

    fn script_variant(&self) -> Lang {
        self.script_variant
    }

    fn global_value(&self, ex: &mut Execution, g: &AppGlobal) -> Result<Value, Exception> {
        match g {
            AppGlobal::CommonModule { name } if name == STAND_GLOBAL => Ok(stand_object()),
            AppGlobal::CommonModule { name } => Err(ex.error(HostError::text(
                &format!("Нет значения {name}"),
                &format!("No value {name}"),
            ))),
        }
    }

    fn module(&self, ex: &mut Execution, m: ModuleRef) -> Result<ModuleInstanceRef, Exception> {
        let cached = ex
            .host_data()
            .and_then(|d| d.downcast_mut::<Instances>())
            .and_then(|c| c.0.get(&m.0).cloned());
        if let Some(i) = cached {
            return Ok(i);
        }
        let Some((name, compiled)) = self.compiled(m) else {
            return Err(ex.error(HostError::text("Модуль не найден", "Module not found")));
        };
        let module = match compiled {
            Ok(m) => m,
            Err(Failure::Compile(e)) => return Err(module_init_error(&name, &e, ex.lang())),
            Err(Failure::Io(text)) => return Err(ex.error(HostError::text(&text, &text))),
        };
        let inst = ex.instantiate(&module, ModuleContext::None)?;
        if ex.host_data().is_none() {
            ex.set_host_data(Box::new(Instances::default()));
        }
        if let Some(c) = ex.host_data().and_then(|d| d.downcast_mut::<Instances>()) {
            c.0.insert(m.0, inst.clone());
        }
        Ok(inst)
    }

    fn module_ref(&self, name: &str) -> Option<ModuleRef> {
        self.by_name.get(&*fold_name(name)).copied()
    }

    fn file_system(&self) -> Option<&dyn FileSystem> {
        Some(&self.files)
    }
}
