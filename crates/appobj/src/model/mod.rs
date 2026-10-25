mod behavior;
mod extra;
mod kinds;
mod stdattrs;
mod storage;
mod types;

pub use behavior::*;
pub use extra::*;
pub use kinds::{KINDS, ObjKind};
pub use stdattrs::{StdAttr, std_attrs};
pub use storage::{ModelStorage, ObjStorage, std_fields as storage_fields};
pub use types::{TypeAtom, TypeSet};

use bsl_compiler::{CommonFlags, Reuse};
use bsl_prim::fold_name;
use md_model::{
    Child, ChildTag, Configuration, LocalString, MdKind, MdObject, MdRef, ModuleKind, Value,
};
use platform_catalog::messages::Lang;
use std::collections::HashMap;
use std::sync::Arc;

pub type Uuid16 = brace::Guid;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct ObjId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldRole {
    Standard,
    Attribute,
    Dimension,
    Resource,
    AddressingAttribute,
    Common,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Usage {
    #[default]
    ForItem,
    ForFolder,
    ForFolderAndItem,
}

#[derive(Clone, Debug)]
pub struct FieldDef {
    pub md: Option<Uuid16>,
    pub name: String,
    pub synonym: LocalString,
    pub std: Option<StdAttr>,
    pub role: FieldRole,
    pub types: TypeSet,
    pub usage: Usage,
    pub fill_check: bool,
    pub indexed: bool,
}

#[derive(Clone, Debug)]
pub struct TabDef {
    pub std_list: Option<ibschema::storage::StdList>,
    pub md: Uuid16,
    pub name: String,
    pub synonym: LocalString,
    pub fields: Vec<FieldDef>,
}

#[derive(Clone, Debug)]
pub struct EnumValueDef {
    pub md: Uuid16,
    pub name: String,
    pub synonym: LocalString,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ObjModules {
    pub object: bool,
    pub manager: bool,
    pub record_set: bool,
    pub value_manager: bool,
}

#[derive(Clone, Debug)]
pub struct ObjDef {
    pub id: ObjId,
    pub kind: ObjKind,
    pub md: Uuid16,
    pub name: String,
    pub synonym: LocalString,
    pub comment: String,
    pub fields: Vec<FieldDef>,
    pub tabular: Vec<TabDef>,
    pub enum_values: Vec<EnumValueDef>,
    pub behavior: Behavior,
    pub generated: Vec<md_model::GeneratedType>,
    pub modules: ObjModules,
}

pub struct AppModel {
    pub config: Arc<Configuration>,
    pub script: md_model::ScriptVariant,
    pub lang_code: String,
    pub objects: Vec<ObjDef>,
    pub common_modules: Vec<CommonModuleDef>,
    pub session_params: Vec<SessionParamDef>,
    pub subscriptions: Vec<SubscriptionDef>,
    pub storage: Option<ModelStorage>,
    pub diagnostics: Vec<String>,
    by_uuid: HashMap<Uuid16, ObjId>,
    by_name: HashMap<(ObjKind, String), ObjId>,
    common_by_name: HashMap<String, usize>,
    param_by_name: HashMap<String, usize>,
    kind_ranges: Vec<std::ops::Range<usize>>,
    empty: ObjDef,
}

fn text(c: &[md_model::Prop], name: &str) -> Option<String> {
    c.iter()
        .find(|p| p.name == name)
        .and_then(|p| match &p.value {
            Value::Text(t) => Some(t.clone()),
            Value::Bool(b) => Some(b.to_string()),
            _ => None,
        })
}

struct Ctx<'a> {
    cfg: &'a Configuration,
    by_type_id: &'a HashMap<Uuid16, ObjId>,
    diag: Vec<String>,
}

impl Ctx<'_> {
    fn types(&mut self, td: Option<&md_model::TypeDescription>) -> TypeSet {
        let map = self.by_type_id;
        let lookup = |g: Uuid16| map.get(&g).copied();
        match td {
            Some(td) => TypeSet::of_diag(self.cfg, td, &lookup, &mut self.diag),
            None => TypeSet::default(),
        }
    }

    fn field(&mut self, c: &Child, role: FieldRole) -> FieldDef {
        FieldDef {
            md: Some(c.id),
            name: c.name.clone(),
            synonym: c.synonym.clone(),
            std: None,
            role,
            types: self.types(c.type_desc()),
            usage: text(&c.props, "Use").map_or(Usage::ForItem, |t| parse_usage(&t)),
            fill_check: text(&c.props, "FillChecking").is_some_and(|t| t == "ShowError"),
            indexed: text(&c.props, "Indexing").is_some_and(|t| t != "DontIndex"),
        }
    }
}

fn role_of(tag: ChildTag) -> Option<FieldRole> {
    match tag {
        ChildTag::Attribute => Some(FieldRole::Attribute),
        ChildTag::Dimension => Some(FieldRole::Dimension),
        ChildTag::Resource => Some(FieldRole::Resource),
        ChildTag::AddressingAttribute => Some(FieldRole::AddressingAttribute),
        _ => None,
    }
}

fn common_flags(o: &MdObject) -> CommonFlags {
    let b = |n: &str| o.prop(n).is_some_and(|v| matches!(v, Value::Bool(true)));
    CommonFlags {
        server: b("Server"),
        client_managed: b("ClientManagedApplication"),
        client_ordinary: b("ClientOrdinaryApplication"),
        external_connection: b("ExternalConnection"),
        server_call: b("ServerCall"),
        global: b("Global"),
        privileged: b("Privileged"),
        reuse: match o.prop("ReturnValuesReuse") {
            Some(Value::Text(t)) if t == "DuringRequest" => Reuse::DuringRequest,
            Some(Value::Text(t)) if t == "DuringSession" => Reuse::DuringSession,
            _ => Reuse::DontUse,
        },
    }
}

impl AppModel {
    pub fn build(config: Arc<Configuration>) -> crate::error::Result<AppModel> {
        let cfg: &Configuration = &config;
        let script = cfg.script_variant();
        let lock = cfg.data_lock_control_mode();
        let mut diagnostics = Vec::new();

        let mut src: Vec<(ObjKind, &MdObject)> = Vec::new();
        let mut kind_ranges = Vec::with_capacity(KINDS.len());
        for k in KINDS {
            let start = src.len();
            src.extend(
                cfg.objects
                    .iter()
                    .filter(|o| ObjKind::of_md(o.kind) == Some(k))
                    .map(|o| (k, o)),
            );
            kind_ranges.push(start..src.len());
        }
        let mut by_uuid = HashMap::new();
        let mut by_name = HashMap::new();
        let mut by_type_id = HashMap::new();
        for (i, (k, o)) in src.iter().enumerate() {
            let id = ObjId(i as u32);
            if *by_uuid.entry(o.id).or_insert(id) != id {
                diagnostics.push(format!("duplicate uuid of {} {}", k.name(Lang::En), o.name));
            }
            let key = (*k, fold_name(&o.name));
            if *by_name.entry(key).or_insert(id) != id {
                diagnostics.push(format!("duplicate name: {}.{}", k.name(Lang::En), o.name));
            }
            for g in o.generated_types.iter().filter(|g| g.category == "Ref") {
                by_type_id.entry(g.type_id).or_insert(id);
            }
        }

        let resolve = |r: &MdRef| -> Option<Uuid16> {
            if let Some(id) = r.id {
                return Some(id);
            }
            let (w, n) = r.name.split_once('.')?;
            let k = ObjKind::from_word(w)?;
            let id = by_name.get(&(k, fold_name(n)))?;
            src.get(id.0 as usize).map(|(_, o)| o.id)
        };

        let mut cx = Ctx {
            cfg,
            by_type_id: &by_type_id,
            diag: Vec::new(),
        };
        let mut objects = Vec::with_capacity(src.len());
        for (i, (kind, o)) in src.iter().enumerate() {
            let id = ObjId(i as u32);
            let behavior = behavior::parse(*kind, o, lock, &resolve);
            let owners: Vec<ObjId> = behavior
                .owners
                .iter()
                .filter_map(|u| by_uuid.get(u).copied())
                .collect();
            let sx = stdattrs::StdCtx {
                kind: *kind,
                me: id,
                owners: &owners,
                script,
            };
            let mut fields = stdattrs::std_fields(o, &behavior, &sx);
            let mut tabular = Vec::new();
            let mut enum_values = Vec::new();
            for c in &o.children {
                match c.tag {
                    ChildTag::TabularSection => {
                        let len = text(&c.props, "LineNumberLength")
                            .and_then(|t| t.parse().ok())
                            .unwrap_or(9);
                        let mut tf = vec![stdattrs::line_number_field(script, len)];
                        for a in c.children.iter().filter(|a| a.tag == ChildTag::Attribute) {
                            tf.push(cx.field(a, FieldRole::Attribute));
                        }
                        tabular.push(TabDef {
                            std_list: None,
                            md: c.id,
                            name: c.name.clone(),
                            synonym: c.synonym.clone(),
                            fields: tf,
                        });
                    }
                    ChildTag::EnumValue => enum_values.push(EnumValueDef {
                        md: c.id,
                        name: c.name.clone(),
                        synonym: c.synonym.clone(),
                    }),
                    t => {
                        if let Some(role) = role_of(t) {
                            fields.push(cx.field(c, role));
                        }
                    }
                }
            }
            let has = |k| o.module(k).is_some();
            objects.push(ObjDef {
                id,
                kind: *kind,
                md: o.id,
                name: o.name.clone(),
                synonym: o.synonym.clone(),
                comment: o.comment.clone(),
                fields,
                tabular,
                enum_values,
                behavior,
                generated: o.generated_types.clone(),
                modules: ObjModules {
                    object: has(ModuleKind::ObjectModule),
                    manager: has(ModuleKind::ManagerModule),
                    record_set: has(ModuleKind::RecordSetModule),
                    value_manager: has(ModuleKind::ValueManagerModule),
                },
            });
        }

        for (_, s) in src.iter().filter(|(k, _)| *k == ObjKind::Sequence) {
            if let Some(Value::RefList(docs)) = s.prop("Documents") {
                for d in docs {
                    let Some(did) = resolve(d).and_then(|u| by_uuid.get(&u).copied()) else {
                        continue;
                    };
                    if let Some(doc) = objects
                        .get_mut(did.0 as usize)
                        .filter(|d| d.kind == ObjKind::Document)
                    {
                        doc.behavior.sequences.push(s.id);
                    }
                }
            }
        }

        let mut common_modules = Vec::new();
        let mut session_params = Vec::new();
        let mut subscriptions = Vec::new();
        let mut common_by_name = HashMap::new();
        let mut param_by_name = HashMap::new();
        for o in &cfg.objects {
            match o.kind {
                MdKind::CommonModule => {
                    let has_text = o
                        .module(ModuleKind::Module)
                        .is_some_and(|m| !m.text().trim().is_empty());
                    match common_by_name.entry(fold_name(&o.name)) {
                        std::collections::hash_map::Entry::Occupied(_) => {
                            diagnostics.push(format!("duplicate name: CommonModule.{}", o.name));
                        }
                        std::collections::hash_map::Entry::Vacant(v) => {
                            v.insert(common_modules.len());
                        }
                    }
                    common_modules.push(CommonModuleDef {
                        md: o.id,
                        name: o.name.clone(),
                        flags: common_flags(o),
                        has_text,
                    });
                }
                MdKind::SessionParameter => {
                    let td = match o.prop("Type") {
                        Some(Value::Type(t)) => Some(t),
                        _ => None,
                    };
                    match param_by_name.entry(fold_name(&o.name)) {
                        std::collections::hash_map::Entry::Occupied(_) => {
                            diagnostics
                                .push(format!("duplicate name: SessionParameter.{}", o.name));
                        }
                        std::collections::hash_map::Entry::Vacant(v) => {
                            v.insert(session_params.len());
                        }
                    }
                    let types = cx.types(td);
                    session_params.push(SessionParamDef {
                        md: o.id,
                        name: o.name.clone(),
                        synonym: o.synonym.clone(),
                        types,
                    });
                }
                MdKind::EventSubscription => {
                    let td = match o.prop("Source") {
                        Some(Value::Type(t)) => Some(t),
                        _ => None,
                    };
                    let source = cx.types(td);
                    let full = text(&o.props, "Handler").unwrap_or_default();
                    let parts: Vec<&str> = full.split('.').collect();
                    let handler = match parts.as_slice() {
                        [_, m, f] => (m.to_string(), f.to_string()),
                        _ => {
                            cx.diag.push(format!(
                                "event subscription {}: unusual handler {full:?}",
                                o.name
                            ));
                            (String::new(), full.clone())
                        }
                    };
                    subscriptions.push(SubscriptionDef {
                        md: o.id,
                        name: o.name.clone(),
                        source,
                        event: text(&o.props, "Event").unwrap_or_default(),
                        handler,
                    });
                }
                _ => {}
            }
        }
        diagnostics.extend(cx.diag);
        diagnostics.sort();
        diagnostics.dedup();

        let empty = ObjDef {
            id: ObjId(u32::MAX),
            kind: ObjKind::Catalog,
            md: Uuid16::NIL,
            name: String::new(),
            synonym: LocalString::default(),
            comment: String::new(),
            fields: Vec::new(),
            tabular: Vec::new(),
            enum_values: Vec::new(),
            behavior: Behavior::default(),
            generated: Vec::new(),
            modules: ObjModules::default(),
        };
        let lang_code = config
            .default_language()
            .and_then(|l| l.props.iter().find(|p| p.name == "LanguageCode"))
            .and_then(|p| match &p.value {
                Value::Text(t) if !t.is_empty() => Some(t.clone()),
                _ => None,
            })
            .unwrap_or_else(|| "ru".to_string());
        Ok(AppModel {
            config,
            script,
            lang_code,
            objects,
            common_modules,
            session_params,
            subscriptions,
            storage: None,
            diagnostics,
            by_uuid,
            by_name,
            common_by_name,
            param_by_name,
            kind_ranges,
            empty,
        })
    }

    pub fn obj(&self, id: ObjId) -> &ObjDef {
        self.objects.get(id.0 as usize).unwrap_or(&self.empty)
    }

    pub fn by_uuid(&self, md: Uuid16) -> Option<ObjId> {
        self.by_uuid.get(&md).copied()
    }

    pub fn find(&self, kind: ObjKind, name: &str) -> Option<ObjId> {
        self.by_name.get(&(kind, fold_name(name))).copied()
    }

    pub fn of_kind(&self, kind: ObjKind) -> impl Iterator<Item = &ObjDef> {
        self.kind_ranges
            .get(kind.index())
            .and_then(|r| self.objects.get(r.clone()))
            .unwrap_or(&[])
            .iter()
    }

    pub fn common_module(&self, name: &str) -> Option<&CommonModuleDef> {
        self.common_by_name
            .get(&fold_name(name))
            .and_then(|i| self.common_modules.get(*i))
    }

    pub fn session_param(&self, name: &str) -> Option<(usize, &SessionParamDef)> {
        let i = *self.param_by_name.get(&fold_name(name))?;
        self.session_params.get(i).map(|p| (i, p))
    }

    pub fn find_full_name(&self, full: &str) -> Option<MdPath> {
        let seg: Vec<&str> = full.split('.').collect();
        let word = fold_name(seg.first()?);
        match (word.as_str(), seg.len()) {
            ("конфигурация" | "configuration", 1) => {
                return Some(MdPath::Configuration);
            }
            ("общиймодуль" | "commonmodule", 2) => {
                return self
                    .common_by_name
                    .get(&fold_name(seg.get(1)?))
                    .map(|i| MdPath::CommonModule(*i));
            }
            ("параметрсеанса" | "sessionparameter", 2) => {
                return self
                    .session_param(seg.get(1)?)
                    .map(|(i, _)| MdPath::SessionParam(i));
            }
            ("подписканасобытие" | "eventsubscription", 2) => {
                let n = fold_name(seg.get(1)?);
                return self
                    .subscriptions
                    .iter()
                    .position(|s| fold_name(&s.name) == n)
                    .map(MdPath::Subscription);
            }
            _ => {}
        }
        let kind = ObjKind::from_word(seg.first()?)?;
        let id = self.find(kind, seg.get(1)?)?;
        let o = self.obj(id);
        let member = |fields: &[FieldDef], role: &[FieldRole], name: &str| {
            let n = fold_name(name);
            fields
                .iter()
                .position(|f| f.std.is_none() && role.contains(&f.role) && fold_name(&f.name) == n)
        };
        let roles = |w: &str| -> Option<&'static [FieldRole]> {
            match fold_name(w).as_str() {
                "реквизит" | "attribute" => Some(&[FieldRole::Attribute]),
                "измерение" | "dimension" => Some(&[FieldRole::Dimension]),
                "ресурс" | "resource" => Some(&[FieldRole::Resource]),
                "реквизитадресации" | "addressingattribute" => {
                    Some(&[FieldRole::AddressingAttribute])
                }
                _ => None,
            }
        };
        let is_tab = |w: &str| matches!(fold_name(w).as_str(), "табличнаячасть" | "tabularsection");
        match seg.len() {
            2 => Some(MdPath::Object(id)),
            4 => {
                let (w, n) = (seg.get(2)?, seg.get(3)?);
                if let Some(r) = roles(w) {
                    member(&o.fields, r, n).map(|i| MdPath::Field(id, None, i))
                } else if is_tab(w) {
                    let nn = fold_name(n);
                    o.tabular
                        .iter()
                        .position(|t| fold_name(&t.name) == nn)
                        .map(|i| MdPath::Tabular(id, i))
                } else if matches!(fold_name(w).as_str(), "форма" | "form") {
                    let nn = fold_name(n);
                    self.config
                        .objects
                        .iter()
                        .find(|m| m.id == o.md)?
                        .forms
                        .iter()
                        .position(|f| fold_name(&f.name) == nn)
                        .map(|i| MdPath::Form(id, i))
                } else if matches!(fold_name(w).as_str(), "значениеперечисления" | "enumvalue")
                {
                    let nn = fold_name(n);
                    o.enum_values
                        .iter()
                        .position(|v| fold_name(&v.name) == nn)
                        .map(|i| MdPath::EnumValue(id, i))
                } else {
                    None
                }
            }
            6 if is_tab(seg.get(2)?) => {
                let nn = fold_name(seg.get(3)?);
                let ti = o.tabular.iter().position(|t| fold_name(&t.name) == nn)?;
                let r = roles(seg.get(4)?)?;
                let t = o.tabular.get(ti)?;
                member(&t.fields, r, seg.get(5)?).map(|i| MdPath::Field(id, Some(ti), i))
            }
            _ => None,
        }
    }
}
