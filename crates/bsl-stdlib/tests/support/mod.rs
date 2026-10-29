#![allow(dead_code)]

use bsl_compiler::{CommonFlags, CompileError, CompileOptions, ExecContext, ModuleKind, compile};
use bsl_prim::{BslDate, Collation, CompatMode, IcuCollation, Number, fold_name};
use bsl_syntax::SourceText;
use bsl_vm::*;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

pub struct NullHost;

impl AppHost for NullHost {
    fn names(&self) -> &dyn AppNames {
        &NoApp
    }

    fn global_value(&self, ex: &mut Execution, _g: &AppGlobal) -> Result<Value, Exception> {
        Err(ex.error(HostError::text(
            "Нет глобальных значений",
            "No global values",
        )))
    }

    fn module(&self, ex: &mut Execution, _m: ModuleRef) -> Result<ModuleInstanceRef, Exception> {
        Err(ex.error(HostError::text("Модуль не найден", "Module not found")))
    }

    fn module_ref(&self, _name: &str) -> Option<ModuleRef> {
        None
    }
}

pub struct TestEnv {
    locale: String,
    lang: Lang,
    config: String,
    collation: IcuCollation,
    now: BslDate,
    compat: CompatMode,
}

impl SessionEnv for TestEnv {
    fn locale(&self) -> &str {
        &self.locale
    }
    fn ib_locale(&self) -> &str {
        "ru_RU"
    }
    fn collation(&self) -> &dyn Collation {
        &self.collation
    }
    fn interface_language(&self) -> Lang {
        self.lang
    }
    fn config_language(&self) -> &str {
        &self.config
    }
    fn time_zone(&self) -> &str {
        "Pacific/Auckland"
    }
    fn now_utc(&self) -> BslDate {
        self.now
    }
    fn compat(&self) -> CompatMode {
        self.compat
    }
    fn code_pages(&self) -> [&str; 3] {
        ["windows-1251", "IBM866", "windows-1252"]
    }
}

pub fn env_with(locale: &str, lang: Lang, config: &str) -> Arc<dyn SessionEnv> {
    env_full(locale, lang, config, CompatMode::DontUse)
}

pub fn env_compat(compat: CompatMode) -> Arc<dyn SessionEnv> {
    env_full("ru_RU", Lang::Ru, "ru", compat)
}

fn env_full(locale: &str, lang: Lang, config: &str, compat: CompatMode) -> Arc<dyn SessionEnv> {
    let now = BslDate::from_parts(2025, 1, 15, 0, 0, 0)
        .expect("valid date")
        .add_seconds(&Number::parse("0.5").expect("number"));
    Arc::new(TestEnv {
        locale: locale.into(),
        lang,
        config: config.into(),
        collation: IcuCollation::new("ru_RU"),
        now,
        compat,
    })
}

pub fn env() -> Arc<dyn SessionEnv> {
    env_with("ru_RU", Lang::Ru, "ru")
}

pub fn runtime() -> Runtime {
    static RT: OnceLock<Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        let mut reg = Registry::new(platform_catalog::catalog());
        bsl_stdlib::register(&mut reg);
        Runtime::new(platform_catalog::catalog(), reg, Arc::new(NullHost))
    })
    .clone()
}

pub fn execution() -> Execution {
    runtime().execution(env(), ExecOptions::default())
}

pub fn eval(f: impl FnOnce(&mut Execution) -> Result<Value, Exception>) -> String {
    eval_in(execution(), f)
}

pub fn eval_in(
    mut ex: Execution,
    f: impl FnOnce(&mut Execution) -> Result<Value, Exception>,
) -> String {
    let r = f(&mut ex);
    show(&mut ex, r)
}

pub fn call(name: &str, args: &[Value]) -> String {
    eval(|ex| {
        let mut args: Vec<Arg> = args.iter().cloned().map(Arg::Val).collect();
        ex.call_global(name, &mut args)
    })
}

pub fn present(v: &Value) -> String {
    eval(|ex| ex.present(v).map(Value::String))
}

pub fn member(ex: &mut Execution, obj: &Value, name: &str) -> Result<Value, Exception> {
    ex.get_member(obj, name)
}

pub fn index(ex: &mut Execution, obj: &Value, key: &Value) -> Result<Value, Exception> {
    ex.index_get(obj, key)
}

pub fn for_each(
    ex: &mut Execution,
    coll: &Value,
    mut body: impl FnMut(&mut Execution, Value) -> Result<(), Exception>,
) -> Result<(), Exception> {
    let Value::Object(o) = coll else {
        return Err(ex.error(HostError::key("bsl:IDS_RTE_NOTENUM")));
    };
    if let Some(keys) = o.iter_keys(ex).map_err(|e| ex.call_error(e))? {
        for key in keys {
            if let Some(v) = o.iter_value(ex, &key).map_err(|e| ex.call_error(e))? {
                body(ex, v)?;
            }
        }
        return Ok(());
    }
    let mut i = 0;
    loop {
        let Some(len) = o.iter_len(ex) else {
            return Err(ex.error(HostError::key("bsl:IDS_RTE_NOTENUM")));
        };
        if i >= len {
            return Ok(());
        }
        let item = o.iter_get(ex, i).map_err(|e| ex.call_error(e))?;
        body(ex, item)?;
        i += 1;
    }
}

pub fn type_idx(name: &str) -> TypeIdx {
    platform_catalog::catalog()
        .find_type(name)
        .unwrap_or_else(|| panic!("no catalog type {name}"))
}

pub fn enum_value(ex: &mut Execution, ty: &str, value: &str) -> Result<Value, Exception> {
    let mgr = ex.system_enum(type_idx(ty))?;
    member(ex, &mgr, value)
}

pub fn show(ex: &mut Execution, r: Result<Value, Exception>) -> String {
    match r {
        Ok(v) => value(ex, &v),
        Err(Exception::Catchable(info)) => {
            let mut chain = Vec::new();
            let mut all = CategorySet::default();
            let mut cur = Some(&*info);
            while let Some(i) = cur {
                chain.push(i.description.to_string_lossy());
                all = all.union(i.categories);
                cur = i.cause.as_deref();
            }
            let mut out = format!("err: {}", chain.join(" <- "));
            let default = CategorySet::of(&[
                Category::AllErrors,
                Category::ScriptRuntimeError,
                Category::ScriptUseError,
            ]);
            if all != default {
                let names: Vec<&str> = all.iter().map(Category::name_ru).collect();
                out.push_str(&format!(" [{}]", names.join(",")));
            }
            if info.line > 0 {
                out.push_str(&format!(" @{}", info.line));
            }
            out
        }
        Err(Exception::Fatal(f)) => format!("fatal: {}", f.text),
    }
}

fn value(ex: &Execution, v: &Value) -> String {
    match v {
        Value::Undefined => "Неопределено".into(),
        Value::Null => "NULL".into(),
        Value::Bool(b) => (if *b { "Истина" } else { "Ложь" }).into(),
        Value::Number(n) => n.to_xml_string(),
        Value::String(s) => format!("\"{}\"", s.to_string_lossy()),
        Value::Date(d) => format!("'{}'", d.to_xml_string()),
        Value::Type(t) => format!("<{}>", ex.runtime().types().name(*t, Lang::Ru)),
        Value::Object(o) => format!(
            "<объект {}>",
            ex.runtime()
                .types()
                .name(HostObject::type_id(&**o), Lang::Ru)
        ),
    }
}

pub fn run(src: &str) -> String {
    run_opts(src, ExecOptions::default())
}

pub fn run_opts(src: &str, opts: ExecOptions) -> String {
    run_env(src, env(), opts)
}

pub fn run_env(src: &str, env: Arc<dyn SessionEnv>, opts: ExecOptions) -> String {
    run_env_context(src, env, opts, ExecContext::Server)
}

pub fn run_env_context(
    src: &str,
    env: Arc<dyn SessionEnv>,
    opts: ExecOptions,
    context: ExecContext,
) -> String {
    let o = CompileOptions {
        module_name: "",
        kind: if context.is_client() {
            ModuleKind::ManagedApplication
        } else {
            ModuleKind::Object
        },
        context,
        compat: CompatMode::DontUse,
        catalog: platform_catalog::catalog(),
        app: &NoApp,
        extension: None,
    };
    let module = match compile(&SourceText::from_str("m", src), &o) {
        Ok(m) => m,
        Err(e) => return format!("compile error: {e:?}"),
    };
    let mut ex = runtime().execution(env, opts);
    let inst = ex.bare_instance(module, ModuleContext::None);
    match ex.run_body_with_locals(&inst) {
        Ok(locals) => {
            let v = locals
                .into_iter()
                .find(|(n, _)| n == "Р")
                .map_or(Value::Undefined, |(_, v)| v);
            value(&ex, &v)
        }
        Err(Exception::Fatal(f)) if f.kind != FatalKind::ModuleInit => {
            format!("fatal: {:?}: {}", f.kind, f.text)
        }
        Err(e) => show(&mut ex, Err(e)),
    }
}

fn full_name(short: &str) -> String {
    format!("ОбщийМодуль.{short}.Модуль")
}

struct ModNames {
    shorts: Vec<String>,
}

impl AppNames for ModNames {
    fn global_value(&self, name: &str, _: ExecContext) -> Option<AppGlobal> {
        let folded = fold_name(name);
        let short = self.shorts.iter().find(|s| fold_name(s) == folded)?;
        Some(AppGlobal::CommonModule {
            name: full_name(short),
        })
    }
    fn global_method(&self, _: &str, _: ExecContext) -> Option<AppMethodSig> {
        None
    }
    fn context_value(&self, _: &str, _: &str) -> Option<ContextValue> {
        None
    }
    fn context_method(&self, _: &str, _: &str) -> bool {
        false
    }
}

#[derive(Default)]
struct Instances(HashMap<u32, ModuleInstanceRef>);

pub struct ModsHost {
    mods: Vec<(String, String)>,
    names: ModNames,
    compiled: Mutex<HashMap<u32, Result<Arc<CompiledModule>, CompileError>>>,
}

impl ModsHost {
    pub fn new(mods: &[(&str, &str)]) -> ModsHost {
        ModsHost {
            mods: mods
                .iter()
                .map(|(n, s)| (n.to_string(), s.to_string()))
                .collect(),
            names: ModNames {
                shorts: mods.iter().map(|(n, _)| n.to_string()).collect(),
            },
            compiled: Mutex::new(HashMap::new()),
        }
    }
}

impl AppHost for ModsHost {
    fn names(&self) -> &dyn AppNames {
        &self.names
    }

    fn global_value(&self, ex: &mut Execution, _g: &AppGlobal) -> Result<Value, Exception> {
        Err(ex.error(HostError::text("Нет значения", "No value")))
    }

    fn module(&self, ex: &mut Execution, m: ModuleRef) -> Result<ModuleInstanceRef, Exception> {
        let cached = ex
            .host_data()
            .and_then(|d| d.downcast_mut::<Instances>())
            .and_then(|c| c.0.get(&m.0).cloned());
        if let Some(i) = cached {
            return Ok(i);
        }
        let Some((short, src)) = self.mods.get(m.0 as usize) else {
            return Err(ex.error(HostError::text("Модуль не найден", "Module not found")));
        };
        let name = full_name(short);
        let compiled = {
            let mut cache = self.compiled.lock().expect("lock");
            cache
                .entry(m.0)
                .or_insert_with(|| {
                    let o = CompileOptions {
                        module_name: &name,
                        kind: ModuleKind::Common(CommonFlags {
                            server: true,
                            ..CommonFlags::default()
                        }),
                        context: ExecContext::Server,
                        compat: CompatMode::DontUse,
                        catalog: platform_catalog::catalog(),
                        app: &self.names,
                        extension: None,
                    };
                    compile(&SourceText::from_str(&name, src), &o)
                })
                .clone()
        };
        let module = compiled.map_err(|e| module_init_error(&name, &e, ex.lang()))?;
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
        let folded = fold_name(name);
        let i = self
            .mods
            .iter()
            .position(|(s, _)| fold_name(&full_name(s)) == folded)?;
        Some(ModuleRef(u32::try_from(i).ok()?))
    }
}

pub fn run_mods(mods: &[(&str, &str)], src: &str) -> String {
    run_mods_opts(mods, src, ExecOptions::default())
}

pub fn run_mods_opts(mods: &[(&str, &str)], src: &str, opts: ExecOptions) -> String {
    let host = ModsHost::new(mods);
    let o = CompileOptions {
        module_name: "",
        kind: ModuleKind::Object,
        context: ExecContext::Server,
        compat: CompatMode::DontUse,
        catalog: platform_catalog::catalog(),
        app: &host.names,
        extension: None,
    };
    let module = match compile(&SourceText::from_str("m", src), &o) {
        Ok(m) => m,
        Err(e) => return format!("compile error: {e:?}"),
    };
    let mut reg = Registry::new(platform_catalog::catalog());
    bsl_stdlib::register(&mut reg);
    let rt = Runtime::new(platform_catalog::catalog(), reg, Arc::new(host));
    let mut ex = rt.execution(env(), opts);
    let inst = ex.bare_instance(module, ModuleContext::None);
    match ex.run_body_with_locals(&inst) {
        Ok(locals) => {
            let v = locals
                .into_iter()
                .find(|(n, _)| n == "Р")
                .map_or(Value::Undefined, |(_, v)| v);
            value(&ex, &v)
        }
        Err(Exception::Fatal(f)) if f.kind != FatalKind::ModuleInit => {
            format!("fatal: {:?}: {}", f.kind, f.text)
        }
        Err(e) => show(&mut ex, Err(e)),
    }
}
