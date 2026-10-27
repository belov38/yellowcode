use std::collections::HashMap;
use std::sync::OnceLock;

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

pub fn kind_from_name(s: &str) -> Option<MdKind> {
    static MAP: OnceLock<HashMap<String, MdKind>> = OnceLock::new();
    let map = MAP.get_or_init(|| {
        let mut m = HashMap::new();
        for &k in MdKind::all() {
            m.entry(fold_path_name(k.as_str())).or_insert(k);
            if let Some(ru) = kind_ru(k) {
                m.entry(fold_path_name(&ru)).or_insert(k);
            }
        }
        m
    });
    map.get(&fold_path_name(s)).copied()
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
