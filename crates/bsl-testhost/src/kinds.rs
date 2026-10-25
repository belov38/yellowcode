use bsl_compiler::{CommonFlags, ModuleKind};
use bsl_vm::Lang;
use std::borrow::Cow;
use std::path::Path;

#[derive(Clone, Debug)]
pub(crate) struct Classified {
    pub kind: ModuleKind,
    pub name: String,
    pub owner: Option<String>,
    pub common: Option<String>,
}

pub(crate) fn kind_ru(dir: &str) -> Option<&'static str> {
    Some(match dir {
        "Catalogs" => "Справочник",
        "Documents" => "Документ",
        "DataProcessors" => "Обработка",
        "Reports" => "Отчет",
        "ExchangePlans" => "ПланОбмена",
        "ChartsOfCharacteristicTypes" => "ПланВидовХарактеристик",
        "ChartsOfAccounts" => "ПланСчетов",
        "ChartsOfCalculationTypes" => "ПланВидовРасчета",
        "BusinessProcesses" => "БизнесПроцесс",
        "Tasks" => "Задача",
        "InformationRegisters" => "РегистрСведений",
        "AccumulationRegisters" => "РегистрНакопления",
        "AccountingRegisters" => "РегистрБухгалтерии",
        "CalculationRegisters" => "РегистрРасчета",
        "DocumentJournals" => "ЖурналДокументов",
        "Enums" => "Перечисление",
        "SettingsStorages" => "ХранилищеНастроек",
        "FilterCriteria" => "КритерийОтбора",
        "Constants" => "Константа",
        _ => return None,
    })
}

fn word_en(ru: &str) -> Option<&'static str> {
    Some(match ru {
        "Справочник" => "Catalog",
        "Документ" => "Document",
        "Обработка" => "DataProcessor",
        "Отчет" => "Report",
        "ПланОбмена" => "ExchangePlan",
        "ПланВидовХарактеристик" => "ChartOfCharacteristicTypes",
        "ПланСчетов" => "ChartOfAccounts",
        "ПланВидовРасчета" => "ChartOfCalculationTypes",
        "БизнесПроцесс" => "BusinessProcess",
        "Задача" => "Task",
        "РегистрСведений" => "InformationRegister",
        "РегистрНакопления" => "AccumulationRegister",
        "РегистрБухгалтерии" => "AccountingRegister",
        "РегистрРасчета" => "CalculationRegister",
        "ЖурналДокументов" => "DocumentJournal",
        "Перечисление" => "Enum",
        "ХранилищеНастроек" => "SettingsStorage",
        "КритерийОтбора" => "FilterCriterion",
        "Константа" => "Constant",
        "ОбщийМодуль" => "CommonModule",
        "HTTPСервис" => "HTTPService",
        "WebСервис" => "WebService",
        "ИнтеграционныйСервис" => "IntegrationService",
        "ОбщаяФорма" => "CommonForm",
        "ОбщаяКоманда" => "CommonCommand",
        "Форма" => "Form",
        "Команда" => "Command",
        "Модуль" => "Module",
        "МодульКоманды" => "CommandModule",
        "МодульОбъекта" => "ObjectModule",
        "МодульМенеджера" => "ManagerModule",
        "МодульНабораЗаписей" => "RecordSetModule",
        "МодульМенеджераЗначения" => "ValueManagerModule",
        "МодульСеанса" => "SessionModule",
        "МодульУправляемогоПриложения" => "ManagedApplicationModule",
        "МодульОбычногоПриложения" => "OrdinaryApplicationModule",
        "МодульВнешнегоСоединения" => "ExternalConnectionModule",
        _ => return None,
    })
}

pub(crate) fn display_name(ru: &str, lang: Lang) -> String {
    if lang != Lang::En {
        return ru.to_owned();
    }
    ru.split('.')
        .enumerate()
        .map(|(i, w)| match i {
            0 | 2 | 4 => word_en(w).unwrap_or(w),
            _ => w,
        })
        .collect::<Vec<_>>()
        .join(".")
}

fn object_module(dir: &str, n: &str, kind: ModuleKind, suffix: &str) -> Option<Classified> {
    let ru = kind_ru(dir)?;
    Some(Classified {
        kind,
        name: format!("{ru}.{n}.{suffix}"),
        owner: Some(format!("{ru}.{n}")),
        common: None,
    })
}

pub(crate) fn compose(s: &str) -> Cow<'_, str> {
    if !s.contains(['\u{306}', '\u{308}']) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let composed = match (out.chars().next_back(), c) {
            (Some('и'), '\u{306}') => Some('й'),
            (Some('И'), '\u{306}') => Some('Й'),
            (Some('у'), '\u{306}') => Some('ў'),
            (Some('У'), '\u{306}') => Some('Ў'),
            (Some('е'), '\u{308}') => Some('ё'),
            (Some('Е'), '\u{308}') => Some('Ё'),
            (Some('і'), '\u{308}') => Some('ї'),
            (Some('І'), '\u{308}') => Some('Ї'),
            _ => None,
        };
        if let Some(x) = composed {
            out.pop();
            out.push(x);
        } else {
            out.push(c);
        }
    }
    Cow::Owned(out)
}

pub fn module_kind_by_path(path: &Path) -> Option<ModuleKind> {
    let all: Vec<String> = path
        .iter()
        .map(|c| compose(&c.to_string_lossy()).into_owned())
        .collect();
    (2..=7usize).rev().find_map(|n| {
        let tail = all.get(all.len().checked_sub(n)?..)?;
        let segs: Vec<&str> = tail.iter().map(String::as_str).collect();
        classify(&segs).map(|c| c.kind)
    })
}

pub(crate) fn classify(segs: &[&str]) -> Option<Classified> {
    let plain = |kind, name: &str| {
        Some(Classified {
            kind,
            name: name.to_string(),
            owner: None,
            common: None,
        })
    };
    match segs {
        ["Ext", "SessionModule.bsl"] => plain(ModuleKind::Session, "МодульСеанса"),
        ["Ext", "ManagedApplicationModule.bsl"] => plain(
            ModuleKind::ManagedApplication,
            "МодульУправляемогоПриложения",
        ),
        ["Ext", "OrdinaryApplicationModule.bsl"] => {
            plain(ModuleKind::OrdinaryApplication, "МодульОбычногоПриложения")
        }
        ["Ext", "ExternalConnectionModule.bsl"] => {
            plain(ModuleKind::ExternalConnection, "МодульВнешнегоСоединения")
        }
        ["CommonModules", n, "Ext", "Module.bsl"] => Some(Classified {
            kind: ModuleKind::Common(CommonFlags::default()),
            name: format!("ОбщийМодуль.{n}.Модуль"),
            owner: None,
            common: Some((*n).to_string()),
        }),
        ["HTTPServices", n, "Ext", "Module.bsl"] => {
            plain(ModuleKind::HttpService, &format!("HTTPСервис.{n}.Модуль"))
        }
        ["WebServices", n, "Ext", "Module.bsl"] => {
            plain(ModuleKind::WebService, &format!("WebСервис.{n}.Модуль"))
        }
        ["IntegrationServices", n, "Ext", "Module.bsl"] => plain(
            ModuleKind::HttpService,
            &format!("ИнтеграционныйСервис.{n}.Модуль"),
        ),
        ["CommonForms", n, "Ext", "Form", "Module.bsl"] => Some(Classified {
            kind: ModuleKind::Form,
            name: format!("ОбщаяФорма.{n}.Форма"),
            owner: None,
            common: None,
        }),
        ["CommonCommands", n, "Ext", "CommandModule.bsl"] => plain(
            ModuleKind::Command,
            &format!("ОбщаяКоманда.{n}.МодульКоманды"),
        ),
        [dir, n, "Ext", file] => match *file {
            "ObjectModule.bsl" => object_module(dir, n, ModuleKind::Object, "МодульОбъекта"),
            "ManagerModule.bsl" => object_module(dir, n, ModuleKind::Manager, "МодульМенеджера"),
            "RecordSetModule.bsl" => {
                object_module(dir, n, ModuleKind::RecordSet, "МодульНабораЗаписей")
            }
            "ValueManagerModule.bsl" if *dir == "Constants" => {
                object_module(dir, n, ModuleKind::ValueManager, "МодульМенеджераЗначения")
            }
            _ => None,
        },
        [dir, n, "Forms", f, "Ext", "Form", "Module.bsl"] => {
            let ru = kind_ru(dir)?;
            Some(Classified {
                kind: ModuleKind::Form,
                name: format!("{ru}.{n}.Форма.{f}.Форма"),
                owner: Some(format!("{ru}.{n}")),
                common: None,
            })
        }
        [dir, n, "Commands", c, "Ext", "CommandModule.bsl"] => {
            let ru = kind_ru(dir)?;
            Some(Classified {
                kind: ModuleKind::Command,
                name: format!("{ru}.{n}.Команда.{c}.МодульКоманды"),
                owner: Some(format!("{ru}.{n}")),
                common: None,
            })
        }
        _ => None,
    }
}
