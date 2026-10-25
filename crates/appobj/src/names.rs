use crate::model::{AppModel, FieldRole, ObjId, ObjKind};
use bsl_compiler::{
    AppGlobal, AppMethodSig, AppNames, CatalogBuiltins, CommonFlags, ContextValue, ExecContext,
    ModuleKind, ParamSig,
};
use bsl_prim::fold_name;
use bsl_syntax::{ParseMode, ParseOptions, SourceText, ast::MethodKind};
use md_model::{ChildTag, MdKind};
use platform_catalog::messages::Lang;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

const BOM: &[u8] = b"\xef\xbb\xbf";

#[derive(Clone, Debug)]
pub struct ModuleSource<'a> {
    pub name: String,
    pub kind: ModuleKind,
    pub text: &'a [u8],
    pub owner: Option<ObjId>,
}

#[derive(Clone, Copy, Debug)]
enum Loc {
    Config(usize),
    Object(usize, usize),
    Command(usize, usize),
}

#[derive(Clone, Debug)]
struct Listed {
    name: String,
    kind: ModuleKind,
    loc: Loc,
    owner: Option<ObjId>,
}

struct Common {
    name: String,
    flags: CommonFlags,
    obj: Option<usize>,
}

#[derive(Default)]
struct Members {
    props: HashSet<String>,
    methods: HashSet<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CtxKind {
    Object,
    RecordSet,
    ValueManager,
    Manager,
}

struct ModuleCtx {
    kind: CtxKind,
    template: Arc<Members>,
    own: HashSet<String>,
    common_attrs: bool,
}

type ExportIndex = Arc<HashMap<String, AppMethodSig>>;
type ExportCache = Mutex<HashMap<(usize, ExecContext), Arc<Vec<AppMethodSig>>>>;

pub struct AppNamesImpl {
    model: Arc<AppModel>,
    listed: Vec<Listed>,
    commons: HashMap<String, Common>,
    common_attrs: HashSet<String>,
    md_index: HashMap<crate::model::Uuid16, usize>,
    exports: ExportCache,
    global_index: Mutex<HashMap<ExecContext, ExportIndex>>,
    templates: Mutex<HashMap<String, Arc<Members>>>,
    module_ctx: Mutex<HashMap<String, Option<Arc<ModuleCtx>>>>,
    fingerprint: OnceLock<[u8; 32]>,
}

fn ru_word(kind: ObjKind) -> Option<&'static str> {
    (kind != ObjKind::Sequence).then(|| kind.name(Lang::Ru))
}

fn has_object_module(kind: ObjKind) -> bool {
    matches!(
        kind,
        ObjKind::Catalog
            | ObjKind::Document
            | ObjKind::DataProcessor
            | ObjKind::Report
            | ObjKind::ExchangePlan
            | ObjKind::ChartOfCharacteristicTypes
            | ObjKind::ChartOfAccounts
            | ObjKind::ChartOfCalculationTypes
            | ObjKind::BusinessProcess
            | ObjKind::Task
    )
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

fn parse_exports(module: &str, bytes: &[u8], ctx: ExecContext) -> Vec<AppMethodSig> {
    let src = SourceText::from_utf8(module, bytes);
    let builtins = CatalogBuiltins(platform_catalog::catalog());
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

fn module_word(w: &str) -> Option<CtxKind> {
    Some(match w {
        "модульобъекта" | "objectmodule" => CtxKind::Object,
        "модульнаборазаписей" | "recordsetmodule" => CtxKind::RecordSet,
        "модульменеджера" | "managermodule" => CtxKind::Manager,
        "модульменеджеразначения" | "valuemanagermodule" => {
            CtxKind::ValueManager
        }
        _ => return None,
    })
}

impl AppNamesImpl {
    pub fn new(model: Arc<AppModel>) -> AppNamesImpl {
        let cfg = &model.config;
        let md_index: HashMap<_, _> = cfg
            .objects
            .iter()
            .enumerate()
            .map(|(i, o)| (o.id, i))
            .collect();
        let mut commons = HashMap::new();
        for c in &model.common_modules {
            commons.entry(fold_name(&c.name)).or_insert(Common {
                name: c.name.clone(),
                flags: c.flags,
                obj: md_index.get(&c.md).copied(),
            });
        }
        let common_attrs = cfg
            .objects
            .iter()
            .filter(|o| o.kind == MdKind::CommonAttribute)
            .map(|o| fold_name(&o.name))
            .collect();

        let mut listed = Vec::new();
        let cfg_module = |k: md_model::ModuleKind| cfg.modules.iter().position(|m| m.kind == k);
        if let Some(i) = cfg_module(md_model::ModuleKind::SessionModule) {
            listed.push(Listed {
                name: match model.script {
                    md_model::ScriptVariant::Russian => "МодульСеанса",
                    md_model::ScriptVariant::English => "SessionModule",
                }
                .to_string(),
                kind: ModuleKind::Session,
                loc: Loc::Config(i),
                owner: None,
            });
        }
        for c in &model.common_modules {
            let Some(&o) = md_index.get(&c.md) else {
                continue;
            };
            let Some(mi) = cfg.objects.get(o).and_then(|obj| {
                obj.modules
                    .iter()
                    .position(|m| m.kind == md_model::ModuleKind::Module)
            }) else {
                continue;
            };
            if c.flags.server {
                listed.push(Listed {
                    name: format!("ОбщийМодуль.{}.Модуль", c.name),
                    kind: ModuleKind::Common(c.flags),
                    loc: Loc::Object(o, mi),
                    owner: None,
                });
            }
        }
        for def in &model.objects {
            let (Some(word), Some(&o)) = (ru_word(def.kind), md_index.get(&def.md)) else {
                continue;
            };
            let Some(obj) = cfg.objects.get(o) else {
                continue;
            };
            for (mi, m) in obj.modules.iter().enumerate() {
                let (kind, suffix) = match m.kind {
                    md_model::ModuleKind::ObjectModule => (ModuleKind::Object, "МодульОбъекта"),
                    md_model::ModuleKind::ManagerModule => (ModuleKind::Manager, "МодульМенеджера"),
                    md_model::ModuleKind::RecordSetModule => {
                        (ModuleKind::RecordSet, "МодульНабораЗаписей")
                    }
                    md_model::ModuleKind::ValueManagerModule if def.kind == ObjKind::Constant => {
                        (ModuleKind::ValueManager, "МодульМенеджераЗначения")
                    }
                    _ => continue,
                };
                listed.push(Listed {
                    name: format!("{word}.{}.{suffix}", def.name),
                    kind,
                    loc: Loc::Object(o, mi),
                    owner: Some(def.id),
                });
            }
            for (ci, c) in obj.children.iter().enumerate() {
                if c.tag == ChildTag::Command && c.module.is_some() {
                    listed.push(Listed {
                        name: format!("{word}.{}.Команда.{}.МодульКоманды", def.name, c.name),
                        kind: ModuleKind::Command,
                        loc: Loc::Command(o, ci),
                        owner: Some(def.id),
                    });
                }
            }
        }
        for (o, obj) in cfg.objects.iter().enumerate() {
            let (kind, prefix, suffix) = match obj.kind {
                MdKind::HTTPService => (ModuleKind::HttpService, "HTTPСервис", "Модуль"),
                MdKind::WebService => (ModuleKind::WebService, "WebСервис", "Модуль"),
                MdKind::IntegrationService => {
                    (ModuleKind::HttpService, "ИнтеграционныйСервис", "Модуль")
                }
                MdKind::CommonCommand => (ModuleKind::Command, "ОбщаяКоманда", "МодульКоманды"),
                _ => continue,
            };
            let want = if obj.kind == MdKind::CommonCommand {
                md_model::ModuleKind::CommandModule
            } else {
                md_model::ModuleKind::Module
            };
            if let Some(mi) = obj.modules.iter().position(|m| m.kind == want) {
                listed.push(Listed {
                    name: format!("{prefix}.{}.{suffix}", obj.name),
                    kind,
                    loc: Loc::Object(o, mi),
                    owner: None,
                });
            }
        }
        AppNamesImpl {
            model,
            listed,
            commons,
            common_attrs,
            md_index,
            exports: Mutex::new(HashMap::new()),
            global_index: Mutex::new(HashMap::new()),
            templates: Mutex::new(HashMap::new()),
            module_ctx: Mutex::new(HashMap::new()),
            fingerprint: OnceLock::new(),
        }
    }

    fn source_of(&self, loc: Loc) -> &[u8] {
        let cfg = &self.model.config;
        let m = match loc {
            Loc::Config(i) => cfg.modules.get(i),
            Loc::Object(o, m) => cfg.objects.get(o).and_then(|o| o.modules.get(m)),
            Loc::Command(o, c) => cfg
                .objects
                .get(o)
                .and_then(|o| o.children.get(c))
                .and_then(|c| c.module.as_ref()),
        };
        m.map_or(&[][..], |m| m.source.strip_prefix(BOM).unwrap_or(&m.source))
    }

    pub fn modules(&self) -> Vec<ModuleSource<'_>> {
        self.listed
            .iter()
            .map(|l| ModuleSource {
                name: l.name.clone(),
                kind: l.kind,
                text: self.source_of(l.loc),
                owner: l.owner,
            })
            .collect()
    }

    pub fn module_at(&self, i: usize) -> Option<ModuleSource<'_>> {
        self.listed.get(i).map(|l| ModuleSource {
            name: l.name.clone(),
            kind: l.kind,
            text: self.source_of(l.loc),
            owner: l.owner,
        })
    }

    pub fn fingerprint(&self) -> [u8; 32] {
        *self.fingerprint.get_or_init(|| self.compute_fingerprint())
    }

    fn compute_fingerprint(&self) -> [u8; 32] {
        fn put(h: &mut Sha256, s: &str) {
            h.update((s.len() as u64).to_le_bytes());
            h.update(s.as_bytes());
        }
        fn section(h: &mut Sha256, tag: &str, n: usize) {
            put(h, tag);
            h.update((n as u64).to_le_bytes());
        }
        let m = &self.model;
        let mut h = Sha256::new();
        put(&mut h, &format!("{:?}", m.script));
        section(&mut h, "common", m.common_modules.len());
        for c in &m.common_modules {
            put(&mut h, &c.name);
            let f = c.flags;
            h.update([
                f.server as u8,
                f.client_managed as u8,
                f.client_ordinary as u8,
                f.external_connection as u8,
                f.server_call as u8,
                f.global as u8,
                f.privileged as u8,
                f.reuse as u8,
            ]);
            if f.global {
                let text: &[u8] = self
                    .md_index
                    .get(&c.md)
                    .and_then(|&o| m.config.objects.get(o))
                    .and_then(|o| o.module(md_model::ModuleKind::Module))
                    .map_or(&[][..], |x| x.source.strip_prefix(BOM).unwrap_or(&x.source));
                h.update(Sha256::digest(text));
            }
        }
        section(&mut h, "objects", m.objects.len());
        for d in &m.objects {
            put(&mut h, d.kind.name(Lang::En));
            put(&mut h, &d.name);
            h.update([
                d.modules.object as u8,
                d.modules.manager as u8,
                d.modules.record_set as u8,
                d.modules.value_manager as u8,
            ]);
            section(&mut h, "fields", d.fields.len());
            for f in &d.fields {
                put(&mut h, &f.name);
                h.update([f.role as u8, f.std.is_some() as u8]);
            }
            section(&mut h, "tabular", d.tabular.len());
            for t in &d.tabular {
                put(&mut h, &t.name);
                section(&mut h, "tfields", t.fields.len());
                for f in &t.fields {
                    put(&mut h, &f.name);
                }
            }
            section(&mut h, "enum", d.enum_values.len());
            for v in &d.enum_values {
                put(&mut h, &v.name);
            }
            let md = self
                .md_index
                .get(&d.md)
                .and_then(|&i| m.config.objects.get(i));
            let flags: Vec<&str> = md
                .into_iter()
                .flat_map(|o| o.children.iter())
                .filter(|c| c.tag == ChildTag::AccountingFlag)
                .map(|c| c.name.as_str())
                .collect();
            section(&mut h, "flags", flags.len());
            for n in flags {
                put(&mut h, n);
            }
            let items: Vec<&str> = md
                .and_then(|o| o.predefined())
                .into_iter()
                .flat_map(|p| p.iter())
                .map(|i| i.name.as_str())
                .collect();
            section(&mut h, "predefined", items.len());
            for n in items {
                put(&mut h, n);
            }
            section(&mut h, "end", 0);
        }
        section(&mut h, "params", m.session_params.len());
        for p in &m.session_params {
            put(&mut h, &p.name);
        }
        let mut attrs: Vec<&String> = self.common_attrs.iter().collect();
        attrs.sort();
        section(&mut h, "attrs", attrs.len());
        for a in attrs {
            put(&mut h, a);
        }
        section(&mut h, "listed", self.listed.len());
        for l in &self.listed {
            put(&mut h, &l.name);
        }
        h.finalize().into()
    }

    pub fn module_count(&self) -> usize {
        self.listed.len()
    }

    fn exports_of(&self, c: &Common, at: ExecContext) -> Arc<Vec<AppMethodSig>> {
        let key = (c.obj.unwrap_or(usize::MAX), at);
        if let Ok(cache) = self.exports.lock()
            && let Some(v) = cache.get(&key)
        {
            return v.clone();
        }
        let bytes = c
            .obj
            .and_then(|o| self.model.config.objects.get(o))
            .and_then(|o| {
                o.modules
                    .iter()
                    .find(|m| m.kind == md_model::ModuleKind::Module)
            })
            .map(|m| m.source.strip_prefix(BOM).unwrap_or(&m.source));
        let v = Arc::new(
            bytes
                .map(|b| parse_exports(&c.name, b, at))
                .unwrap_or_default(),
        );
        if let Ok(mut cache) = self.exports.lock() {
            cache.insert(key, v.clone());
        }
        v
    }

    fn global_index(&self, ctx: ExecContext) -> ExportIndex {
        if let Ok(cache) = self.global_index.lock()
            && let Some(v) = cache.get(&ctx)
        {
            return v.clone();
        }
        let mut mods: Vec<(&Common, ExecContext)> = self
            .commons
            .values()
            .filter(|m| m.flags.global)
            .filter_map(|m| Some((m, reach(m.flags, ctx)?)))
            .collect();
        mods.sort_by(|a, b| a.0.name.cmp(&b.0.name));
        let mut index = HashMap::new();
        for (m, at) in mods {
            for s in self.exports_of(m, at).iter() {
                index.entry(fold_name(&s.name)).or_insert_with(|| s.clone());
            }
        }
        let v = Arc::new(index);
        if let Ok(mut cache) = self.global_index.lock() {
            cache.insert(ctx, v.clone());
        }
        v
    }

    fn template_members(&self, template: &str) -> Arc<Members> {
        if let Ok(cache) = self.templates.lock()
            && let Some(v) = cache.get(template)
        {
            return v.clone();
        }
        let prefix = format!("{template}.<");
        let exact = fold_name(template);
        let v = Arc::new(members_of(|t| {
            t.name_ru.starts_with(&prefix) || fold_name(&t.name_ru) == exact
        }));
        if let Ok(mut cache) = self.templates.lock() {
            cache.insert(template.to_string(), v.clone());
        }
        v
    }

    fn own_names(&self, id: ObjId, manager: bool) -> HashSet<String> {
        let def = self.model.obj(id);
        let md = self
            .md_index
            .get(&def.md)
            .and_then(|i| self.model.config.objects.get(*i));
        let mut out = HashSet::new();
        if manager {
            out.extend(def.enum_values.iter().map(|v| fold_name(&v.name)));
            if let Some(p) = md.and_then(|o| o.predefined()) {
                out.extend(p.iter().map(|i| fold_name(&i.name)));
            }
        } else {
            out.extend(
                def.fields
                    .iter()
                    .filter(|f| f.std.is_none() && f.role != FieldRole::Common)
                    .map(|f| fold_name(&f.name)),
            );
            out.extend(def.tabular.iter().map(|t| fold_name(&t.name)));
            if let Some(o) = md {
                out.extend(
                    o.children
                        .iter()
                        .filter(|c| c.tag == ChildTag::AccountingFlag)
                        .map(|c| fold_name(&c.name)),
                );
            }
        }
        out
    }

    fn resolve(&self, module: &str) -> Option<ModuleCtx> {
        let parts: Vec<&str> = module.split('.').collect();
        let [word, name, tail] = parts.as_slice() else {
            return None;
        };
        let ctx = module_word(&fold_name(tail))?;
        let kind = ObjKind::from_word(word).filter(|k| ru_word(*k).is_some())?;
        let ru = kind.name(Lang::Ru);
        let (template, common_attrs) = match ctx {
            CtxKind::Object if has_object_module(kind) => (format!("{ru}Объект"), true),
            CtxKind::Object => return None,
            CtxKind::RecordSet => (format!("{ru}НаборЗаписей"), false),
            CtxKind::Manager => (format!("{ru}Менеджер"), false),
            CtxKind::ValueManager if kind == ObjKind::Constant => {
                ("КонстантаМенеджерЗначения".to_string(), false)
            }
            CtxKind::ValueManager => return None,
        };
        let own = match self.model.find(kind, name) {
            Some(id) => self.own_names(id, ctx == CtxKind::Manager),
            None => HashSet::new(),
        };
        Some(ModuleCtx {
            kind: ctx,
            template: self.template_members(&template),
            own,
            common_attrs,
        })
    }

    fn ctx_of(&self, module: &str) -> Option<Arc<ModuleCtx>> {
        if let Ok(cache) = self.module_ctx.lock()
            && let Some(v) = cache.get(module)
        {
            return v.clone();
        }
        let v = self.resolve(module).map(Arc::new);
        if let Ok(mut cache) = self.module_ctx.lock() {
            cache.insert(module.to_string(), v.clone());
        }
        v
    }
}

impl AppNames for AppNamesImpl {
    fn global_value(&self, name: &str, ctx: ExecContext) -> Option<AppGlobal> {
        let m = self.commons.get(&fold_name(name))?;
        (!m.flags.global && reach(m.flags, ctx).is_some()).then(|| AppGlobal::CommonModule {
            name: m.name.clone(),
        })
    }

    fn global_method(&self, name: &str, ctx: ExecContext) -> Option<AppMethodSig> {
        self.global_index(ctx).get(&fold_name(name)).cloned()
    }

    fn context_value(&self, module: &str, name: &str) -> Option<ContextValue> {
        let c = self.ctx_of(module)?;
        let n = fold_name(name);
        if c.kind != CtxKind::Manager && matches!(n.as_str(), "этотобъект" | "thisobject")
        {
            return Some(ContextValue::ThisObject);
        }
        let has = c.template.props.contains(&n)
            || c.own.contains(&n)
            || (c.common_attrs && self.common_attrs.contains(&n));
        has.then_some(ContextValue::Member)
    }

    fn context_method(&self, module: &str, name: &str) -> bool {
        self.ctx_of(module)
            .is_some_and(|c| c.template.methods.contains(&fold_name(name)))
    }
}
