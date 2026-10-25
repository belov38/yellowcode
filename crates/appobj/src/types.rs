use crate::errors;
use crate::model::{AppModel, KINDS, ObjDef, ObjId, ObjKind, Uuid16};
use bsl_prim::fold_name;
use bsl_vm::{AppTypeInfo, Lang, Registry, TypeId, XdtoName};
use md_model::{ChildTag, LocalString, MdObject};
use platform_catalog::TypeIdx;
use std::collections::{HashMap, HashSet};

const XDTO_NAMESPACE: &str = "http://v8.1c.ru/8.1/data/enterprise/current-config";

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Category {
    Ref,
    Object,
    Manager,
    Selection,
    List,
    TabularSection,
    TabularSectionRow,
    RecordSet,
    Record,
    RecordManager,
    RecordKey,
    ValueManager,
    ValueKey,
    ExtDimensions,
    ExtDimensionTypes,
    ExtDimensionTypesRow,
    DisplacingCalculationTypes,
    DisplacingCalculationTypesRow,
    BaseCalculationTypes,
    BaseCalculationTypesRow,
    LeadingCalculationTypes,
    LeadingCalculationTypesRow,
    Recalcs,
    RoutePointRef,
}

#[derive(Clone, Copy)]
enum Affix {
    Kind(&'static str),
    Literal(&'static str),
}

impl Affix {
    fn stem(self, kind: ObjKind, lang: Lang) -> String {
        match self {
            Affix::Kind(s) => format!("{}{s}", kind.name(lang)),
            Affix::Literal(s) => s.to_string(),
        }
    }
}

const CATEGORIES: [(Category, &str, Affix, Affix); 24] = [
    (
        Category::Ref,
        "Ref",
        Affix::Kind("Ссылка"),
        Affix::Kind("Ref"),
    ),
    (
        Category::Object,
        "Object",
        Affix::Kind("Объект"),
        Affix::Kind("Object"),
    ),
    (
        Category::Manager,
        "Manager",
        Affix::Kind("Менеджер"),
        Affix::Kind("Manager"),
    ),
    (
        Category::Selection,
        "Selection",
        Affix::Kind("Выборка"),
        Affix::Kind("Selection"),
    ),
    (
        Category::List,
        "List",
        Affix::Kind("Список"),
        Affix::Kind("List"),
    ),
    (
        Category::TabularSection,
        "TabularSection",
        Affix::Kind("ТабличнаяЧасть"),
        Affix::Kind("TabularSection"),
    ),
    (
        Category::TabularSectionRow,
        "TabularSectionRow",
        Affix::Kind("ТабличнаяЧастьСтрока"),
        Affix::Kind("TabularSectionRow"),
    ),
    (
        Category::RecordSet,
        "RecordSet",
        Affix::Kind("НаборЗаписей"),
        Affix::Kind("RecordSet"),
    ),
    (
        Category::Record,
        "Record",
        Affix::Kind("Запись"),
        Affix::Kind("Record"),
    ),
    (
        Category::RecordManager,
        "RecordManager",
        Affix::Kind("МенеджерЗаписи"),
        Affix::Kind("RecordManager"),
    ),
    (
        Category::RecordKey,
        "RecordKey",
        Affix::Kind("КлючЗаписи"),
        Affix::Kind("RecordKey"),
    ),
    (
        Category::ValueManager,
        "ValueManager",
        Affix::Kind("МенеджерЗначения"),
        Affix::Kind("ValueManager"),
    ),
    (
        Category::ValueKey,
        "ValueKey",
        Affix::Kind("КлючЗначения"),
        Affix::Kind("ValueKey"),
    ),
    (
        Category::ExtDimensions,
        "ExtDimensions",
        Affix::Kind("Субконто"),
        Affix::Kind("ExtDimensions"),
    ),
    (
        Category::ExtDimensionTypes,
        "ExtDimensionTypes",
        Affix::Kind("ВидыСубконто"),
        Affix::Kind("ExtDimensionTypes"),
    ),
    (
        Category::ExtDimensionTypesRow,
        "ExtDimensionTypesRow",
        Affix::Kind("ВидыСубконтоСтрока"),
        Affix::Kind("ExtDimensionTypesRow"),
    ),
    (
        Category::DisplacingCalculationTypes,
        "DisplacingCalculationTypes",
        Affix::Literal("ВытесняющиеВидыРасчета"),
        Affix::Literal("DisplacingCalculationTypes"),
    ),
    (
        Category::DisplacingCalculationTypesRow,
        "DisplacingCalculationTypesRow",
        Affix::Literal("ВытесняющиеВидыРасчетаСтрока"),
        Affix::Literal("DisplacingCalculationTypesRow"),
    ),
    (
        Category::BaseCalculationTypes,
        "BaseCalculationTypes",
        Affix::Literal("БазовыеВидыРасчета"),
        Affix::Literal("BaseCalculationTypes"),
    ),
    (
        Category::BaseCalculationTypesRow,
        "BaseCalculationTypesRow",
        Affix::Literal("БазовыеВидыРасчетаСтрока"),
        Affix::Literal("BaseCalculationTypesRow"),
    ),
    (
        Category::LeadingCalculationTypes,
        "LeadingCalculationTypes",
        Affix::Literal("ВедущиеВидыРасчета"),
        Affix::Literal("LeadingCalculationTypes"),
    ),
    (
        Category::LeadingCalculationTypesRow,
        "LeadingCalculationTypesRow",
        Affix::Literal("ВедущиеВидыРасчетаСтрока"),
        Affix::Literal("LeadingCalculationTypesRow"),
    ),
    (
        Category::Recalcs,
        "Recalcs",
        Affix::Kind("ПерерасчетыМенеджер"),
        Affix::Kind("RecalcsManager"),
    ),
    (
        Category::RoutePointRef,
        "RoutePointRef",
        Affix::Literal("ТочкаМаршрутаБизнесПроцессаСсылка"),
        Affix::Kind("RoutePointRef"),
    ),
];

impl Category {
    pub fn of_mdconf(category: &str) -> Option<Category> {
        CATEGORIES.iter().find(|r| r.1 == category).map(|r| r.0)
    }

    pub fn mdconf_name(self) -> &'static str {
        CATEGORIES.iter().find(|r| r.0 == self).map_or("", |r| r.1)
    }

    fn row(self) -> Option<&'static (Category, &'static str, Affix, Affix)> {
        CATEGORIES.iter().find(|r| r.0 == self)
    }

    fn stem(self, kind: ObjKind, lang: Lang) -> String {
        match (self.row(), lang) {
            (Some(r), Lang::Ru) => r.2.stem(kind, lang),
            (Some(r), Lang::En) => r.3.stem(kind, lang),
            (None, _) => String::new(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Origin {
    obj: ObjId,
    cat: Category,
    tab: Option<usize>,
}

#[derive(Debug, Default)]
pub struct AppTypes {
    by_obj: HashMap<(ObjId, Category), TypeId>,
    by_tab: HashMap<(ObjId, usize, bool), TypeId>,
    origin: HashMap<TypeId, Origin>,
    managers: HashMap<ObjKind, TypeId>,
    xdto: HashMap<TypeId, XdtoName>,
    pub diagnostics: Vec<String>,
}

impl AppTypes {
    pub(crate) fn same_ids(&self, other: &AppTypes) -> bool {
        self.by_obj == other.by_obj
            && self.by_tab == other.by_tab
            && self.managers == other.managers
    }
}

fn templates(catalog: &platform_catalog::Catalog) -> HashMap<String, TypeIdx> {
    let mut map = HashMap::new();
    for (i, t) in catalog.types().iter().enumerate() {
        if !t.metadata_template {
            continue;
        }
        let Some((stem, _)) = t.name_en.split_once(".<") else {
            continue;
        };
        if let Ok(i) = u16::try_from(i) {
            map.entry(fold_name(stem)).or_insert(TypeIdx(i));
        }
    }
    map
}

struct Registrar<'a> {
    reg: &'a mut Registry,
    seen: HashSet<String>,
    out: AppTypes,
}

impl Registrar<'_> {
    fn add(&mut self, info: AppTypeInfo, origin: Origin) -> Option<TypeId> {
        let (ru, en) = (fold_name(&info.name_ru), fold_name(&info.name_en));
        if self.seen.contains(&ru) || self.seen.contains(&en) {
            self.out
                .diagnostics
                .push(format!("duplicate applied type name: {}", info.name_ru));
            return None;
        }
        self.seen.insert(ru);
        self.seen.insert(en);
        let xdto = info.xdto.clone();
        let id = self.reg.register_app_type(info);
        if let Some(x) = xdto {
            self.out.xdto.insert(id, x);
        }
        self.out.origin.insert(id, origin);
        Some(id)
    }
}

pub(crate) fn synonym(s: &LocalString, name: &str, code: &str) -> String {
    s.0.iter()
        .find(|(l, t)| l == code && !t.is_empty())
        .map_or_else(|| name.to_string(), |(_, t)| t.clone())
}

fn present(def: &ObjDef, code: &str, cat: Category, tab: Option<usize>, lang: Lang) -> String {
    if let Some(list) = tab
        .and_then(|i| def.tabular.get(i))
        .and_then(|t| t.std_list)
    {
        let category = if cat == Category::TabularSectionRow {
            format!("{}Row", list_names(list).1)
        } else {
            list_names(list).1.to_string()
        };
        if let Some(m) =
            errors::type_presentation_key(def.kind.name(Lang::En), &category).and_then(errors::msg)
        {
            let prefix = match lang {
                Lang::Ru => m.ru,
                Lang::En => m.en,
            };
            return format!("{}: {}", prefix, synonym(&def.synonym, &def.name, code));
        }
    }
    let (syn, name) = match tab.and_then(|i| def.tabular.get(i)) {
        Some(t) => (&t.synonym, &t.name),
        None => (&def.synonym, &def.name),
    };
    let name = synonym(syn, name, code);
    if cat == Category::Ref {
        return name;
    }
    if cat == Category::RoutePointRef {
        return match lang {
            Lang::Ru => format!("{name}(точка маршрута)"),
            Lang::En => format!("{name}(route point)"),
        };
    }
    let prefix = errors::type_presentation_key(def.kind.name(Lang::En), cat.mdconf_name())
        .and_then(errors::msg)
        .map(|m| match lang {
            Lang::Ru => m.ru,
            Lang::En => m.en,
        });
    match prefix {
        Some(p) => format!("{p}: {name}"),
        None => name,
    }
}

pub fn presentation(model: &AppModel, obj: ObjId, cat: Category, lang: Lang) -> String {
    let tab = matches!(cat, Category::TabularSection | Category::TabularSectionRow).then_some(0);
    present(model.obj(obj), &model.lang_code, cat, tab, lang)
}

pub fn tabular_presentation(
    model: &AppModel,
    obj: ObjId,
    tab: usize,
    row: bool,
    lang: Lang,
) -> String {
    let cat = if row {
        Category::TabularSectionRow
    } else {
        Category::TabularSection
    };
    present(model.obj(obj), &model.lang_code, cat, Some(tab), lang)
}

fn info(
    stems: (String, String),
    obj_path: &str,
    pres: (String, String),
    guid: [u8; 16],
    template: Option<TypeIdx>,
    with_xdto: bool,
) -> AppTypeInfo {
    let (ru, en) = stems;
    AppTypeInfo {
        name_ru: format!("{ru}.{obj_path}"),
        name_en: format!("{en}.{obj_path}"),
        presentation_ru: pres.0,
        presentation_en: pres.1,
        guid,
        xdto: with_xdto.then(|| XdtoName {
            namespace: XDTO_NAMESPACE.to_string(),
            name: format!("{en}.{obj_path}"),
        }),
        template,
        default_value: None,
    }
}

impl AppTypes {
    pub fn register(model: &AppModel, reg: &mut Registry) -> crate::Result<AppTypes> {
        AppTypes::register_with(model, reg, None)
    }

    pub fn register_with(
        model: &AppModel,
        reg: &mut Registry,
        app: Option<std::sync::Weak<crate::Application>>,
    ) -> crate::Result<AppTypes> {
        let catalog = platform_catalog::catalog();
        let tpl = templates(catalog);
        let mds: HashMap<Uuid16, &MdObject> = model
            .objects
            .iter()
            .filter(|o| !o.tabular.is_empty())
            .filter_map(|o| {
                model
                    .config
                    .objects
                    .iter()
                    .find(|m| m.id == o.md)
                    .map(|m| (o.md, m))
            })
            .collect();
        let mut r = Registrar {
            reg,
            seen: HashSet::new(),
            out: AppTypes::default(),
        };
        for def in &model.objects {
            for g in &def.generated {
                let Some(cat) = Category::of_mdconf(&g.category) else {
                    continue;
                };
                if r.out.by_obj.contains_key(&(def.id, cat)) {
                    continue;
                }
                let template = tpl.get(&fold_name(&cat.stem(def.kind, Lang::En))).copied();
                let (ru, en) = match template {
                    Some(t) => {
                        let ti = catalog.type_info(t);
                        (
                            ti.name_ru
                                .split_once(".<")
                                .map_or(ti.name_ru.as_str(), |s| s.0),
                            ti.name_en
                                .split_once(".<")
                                .map_or(ti.name_en.as_str(), |s| s.0),
                        )
                    }
                    None => ("", ""),
                };
                let stems = if template.is_some() {
                    (ru.to_string(), en.to_string())
                } else {
                    (cat.stem(def.kind, Lang::Ru), cat.stem(def.kind, Lang::En))
                };
                let pres = (
                    present(def, &model.lang_code, cat, None, Lang::Ru),
                    present(def, &model.lang_code, cat, None, Lang::En),
                );
                let mut ty = info(
                    stems,
                    &def.name,
                    pres,
                    *g.type_id.as_bytes(),
                    template,
                    template.is_some_and(|t| catalog.type_info(t).xdto.is_some()),
                );
                if let (Category::Ref, Some(app)) = (cat, &app) {
                    ty.default_value = Some(crate::values::refs::shared_empty(app.clone(), def.id));
                }
                let origin = Origin {
                    obj: def.id,
                    cat,
                    tab: None,
                };
                if let Some(id) = r.add(ty, origin) {
                    r.out.by_obj.insert((def.id, cat), id);
                }
            }
            r.tabular(def, mds.get(&def.md).copied(), &model.lang_code);
        }
        for kind in KINDS {
            let name = format!("{}Менеджер", kind.collection(Lang::Ru));
            if let Some(t) = catalog.find_type(&name) {
                r.out.managers.insert(kind, TypeId::of_catalog(t));
            }
        }
        Ok(r.out)
    }

    pub fn type_of(&self, obj: ObjId, cat: Category) -> Option<TypeId> {
        self.by_obj.get(&(obj, cat)).copied()
    }

    pub fn xdto_name(&self, t: TypeId) -> Option<XdtoName> {
        self.xdto.get(&t).cloned()
    }

    pub fn tabular_type(&self, obj: ObjId, tab: usize, row: bool) -> Option<TypeId> {
        self.by_tab.get(&(obj, tab, row)).copied()
    }

    pub fn collection_manager(&self, kind: ObjKind) -> Option<TypeId> {
        self.managers.get(&kind).copied()
    }

    pub fn describe(&self, t: TypeId) -> Option<(ObjId, Category)> {
        self.origin.get(&t).map(|o| (o.obj, o.cat))
    }

    pub fn describe_tabular(&self, t: TypeId) -> Option<(ObjId, usize, bool)> {
        let o = self.origin.get(&t)?;
        Some((o.obj, o.tab?, o.cat == Category::TabularSectionRow))
    }
}

impl Registrar<'_> {
    fn tabular(&mut self, def: &ObjDef, md: Option<&MdObject>, code: &str) {
        for (i, tab) in def.tabular.iter().enumerate() {
            let child = md.and_then(|m| {
                m.children
                    .iter()
                    .find(|c| c.tag == ChildTag::TabularSection && c.id == tab.md)
            });
            for row in [false, true] {
                if let Some(list) = tab.std_list {
                    let category = match (list, row) {
                        (ibschema::storage::StdList::LeadingCalculationTypes, false) => {
                            Category::LeadingCalculationTypes
                        }
                        (ibschema::storage::StdList::LeadingCalculationTypes, true) => {
                            Category::LeadingCalculationTypesRow
                        }
                        (ibschema::storage::StdList::DisplacingCalculationTypes, false) => {
                            Category::DisplacingCalculationTypes
                        }
                        (ibschema::storage::StdList::DisplacingCalculationTypes, true) => {
                            Category::DisplacingCalculationTypesRow
                        }
                        (ibschema::storage::StdList::BaseCalculationTypes, false) => {
                            Category::BaseCalculationTypes
                        }
                        (ibschema::storage::StdList::BaseCalculationTypes, true) => {
                            Category::BaseCalculationTypesRow
                        }
                        _ => continue,
                    };
                    if let Some(id) = self.out.by_obj.get(&(def.id, category)).copied() {
                        self.out.by_tab.insert((def.id, i, row), id);
                        continue;
                    }
                }
                let cat = if row {
                    Category::TabularSectionRow
                } else {
                    Category::TabularSection
                };
                let guid = child
                    .and_then(|c| {
                        c.generated_types
                            .iter()
                            .find(|g| g.category == cat.mdconf_name())
                    })
                    .map_or([0; 16], |g| *g.type_id.as_bytes());
                let pres = (
                    present(def, code, cat, Some(i), Lang::Ru),
                    present(def, code, cat, Some(i), Lang::En),
                );
                let (stems, path) = match tab.std_list {
                    Some(list) => {
                        let (ru, en) = list_names(list);
                        let suffix = if row { "Строка" } else { "" };
                        let en_suffix = if row { "Row" } else { "" };
                        (
                            (format!("{ru}{suffix}"), format!("{en}{en_suffix}")),
                            def.name.clone(),
                        )
                    }
                    None => (
                        (cat.stem(def.kind, Lang::Ru), cat.stem(def.kind, Lang::En)),
                        format!("{}.{}", def.name, tab.name),
                    ),
                };
                let ty = info(stems, &path, pres, guid, None, false);
                let origin = Origin {
                    obj: def.id,
                    cat,
                    tab: Some(i),
                };
                if let Some(id) = self.add(ty, origin) {
                    self.out.by_tab.insert((def.id, i, row), id);
                }
            }
        }
    }
}

fn list_names(list: ibschema::storage::StdList) -> (&'static str, &'static str) {
    use ibschema::storage::StdList::*;
    match list {
        LeadingCalculationTypes => ("ВедущиеВидыРасчета", "LeadingCalculationTypes"),
        DisplacingCalculationTypes => ("ВытесняющиеВидыРасчета", "DisplacingCalculationTypes"),
        BaseCalculationTypes => ("БазовыеВидыРасчета", "BaseCalculationTypes"),
        ExtDimensionTypes => ("ВидыСубконто", "ExtDimensionTypes"),
    }
}
