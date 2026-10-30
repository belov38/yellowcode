use super::resources::{self, Graph, GuiResourceError};
use memory_budget::{BudgetError, Category, MemoryBudget};

use bsl_prim::fold_name;
use mdconf::{ChildTag, MdKind, SubKind};
use platform_catalog::catalog;

use super::out::Lang;

mod managed;
pub use managed::ManagedNames;
pub(crate) use managed::{eq_path, folded_path};

const PREFIX_EN: &str = "MetadataObject: ";
const PREFIX_RU: &str = "ОбъектМетаданных: ";

pub const CHILD_TAGS: [ChildTag; 16] = [
    ChildTag::Attribute,
    ChildTag::TabularSection,
    ChildTag::Dimension,
    ChildTag::Resource,
    ChildTag::EnumValue,
    ChildTag::Command,
    ChildTag::AddressingAttribute,
    ChildTag::AccountingFlag,
    ChildTag::ExtDimensionAccountingFlag,
    ChildTag::Column,
    ChildTag::URLTemplate,
    ChildTag::Method,
    ChildTag::Operation,
    ChildTag::Parameter,
    ChildTag::IntegrationServiceChannel,
    ChildTag::Function,
];

pub fn child_tag_en(tag: ChildTag) -> &'static str {
    match tag {
        ChildTag::Attribute => "Attribute",
        ChildTag::TabularSection => "TabularSection",
        ChildTag::Dimension => "Dimension",
        ChildTag::Resource => "Resource",
        ChildTag::EnumValue => "EnumValue",
        ChildTag::Command => "Command",
        ChildTag::AddressingAttribute => "AddressingAttribute",
        ChildTag::AccountingFlag => "AccountingFlag",
        ChildTag::ExtDimensionAccountingFlag => "ExtDimensionAccountingFlag",
        ChildTag::Column => "Column",
        ChildTag::URLTemplate => "URLTemplate",
        ChildTag::Method => "Method",
        ChildTag::Operation => "Operation",
        ChildTag::Parameter => "Parameter",
        ChildTag::IntegrationServiceChannel => "IntegrationServiceChannel",
        ChildTag::Function => "Function",
    }
}

pub fn sub_kind_en(kind: SubKind) -> &'static str {
    match kind {
        SubKind::Form => "Form",
        SubKind::Template => "Template",
    }
}

fn type_name_ru(en: &str) -> Option<String> {
    let cat = catalog();
    let idx = cat.find_type(&format!("{PREFIX_EN}{en}"))?;
    let ru = &cat.type_info(idx).name_ru;
    Some(ru.strip_prefix(PREFIX_RU).unwrap_or(ru).to_string())
}

const KIND_RU_FALLBACK: &[(&str, &str)] = &[
    ("CommonForm", "ОбщаяФорма"),
    ("CommonTemplate", "ОбщийМакет"),
    ("Configuration", "Конфигурация"),
];

fn kind_ru(kind: MdKind) -> Option<String> {
    let en = kind.as_str();
    type_name_ru(en).or_else(|| {
        KIND_RU_FALLBACK
            .iter()
            .find(|(k, _)| *k == en)
            .map(|(_, ru)| (*ru).to_string())
    })
}

fn by_lang(en: &str, lang: Lang) -> String {
    match lang {
        Lang::En => en.to_string(),
        Lang::Ru => type_name_ru(en).unwrap_or_else(|| en.to_string()),
    }
}

pub fn kind_name(kind: MdKind, lang: Lang) -> String {
    match lang {
        Lang::En => kind.as_str().to_string(),
        Lang::Ru => kind_ru(kind).unwrap_or_else(|| kind.as_str().to_string()),
    }
}

pub fn section_name(kind: MdKind, lang: Lang) -> String {
    let Some(folder) = kind.folder() else {
        return kind.as_str().to_string();
    };
    if lang == Lang::En {
        return folder.to_string();
    }
    let cat = catalog();
    cat.find_type("ConfigurationMetadataObject")
        .and_then(|t| cat.find_property(t, folder))
        .map(|p| p.name_ru.clone())
        .unwrap_or_else(|| folder.to_string())
}

pub fn child_tag_name(tag: ChildTag, lang: Lang) -> String {
    by_lang(child_tag_en(tag), lang)
}

pub fn sub_kind_name(kind: SubKind, lang: Lang) -> String {
    by_lang(sub_kind_en(kind), lang)
}

pub fn property_name(kind: MdKind, xml: &str, lang: Lang) -> String {
    property_name_of(kind.as_str(), xml, lang)
}

pub fn property_name_of(type_en: &str, xml: &str, lang: Lang) -> String {
    let cat = catalog();
    let ty = cat.find_type(&format!("{PREFIX_EN}{type_en}")).or_else(|| {
        (type_en == "Configuration")
            .then(|| cat.find_type("ConfigurationMetadataObject"))
            .flatten()
    });
    ty.and_then(|t| cat.find_property(t, xml))
        .map(|p| match lang {
            Lang::Ru => p.name_ru.clone(),
            Lang::En => p.name_en.clone(),
        })
        .unwrap_or_else(|| xml.to_string())
}

const GROUPS: &[(&str, &str, &str)] = &[
    ("Attribute", "Реквизиты", "Attributes"),
    ("TabularSection", "ТабличныеЧасти", "TabularSections"),
    ("Dimension", "Измерения", "Dimensions"),
    ("Resource", "Ресурсы", "Resources"),
    ("EnumValue", "Значения", "Values"),
    (
        "AddressingAttribute",
        "РеквизитыАдресации",
        "AddressingAttributes",
    ),
    ("AccountingFlag", "ПризнакиУчета", "AccountingFlags"),
    (
        "ExtDimensionAccountingFlag",
        "ПризнакиУчетаСубконто",
        "ExtDimensionAccountingFlags",
    ),
    ("Column", "Графы", "Columns"),
    ("URLTemplate", "ШаблоныURL", "URLTemplates"),
    ("Method", "Методы", "Methods"),
    ("Operation", "Операции", "Operations"),
    ("Parameter", "Параметры", "Parameters"),
    ("IntegrationServiceChannel", "Каналы", "Channels"),
    ("Command", "Команды", "Commands"),
    ("Function", "Функции", "Functions"),
    ("Forms", "Формы", "Forms"),
    ("Templates", "Макеты", "Templates"),
    ("Recalculations", "Перерасчеты", "Recalculations"),
    ("Modules", "Модули", "Modules"),
    ("Common", "Общие", "Common"),
];

pub fn group_name(key: &str, lang: Lang) -> String {
    group_label(key, lang).to_string()
}

pub fn group_label(key: &str, lang: Lang) -> &str {
    GROUPS
        .iter()
        .find(|(k, _, _)| *k == key)
        .map(|(_, ru, en)| if lang == Lang::Ru { *ru } else { *en })
        .unwrap_or(key)
}

pub fn module_name(kind: mdconf::ModuleKind, lang: Lang) -> String {
    module_label(kind, lang).to_string()
}

pub fn module_label(kind: mdconf::ModuleKind, lang: Lang) -> &'static str {
    use mdconf::ModuleKind as M;
    let (en, ru) = match kind {
        M::Module => ("Module", "Модуль"),
        M::ObjectModule => ("ObjectModule", "МодульОбъекта"),
        M::ManagerModule => ("ManagerModule", "МодульМенеджера"),
        M::RecordSetModule => ("RecordSetModule", "МодульНабораЗаписей"),
        M::ValueManagerModule => ("ValueManagerModule", "МодульМенеджераЗначения"),
        M::CommandModule => ("CommandModule", "МодульКоманды"),
        M::FormModule => ("FormModule", "МодульФормы"),
        M::ManagedApplicationModule => ("ManagedApplicationModule", "МодульУправляемогоПриложения"),
        M::OrdinaryApplicationModule => ("OrdinaryApplicationModule", "МодульОбычногоПриложения"),
        M::SessionModule => ("SessionModule", "МодульСеанса"),
        M::ExternalConnectionModule => ("ExternalConnectionModule", "МодульВнешнегоСоединения"),
    };
    if lang == Lang::Ru { ru } else { en }
}

pub fn fold_path_name(s: &str) -> String {
    fold_name(s.trim()).replace('ё', "е")
}

#[cfg(feature = "testing")]
static DIAGNOSTIC_KIND_MAP: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(feature = "testing")]
#[doc(hidden)]
pub fn diagnostic_kind_map_identity() -> usize {
    let _ = kind_from_name("Catalog");
    DIAGNOSTIC_KIND_MAP.load(std::sync::atomic::Ordering::Relaxed)
}

#[derive(Clone, Copy)]
struct KindEntry {
    raw: &'static str,
    kind: MdKind,
    ordinal: usize,
}
static KINDS: Graph<Vec<KindEntry>> = Graph::new();

fn folded(s: &str) -> impl Iterator<Item = char> + '_ {
    s.trim().chars().map(|c| {
        let mut lower = c.to_lowercase();
        let first = lower.next().unwrap_or(c);
        let c = if lower.next().is_none() { first } else { c };
        if c == 'ё' { 'е' } else { c }
    })
}
fn kind_rows(
    cat: &'static platform_catalog::Catalog,
    root: Option<&MemoryBudget>,
    mut row: impl FnMut(&'static str, MdKind) -> Result<(), BudgetError>,
) -> Result<(), BudgetError> {
    let q = MdKind::all()
        .iter()
        .map(|k| PREFIX_EN.len() + k.as_str().len())
        .max()
        .unwrap_or(0);
    let _scratch = root
        .map(|r| r.try_reserve(Category::Metadata, q as u64))
        .transpose()?;
    let mut query = resources::text("")?;
    query
        .try_reserve_exact(q)
        .map_err(|_| BudgetError::AllocationFailed)?;
    for &kind in MdKind::all() {
        row(kind.as_str(), kind)?;
        query.clear();
        query.push_str(PREFIX_EN);
        query.push_str(kind.as_str());
        let ru = cat
            .find_type(&query)
            .map(|i| {
                let ru = cat.type_info(i).name_ru.as_str();
                ru.strip_prefix(PREFIX_RU).unwrap_or(ru)
            })
            .or_else(|| {
                KIND_RU_FALLBACK
                    .iter()
                    .find(|(en, _)| *en == kind.as_str())
                    .map(|(_, ru)| *ru)
            });
        if let Some(ru) = ru {
            row(ru, kind)?;
        }
    }
    Ok(())
}
fn kind_map(
    cat: &'static platform_catalog::Catalog,
    root: Option<&MemoryBudget>,
) -> Result<&'static Vec<KindEntry>, BudgetError> {
    KINDS.prepare(
        root,
        || {
            let mut count = 0usize;
            kind_rows(cat, root, |_, _| {
                count = count.checked_add(1).ok_or(BudgetError::Overflow)?;
                Ok(())
            })?;
            let mut bytes = 0;
            resources::rows::<KindEntry>(&mut bytes, count)?;
            Ok(bytes)
        },
        |rows| {
            let mut bytes = 0;
            resources::rows::<KindEntry>(&mut bytes, rows.capacity())?;
            Ok(bytes)
        },
        || {
            let mut count = 0usize;
            kind_rows(cat, root, |_, _| {
                count = count.checked_add(1).ok_or(BudgetError::Overflow)?;
                Ok(())
            })?;
            let mut rows = resources::vec(count)?;
            kind_rows(cat, root, |raw, kind| {
                rows.push(KindEntry {
                    raw,
                    kind,
                    ordinal: rows.len(),
                });
                Ok(())
            })?;
            rows.sort_unstable_by(|a, b| {
                folded(a.raw)
                    .cmp(folded(b.raw))
                    .then(a.ordinal.cmp(&b.ordinal))
            });
            Ok(rows)
        },
    )
}
pub fn prepare_with_memory(budget: &MemoryBudget) -> Result<(), GuiResourceError> {
    let (cat, root) = platform_catalog::catalog_with_memory(budget)?;
    kind_map(cat, Some(&root))?;
    Ok(())
}
#[allow(
    clippy::panic,
    reason = "legacy infallible accessor; managed readiness is fallible"
)]
pub fn kind_from_name(s: &str) -> Option<MdKind> {
    let map = match kind_map(catalog(), None) {
        Ok(v) => v,
        Err(e) => panic!("kind aliases: {e}"),
    };
    #[cfg(feature = "testing")]
    DIAGNOSTIC_KIND_MAP.store(
        std::ptr::from_ref(map) as usize,
        std::sync::atomic::Ordering::Relaxed,
    );
    let i = map.partition_point(|entry| folded(entry.raw).cmp(folded(s)).is_lt());
    map.get(i)
        .filter(|entry| folded(entry.raw).eq(folded(s)))
        .map(|entry| entry.kind)
}

pub fn child_tag_from_name(s: &str) -> Option<ChildTag> {
    let f = fold_path_name(s);
    CHILD_TAGS.into_iter().find(|&t| {
        fold_path_name(child_tag_en(t)) == f
            || type_name_ru(child_tag_en(t)).is_some_and(|ru| fold_path_name(&ru) == f)
    })
}

pub fn sub_kind_from_name(s: &str) -> Option<SubKind> {
    let f = fold_path_name(s);
    [SubKind::Form, SubKind::Template].into_iter().find(|&k| {
        fold_path_name(sub_kind_en(k)) == f
            || type_name_ru(sub_kind_en(k)).is_some_and(|ru| fold_path_name(&ru) == f)
    })
}
