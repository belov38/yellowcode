use bsl_prim::fold_name;
use md_model::MdKind;
use platform_catalog::messages::Lang;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum ObjKind {
    Catalog,
    Document,
    DocumentJournal,
    Enum,
    Constant,
    ChartOfCharacteristicTypes,
    ChartOfAccounts,
    ChartOfCalculationTypes,
    ExchangePlan,
    InformationRegister,
    AccumulationRegister,
    AccountingRegister,
    CalculationRegister,
    Sequence,
    BusinessProcess,
    Task,
    DataProcessor,
    Report,
}

pub const KINDS: [ObjKind; 18] = [
    ObjKind::Catalog,
    ObjKind::Document,
    ObjKind::DocumentJournal,
    ObjKind::Enum,
    ObjKind::Constant,
    ObjKind::ChartOfCharacteristicTypes,
    ObjKind::ChartOfAccounts,
    ObjKind::ChartOfCalculationTypes,
    ObjKind::ExchangePlan,
    ObjKind::InformationRegister,
    ObjKind::AccumulationRegister,
    ObjKind::AccountingRegister,
    ObjKind::CalculationRegister,
    ObjKind::Sequence,
    ObjKind::BusinessProcess,
    ObjKind::Task,
    ObjKind::DataProcessor,
    ObjKind::Report,
];

type Row = (
    ObjKind,
    MdKind,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
);

const TABLE: [Row; 18] = [
    (
        ObjKind::Catalog,
        MdKind::Catalog,
        "Справочник",
        "Справочники",
        "Catalog",
        "Catalogs",
    ),
    (
        ObjKind::Document,
        MdKind::Document,
        "Документ",
        "Документы",
        "Document",
        "Documents",
    ),
    (
        ObjKind::DocumentJournal,
        MdKind::DocumentJournal,
        "ЖурналДокументов",
        "ЖурналыДокументов",
        "DocumentJournal",
        "DocumentJournals",
    ),
    (
        ObjKind::Enum,
        MdKind::Enum,
        "Перечисление",
        "Перечисления",
        "Enum",
        "Enums",
    ),
    (
        ObjKind::Constant,
        MdKind::Constant,
        "Константа",
        "Константы",
        "Constant",
        "Constants",
    ),
    (
        ObjKind::ChartOfCharacteristicTypes,
        MdKind::ChartOfCharacteristicTypes,
        "ПланВидовХарактеристик",
        "ПланыВидовХарактеристик",
        "ChartOfCharacteristicTypes",
        "ChartsOfCharacteristicTypes",
    ),
    (
        ObjKind::ChartOfAccounts,
        MdKind::ChartOfAccounts,
        "ПланСчетов",
        "ПланыСчетов",
        "ChartOfAccounts",
        "ChartsOfAccounts",
    ),
    (
        ObjKind::ChartOfCalculationTypes,
        MdKind::ChartOfCalculationTypes,
        "ПланВидовРасчета",
        "ПланыВидовРасчета",
        "ChartOfCalculationTypes",
        "ChartsOfCalculationTypes",
    ),
    (
        ObjKind::ExchangePlan,
        MdKind::ExchangePlan,
        "ПланОбмена",
        "ПланыОбмена",
        "ExchangePlan",
        "ExchangePlans",
    ),
    (
        ObjKind::InformationRegister,
        MdKind::InformationRegister,
        "РегистрСведений",
        "РегистрыСведений",
        "InformationRegister",
        "InformationRegisters",
    ),
    (
        ObjKind::AccumulationRegister,
        MdKind::AccumulationRegister,
        "РегистрНакопления",
        "РегистрыНакопления",
        "AccumulationRegister",
        "AccumulationRegisters",
    ),
    (
        ObjKind::AccountingRegister,
        MdKind::AccountingRegister,
        "РегистрБухгалтерии",
        "РегистрыБухгалтерии",
        "AccountingRegister",
        "AccountingRegisters",
    ),
    (
        ObjKind::CalculationRegister,
        MdKind::CalculationRegister,
        "РегистрРасчета",
        "РегистрыРасчета",
        "CalculationRegister",
        "CalculationRegisters",
    ),
    (
        ObjKind::Sequence,
        MdKind::Sequence,
        "Последовательность",
        "Последовательности",
        "Sequence",
        "Sequences",
    ),
    (
        ObjKind::BusinessProcess,
        MdKind::BusinessProcess,
        "БизнесПроцесс",
        "БизнесПроцессы",
        "BusinessProcess",
        "BusinessProcesses",
    ),
    (
        ObjKind::Task,
        MdKind::Task,
        "Задача",
        "Задачи",
        "Task",
        "Tasks",
    ),
    (
        ObjKind::DataProcessor,
        MdKind::DataProcessor,
        "Обработка",
        "Обработки",
        "DataProcessor",
        "DataProcessors",
    ),
    (
        ObjKind::Report,
        MdKind::Report,
        "Отчет",
        "Отчеты",
        "Report",
        "Reports",
    ),
];

impl ObjKind {
    fn row(self) -> Option<&'static Row> {
        TABLE.iter().find(|r| r.0 == self)
    }

    pub fn of_md(k: MdKind) -> Option<ObjKind> {
        TABLE.iter().find(|r| r.1 == k).map(|r| r.0)
    }

    pub fn md_kind(self) -> MdKind {
        self.row().map_or(MdKind::Catalog, |r| r.1)
    }

    pub fn name(self, lang: Lang) -> &'static str {
        self.row().map_or("", |r| match lang {
            Lang::Ru => r.2,
            Lang::En => r.4,
        })
    }

    pub fn collection(self, lang: Lang) -> &'static str {
        self.row().map_or("", |r| match lang {
            Lang::Ru => r.3,
            Lang::En => r.5,
        })
    }

    pub fn is_reference(self) -> bool {
        matches!(
            self,
            ObjKind::Catalog
                | ObjKind::Document
                | ObjKind::Enum
                | ObjKind::ChartOfCharacteristicTypes
                | ObjKind::ChartOfAccounts
                | ObjKind::ChartOfCalculationTypes
                | ObjKind::ExchangePlan
                | ObjKind::BusinessProcess
                | ObjKind::Task
        )
    }

    pub fn is_register(self) -> bool {
        matches!(
            self,
            ObjKind::InformationRegister
                | ObjKind::AccumulationRegister
                | ObjKind::AccountingRegister
                | ObjKind::CalculationRegister
        )
    }

    pub fn from_word(word: &str) -> Option<ObjKind> {
        let w = fold_name(word);
        TABLE
            .iter()
            .find(|r| fold_name(r.2) == w || fold_name(r.4) == w)
            .map(|r| r.0)
    }

    pub fn from_en(name: &str) -> Option<ObjKind> {
        TABLE.iter().find(|r| r.4 == name).map(|r| r.0)
    }

    pub(crate) fn index(self) -> usize {
        KINDS.iter().position(|k| *k == self).unwrap_or(0)
    }
}
