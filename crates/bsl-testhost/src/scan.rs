use crate::kinds::{classify, compose, kind_ru};
use crate::scan_io::ScanInput;
use crate::xml;
mod application;
use bsl_compiler::{
    AppGlobal, AppMethodSig, AppNames, CatalogBuiltins, CommonFlags, ContextValue, ExecContext,
    ModuleKind, ParamSig, Reuse,
};
use bsl_prim::fold_name;
use bsl_syntax::{ParseMode, ParseOptions, SourceText, ast::MethodKind};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanErrorKind {
    Io,
    Changed,
    Poisoned,
    InvalidData,
}

#[derive(Debug, Clone)]
pub struct ScanError {
    pub kind: ScanErrorKind,
    pub path: PathBuf,
    pub what: String,
}

impl ScanError {
    pub(crate) fn new(path: &Path, what: impl Into<String>) -> ScanError {
        Self::typed(path, ScanErrorKind::InvalidData, what)
    }
    pub(crate) fn typed(path: &Path, kind: ScanErrorKind, what: impl Into<String>) -> Self {
        Self {
            kind,
            path: path.to_path_buf(),
            what: what.into(),
        }
    }
}

impl fmt::Display for ScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path.display(), self.what)
    }
}

impl std::error::Error for ScanError {}

#[derive(Clone, Debug)]
pub struct ModuleFile {
    pub path: PathBuf,
    pub name: String,
    pub kind: ModuleKind,
    pub owner: Option<String>,
}

struct CommonModule {
    name: String,
    flags: CommonFlags,
    file: Option<PathBuf>,
}

#[derive(Default)]
struct Members {
    props: HashSet<String>,
    methods: HashSet<String>,
}

#[derive(Default)]
struct Object {
    members: HashSet<String>,
    manager: HashSet<String>,
}

#[derive(Default)]
struct FormCtx {
    attrs: HashSet<String>,
    extension: Arc<Members>,
}

type MethodCache = Mutex<HashMap<(String, ExecContext), Arc<Vec<AppMethodSig>>>>;

struct ModuleCtx {
    ctx: CtxKind,
    template: Arc<Members>,
    own: HashSet<String>,
    common_attrs: bool,
    extension: Arc<Members>,
}

impl ModuleCtx {
    fn has_value(&self, n: &str, common_attrs: &HashSet<String>) -> bool {
        self.template.props.contains(n)
            || self.own.contains(n)
            || (self.common_attrs && common_attrs.contains(n))
            || self.extension.props.contains(n)
    }

    fn has_method(&self, n: &str) -> bool {
        self.template.methods.contains(n) || self.extension.methods.contains(n)
    }
}

pub struct ConfigScan {
    input: ScanInput,
    root: PathBuf,
    modules: Vec<ModuleFile>,
    commons: HashMap<String, CommonModule>,
    objects: HashMap<String, Object>,
    common_attrs: HashSet<String>,
    form_files: HashMap<String, PathBuf>,
    global_cache: MethodCache,
    application_vars: application::VariableCache,
    template_cache: Mutex<HashMap<String, Arc<Members>>>,
    global_index: Mutex<HashMap<ExecContext, Arc<HashMap<String, AppMethodSig>>>>,
    ctx_cache: Mutex<HashMap<String, Option<Arc<ModuleCtx>>>>,
}

fn dir_of(element: &str) -> Option<&'static str> {
    Some(match element {
        "Catalog" => "Catalogs",
        "Document" => "Documents",
        "DataProcessor" => "DataProcessors",
        "Report" => "Reports",
        "ExchangePlan" => "ExchangePlans",
        "ChartOfCharacteristicTypes" => "ChartsOfCharacteristicTypes",
        "ChartOfAccounts" => "ChartsOfAccounts",
        "ChartOfCalculationTypes" => "ChartsOfCalculationTypes",
        "BusinessProcess" => "BusinessProcesses",
        "Task" => "Tasks",
        "InformationRegister" => "InformationRegisters",
        "AccumulationRegister" => "AccumulationRegisters",
        "AccountingRegister" => "AccountingRegisters",
        "CalculationRegister" => "CalculationRegisters",
        "DocumentJournal" => "DocumentJournals",
        "Enum" => "Enums",
        "SettingsStorage" => "SettingsStorages",
        "FilterCriterion" => "FilterCriteria",
        "Constant" => "Constants",
        _ => return None,
    })
}

fn flag(v: &str) -> bool {
    v.trim() == "true"
}

fn common_flags(props: &[(String, String)]) -> CommonFlags {
    let mut f = CommonFlags::default();
    for (k, v) in props {
        match k.as_str() {
            "Global" => f.global = flag(v),
            "ClientManagedApplication" => f.client_managed = flag(v),
            "Server" => f.server = flag(v),
            "ExternalConnection" => f.external_connection = flag(v),
            "ClientOrdinaryApplication" => f.client_ordinary = flag(v),
            "ServerCall" => f.server_call = flag(v),
            "Privileged" => f.privileged = flag(v),
            "ReturnValuesReuse" => {
                f.reuse = match v.trim() {
                    "DuringRequest" => Reuse::DuringRequest,
                    "DuringSession" => Reuse::DuringSession,
                    _ => Reuse::DontUse,
                }
            }
            _ => {}
        }
    }
    f
}

const PREDEFINED: &str = "Predefined.xml";

fn walk(
    root: &Path,
    dir: &Path,
    out: &mut Vec<PathBuf>,
    xml: &mut HashMap<String, PathBuf>,
    input: &ScanInput,
) {
    let Some(rd) = input.read_dir(dir) else {
        return;
    };
    for e in rd {
        let Some((p, ft)) = input.entry(dir, e) else {
            continue;
        };
        if ft.is_dir() {
            walk(root, &p, out, xml, input);
        } else if p.extension().is_some_and(|x| x == "bsl")
            || p.file_name().is_some_and(|n| n == PREDEFINED)
        {
            out.push(p);
        } else if p.extension().is_some_and(|x| x == "xml")
            && p.parent().and_then(Path::parent) == Some(root)
            && let (Some(d), Some(n)) = (
                p.parent()
                    .and_then(|d| d.file_name())
                    .and_then(|s| s.to_str()),
                p.file_name().and_then(|s| s.to_str()),
            )
        {
            xml.insert(format!("{}/{}", compose(d), compose(n)), p);
        }
    }
}

const FORM_EXTENSIONS: [(&str, &str); 18] = [
    ("CatalogObject", "Extension for catalogs"),
    ("DocumentObject", "Extension for documents"),
    ("BusinessProcessObject", "Extension for business processes"),
    ("TaskObject", "Extension for tasks"),
    (
        "ChartOfCharacteristicTypesObject",
        "Extension for chart of characteristics types",
    ),
    ("ChartOfAccountsObject", "Extension for objects"),
    ("ChartOfCalculationTypesObject", "Extension for objects"),
    ("ExchangePlanObject", "Extension for objects"),
    ("ConstantsSet", "Extension for constants"),
    (
        "InformationRegisterRecordManager",
        "Extension for information register records",
    ),
    ("InformationRegisterRecordSet", "Extension for record set"),
    ("AccumulationRegisterRecordSet", "Extension for record set"),
    ("AccountingRegisterRecordSet", "Extension for record set"),
    ("CalculationRegisterRecordSet", "Extension for record set"),
    ("ReportObject", "Extension for reports"),
    ("DataProcessorObject", "Form extension for processings"),
    ("DynamicList", "Extension for dynamic lists"),
    ("SettingsComposer", "Extension for settings composer"),
];

fn form_extension(main_type: &str) -> Option<&'static str> {
    let local = main_type.rsplit(':').next().unwrap_or(main_type);
    let kind = local.split('.').next().unwrap_or(local);
    FORM_EXTENSIONS
        .iter()
        .find(|(k, _)| *k == kind)
        .map(|(_, id)| *id)
}

fn members_of(keep: impl Fn(&platform_catalog::TypeInfo) -> bool) -> Members {
    let mut mem = Members::default();
    for t in platform_catalog::catalog()
        .types()
        .iter()
        .filter(|t| keep(t))
    {
        for p in &t.properties {
            mem.props.insert(fold_name(&p.name_ru));
            mem.props.insert(fold_name(&p.name_en));
        }
        for m in &t.methods {
            mem.methods.insert(fold_name(&m.name_ru));
            mem.methods.insert(fold_name(&m.name_en));
        }
    }
    mem
}

impl ConfigScan {
    pub fn load(dir: &Path) -> Result<ConfigScan, ScanError> {
        Self::load_with(dir, ScanInput::default())
    }

    fn load_with(dir: &Path, input: ScanInput) -> Result<ConfigScan, ScanError> {
        let cfg_path = dir.join("Configuration.xml");
        let children = xml::config_children(&cfg_path, &input)?;
        let mut files = Vec::new();
        let mut xml_files = HashMap::new();
        walk(dir, dir, &mut files, &mut xml_files, &input);
        input.check()?;
        files.sort();
        let mut commons = HashMap::new();
        let mut objects = HashMap::new();
        let mut common_attrs = HashSet::new();
        for (el, name) in &children {
            if el == "CommonModule" {
                let key = format!("CommonModules/{}.xml", compose(name));
                let xml_path = xml_files.get(&key).ok_or_else(|| {
                    ScanError::new(&cfg_path, format!("no XML file for common module: {key}"))
                })?;
                let flags = common_flags(&xml::properties_with(xml_path, &input)?);
                commons.insert(
                    fold_name(name),
                    CommonModule {
                        name: name.clone(),
                        flags,
                        file: None,
                    },
                );
            } else if el == "CommonAttribute" {
                common_attrs.insert(fold_name(name));
            } else if let Some(d) = dir_of(el) {
                let key = format!("{d}/{}.xml", compose(name));
                let xml_path = xml_files.get(&key).ok_or_else(|| {
                    ScanError::new(&cfg_path, format!("no XML file for object: {key}"))
                })?;
                let names = xml::object_names(xml_path, &input)?;
                let o = Object {
                    members: names.fields.iter().map(|n| fold_name(n)).collect(),
                    manager: names.enum_values.iter().map(|n| fold_name(n)).collect(),
                };
                objects.insert(fold_name(&format!("{d}/{name}")), o);
            }
        }
        let mut modules = Vec::new();
        let mut form_files = HashMap::new();
        for path in files {
            let Ok(rel) = path.strip_prefix(dir) else {
                continue;
            };
            let Some(names) = rel
                .iter()
                .map(|s| s.to_str().map(|s| compose(s).into_owned()))
                .collect::<Option<Vec<String>>>()
            else {
                continue;
            };
            let segs: Vec<&str> = names.iter().map(String::as_str).collect();
            if let [d, n, "Ext", PREDEFINED] = segs.as_slice() {
                if let Some(o) = objects.get_mut(&fold_name(&format!("{d}/{n}"))) {
                    let items = xml::predefined_names(&path, &input)?;
                    o.manager.extend(items.iter().map(|n| fold_name(n)));
                }
                continue;
            }
            let Some(c) = classify(&segs) else { continue };
            let kind = match (&c.kind, &c.common) {
                (ModuleKind::Common(_), Some(n)) => {
                    let m = commons.get_mut(&fold_name(n));
                    let flags = m.as_ref().map(|m| m.flags).unwrap_or_default();
                    if let Some(m) = m {
                        m.file = Some(path.clone());
                    }
                    ModuleKind::Common(flags)
                }
                (k, _) => *k,
            };
            if kind == ModuleKind::Form
                && let Some(ext) = path.parent().and_then(Path::parent)
            {
                form_files.insert(fold_name(&c.name), ext.join("Form.xml"));
            }
            modules.push(ModuleFile {
                path,
                name: c.name,
                kind,
                owner: c.owner,
            });
        }
        Ok(ConfigScan {
            input,
            root: dir.to_path_buf(),
            modules,
            commons,
            objects,
            common_attrs,
            form_files,
            global_cache: Mutex::new(HashMap::new()),
            application_vars: Mutex::new(HashMap::new()),
            template_cache: Mutex::new(HashMap::new()),
            global_index: Mutex::new(HashMap::new()),
            ctx_cache: Mutex::new(HashMap::new()),
        })
    }

    pub fn load_strict(dir: &Path) -> Result<ConfigScan, ScanError> {
        let input = ScanInput::strict(dir)?;
        let scan = Self::load_with(dir, input)?;
        scan.verify_snapshot()?;
        Ok(scan)
    }

    pub fn check_io(&self) -> Result<(), ScanError> {
        self.input.check()
    }

    pub fn verify_snapshot(&self) -> Result<(), ScanError> {
        self.input.verify(&self.root)
    }

    fn cache<'a, T>(&self, cache: &'a Mutex<T>) -> Option<MutexGuard<'a, T>> {
        match cache.lock() {
            Ok(guard) => Some(guard),
            Err(_) => {
                self.input.latch(ScanError::typed(
                    &self.root,
                    ScanErrorKind::Poisoned,
                    "scanner cache poisoned",
                ));
                None
            }
        }
    }

    pub fn modules(&self) -> &[ModuleFile] {
        &self.modules
    }

    pub fn server_contexts(&self, m: &ModuleFile) -> bool {
        match m.kind {
            ModuleKind::Common(f) => f.server,
            ModuleKind::Object
            | ModuleKind::Manager
            | ModuleKind::RecordSet
            | ModuleKind::ValueManager
            | ModuleKind::Session
            | ModuleKind::HttpService
            | ModuleKind::WebService
            | ModuleKind::Form
            | ModuleKind::Command => true,
            ModuleKind::ManagedApplication
            | ModuleKind::OrdinaryApplication
            | ModuleKind::ExternalConnection => false,
        }
    }

    fn reach(flags: CommonFlags, ctx: ExecContext) -> Option<ExecContext> {
        let own = match ctx {
            ExecContext::Server => flags.server,
            ExecContext::ThinClient | ExecContext::WebClient | ExecContext::ThickClientManaged => {
                flags.client_managed
            }
            ExecContext::ThickClientOrdinary => flags.client_ordinary,
            ExecContext::ExternalConnection => flags.external_connection,
            _ => false,
        };
        let client = matches!(
            ctx,
            ExecContext::ThinClient
                | ExecContext::WebClient
                | ExecContext::ThickClientManaged
                | ExecContext::ThickClientOrdinary
        );
        if own {
            Some(ctx)
        } else if client && flags.server && flags.server_call {
            Some(ExecContext::Server)
        } else {
            None
        }
    }

    fn global_methods_of(&self, m: &CommonModule, ctx: ExecContext) -> Arc<Vec<AppMethodSig>> {
        let key = (fold_name(&m.name), ctx);
        let Some(mut cache) = self.cache(&self.global_cache) else {
            return Arc::default();
        };
        if let Some(v) = cache.get(&key) {
            return v.clone();
        }
        let sigs = m
            .file
            .as_ref()
            .and_then(|p| self.input.read(p).ok())
            .map(|bytes| parse_exports(&m.name, &bytes, ctx))
            .unwrap_or_default();
        let v = Arc::new(sigs);
        cache.insert(key, v.clone());
        v
    }

    fn template_members(&self, template: &str) -> Arc<Members> {
        let Some(mut cache) = self.cache(&self.template_cache) else {
            return Arc::default();
        };
        if let Some(v) = cache.get(template) {
            return v.clone();
        }
        let prefix = format!("{template}.<");
        let exact = fold_name(template);
        let mem = members_of(|t| t.name_ru.starts_with(&prefix) || fold_name(&t.name_ru) == exact);
        let v = Arc::new(mem);
        cache.insert(template.to_string(), v.clone());
        v
    }

    fn type_members(&self, id: &str) -> Arc<Members> {
        let key = format!("#{id}");
        let Some(mut cache) = self.cache(&self.template_cache) else {
            return Arc::default();
        };
        if let Some(v) = cache.get(&key) {
            return v.clone();
        }
        let v = Arc::new(members_of(|t| t.id == id));
        cache.insert(key, v.clone());
        v
    }

    fn form_ctx(&self, xml_path: &Path) -> FormCtx {
        let names = if self.input.optional_present(xml_path) {
            match xml::form_names(xml_path, &self.input) {
                Ok(names) => names,
                Err(error) => {
                    self.input.latch(error);
                    xml::FormNames::default()
                }
            }
        } else {
            xml::FormNames::default()
        };
        let extension = names
            .main_type
            .as_deref()
            .and_then(form_extension)
            .map(|id| self.type_members(id))
            .unwrap_or_default();
        FormCtx {
            attrs: names.attributes.iter().map(|n| fold_name(n)).collect(),
            extension,
        }
    }

    fn form_file(&self, module: &str, by_name: impl FnOnce() -> PathBuf) -> PathBuf {
        self.form_files
            .get(&fold_name(module))
            .cloned()
            .unwrap_or_else(by_name)
    }

    fn resolve(&self, module: &str) -> Option<Resolved<'_>> {
        let parts: Vec<String> = module.split('.').map(fold_name).collect();
        let p: Vec<&str> = parts.iter().map(String::as_str).collect();
        let ru_dir = |k: &str| {
            ALL_DIRS
                .iter()
                .find(|d| kind_ru(d).is_some_and(|r| fold_name(r) == k))
                .copied()
        };
        match p.as_slice() {
            ["общаяформа", _, "форма"] => {
                let name = module.split('.').nth(1)?;
                let xml = self.form_file(module, || {
                    self.root
                        .join("CommonForms")
                        .join(name)
                        .join("Ext")
                        .join("Form.xml")
                });
                Some(Resolved {
                    ctx: CtxKind::Form,
                    template: "ФормаКлиентскогоПриложения".to_string(),
                    object: None,
                    form_xml: Some(xml),
                    common_attrs: false,
                })
            }
            [k, _, "форма", _, "форма"] => {
                let dir = ru_dir(k)?;
                let mut it = module.split('.').skip(1);
                let (n, f) = (it.next()?, it.nth(1)?);
                let xml = self.form_file(module, || {
                    self.root
                        .join(dir)
                        .join(n)
                        .join("Forms")
                        .join(f)
                        .join("Ext")
                        .join("Form.xml")
                });
                Some(Resolved {
                    ctx: CtxKind::Form,
                    template: "ФормаКлиентскогоПриложения".to_string(),
                    object: None,
                    form_xml: Some(xml),
                    common_attrs: false,
                })
            }
            [k, _, "модульобъекта"] => {
                let dir = ru_dir(k)?;
                let n = module.split('.').nth(1)?;
                let ru = kind_ru(dir)?;
                if !matches!(
                    dir,
                    "Catalogs"
                        | "Documents"
                        | "DataProcessors"
                        | "Reports"
                        | "ExchangePlans"
                        | "ChartsOfCharacteristicTypes"
                        | "ChartsOfAccounts"
                        | "ChartsOfCalculationTypes"
                        | "BusinessProcesses"
                        | "Tasks"
                ) {
                    return None;
                }
                Some(Resolved {
                    ctx: CtxKind::Object,
                    template: format!("{ru}Объект"),
                    object: self.objects.get(&fold_name(&format!("{dir}/{n}"))),
                    form_xml: None,
                    common_attrs: true,
                })
            }
            [k, _, "модульнаборазаписей"] => {
                let dir = ru_dir(k)?;
                let n = module.split('.').nth(1)?;
                let ru = kind_ru(dir)?;
                Some(Resolved {
                    ctx: CtxKind::RecordSet,
                    template: format!("{ru}НаборЗаписей"),
                    object: self.objects.get(&fold_name(&format!("{dir}/{n}"))),
                    form_xml: None,
                    common_attrs: false,
                })
            }
            [k, _, "модульменеджера"] => {
                let dir = ru_dir(k)?;
                let n = module.split('.').nth(1)?;
                let ru = kind_ru(dir)?;
                Some(Resolved {
                    ctx: CtxKind::Manager,
                    template: format!("{ru}Менеджер"),
                    object: self.objects.get(&fold_name(&format!("{dir}/{n}"))),
                    form_xml: None,
                    common_attrs: false,
                })
            }
            ["константа", _, "модульменеджеразначения"] => {
                let n = module.split('.').nth(1)?;
                Some(Resolved {
                    ctx: CtxKind::ValueManager,
                    template: "КонстантаМенеджерЗначения".to_string(),
                    object: self.objects.get(&fold_name(&format!("Constants/{n}"))),
                    form_xml: None,
                    common_attrs: false,
                })
            }
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CtxKind {
    Object,
    RecordSet,
    ValueManager,
    Manager,
    Form,
}

struct Resolved<'a> {
    ctx: CtxKind,
    template: String,
    object: Option<&'a Object>,
    form_xml: Option<PathBuf>,
    common_attrs: bool,
}

const ALL_DIRS: [&str; 19] = [
    "Catalogs",
    "Documents",
    "DataProcessors",
    "Reports",
    "ExchangePlans",
    "ChartsOfCharacteristicTypes",
    "ChartsOfAccounts",
    "ChartsOfCalculationTypes",
    "BusinessProcesses",
    "Tasks",
    "InformationRegisters",
    "AccumulationRegisters",
    "AccountingRegisters",
    "CalculationRegisters",
    "DocumentJournals",
    "Enums",
    "SettingsStorages",
    "FilterCriteria",
    "Constants",
];

fn parse_exports(module: &str, bytes: &[u8], ctx: ExecContext) -> Vec<AppMethodSig> {
    let src = SourceText::from_utf8(module, bytes);
    let cat = platform_catalog::catalog();
    let builtins = CatalogBuiltins(cat);
    let out = bsl_syntax::parse(
        &src,
        &ParseOptions {
            mode: ParseMode::Module,
            context: ctx,
            methods_only: true,
            extension: false,
            builtins: &builtins,
        },
    );
    out.module
        .methods
        .iter()
        .filter(|m| m.export && !m.incomplete)
        .map(|m| AppMethodSig {
            module: module.to_string(),
            name: m.name.name.clone(),
            is_function: m.kind == MethodKind::Function,
            params: m
                .params
                .iter()
                .map(|p| ParamSig {
                    by_val: p.by_val,
                    has_default: p.default.is_some(),
                })
                .collect(),
        })
        .collect()
}

impl ConfigScan {
    fn global_index(&self, ctx: ExecContext) -> Arc<HashMap<String, AppMethodSig>> {
        if let Some(cache) = self.cache(&self.global_index)
            && let Some(v) = cache.get(&ctx)
        {
            return v.clone();
        }
        let mut mods: Vec<(&CommonModule, ExecContext)> = self
            .commons
            .values()
            .filter(|m| m.flags.global)
            .filter_map(|m| Some((m, Self::reach(m.flags, ctx)?)))
            .collect();
        mods.sort_by(|a, b| a.0.name.cmp(&b.0.name));
        let mut index = HashMap::new();
        for (m, at) in mods {
            for s in self.global_methods_of(m, at).iter() {
                index.entry(fold_name(&s.name)).or_insert_with(|| s.clone());
            }
        }
        let v = Arc::new(index);
        if let Some(mut cache) = self.cache(&self.global_index) {
            cache.insert(ctx, v.clone());
        }
        v
    }

    fn module_ctx(&self, module: &str) -> Option<Arc<ModuleCtx>> {
        if let Some(cache) = self.cache(&self.ctx_cache)
            && let Some(v) = cache.get(module)
        {
            return v.clone();
        }
        let v = self.resolve(module).map(|r| {
            let (own, extension) = match (r.object, r.form_xml) {
                (_, Some(p)) => {
                    let f = self.form_ctx(&p);
                    (f.attrs, f.extension)
                }
                (Some(o), None) if r.ctx == CtxKind::Manager => (o.manager.clone(), Arc::default()),
                (Some(o), None) => (o.members.clone(), Arc::default()),
                (None, None) => (HashSet::new(), Arc::default()),
            };
            Arc::new(ModuleCtx {
                ctx: r.ctx,
                template: self.template_members(&r.template),
                own,
                common_attrs: r.common_attrs,
                extension,
            })
        });
        if let Some(mut cache) = self.cache(&self.ctx_cache) {
            cache.insert(module.to_string(), v.clone());
        }
        v
    }
}

impl AppNames for ConfigScan {
    fn application_variable(
        &self,
        name: &str,
        ctx: ExecContext,
    ) -> Option<bsl_compiler::ApplicationVariable> {
        self.application_variables(ctx)
            .get(&fold_name(name))
            .cloned()
    }

    fn global_value(&self, name: &str, ctx: ExecContext) -> Option<AppGlobal> {
        let m = self.commons.get(&fold_name(name))?;
        (!m.flags.global && Self::reach(m.flags, ctx).is_some()).then(|| AppGlobal::CommonModule {
            name: m.name.clone(),
        })
    }

    fn global_method(&self, name: &str, ctx: ExecContext) -> Option<AppMethodSig> {
        self.global_index(ctx).get(&fold_name(name)).cloned()
    }

    fn context_value(&self, module: &str, name: &str) -> Option<ContextValue> {
        let c = self.module_ctx(module)?;
        let n = fold_name(name);
        let is_this = (c.ctx != CtxKind::Manager
            && matches!(n.as_str(), "этотобъект" | "thisobject"))
            || (c.ctx == CtxKind::Form && matches!(n.as_str(), "этаформа" | "thisform"));
        if is_this {
            Some(ContextValue::ThisObject)
        } else {
            c.has_value(&n, &self.common_attrs)
                .then_some(ContextValue::Member)
        }
    }

    fn context_method(&self, module: &str, name: &str) -> bool {
        self.module_ctx(module)
            .is_some_and(|c| c.has_method(&fold_name(name)))
    }
}
