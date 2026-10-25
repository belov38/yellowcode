use bsl_prim::fold_name;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u32)]
pub enum Category {
    AllErrors,
    ExceptionRaisedFromScript,
    AccessViolation,
    UnsupportedFormat,
    InvalidPassword,
    NoPermissionToUseFunctionality,
    ExternalDataSourceError,
    ScriptRuntimeError,
    LocalFileAccessError,
    ScriptUseError,
    ScriptCompileError,
    ConfigurationError,
    DatabaseCopyError,
    DataCompositionSettingsError,
    GotoUrlError,
    FullTextSearchError,
    DocumentConversionError,
    SignatureVerificationError,
    PrinterError,
    SpeechProcessingError,
    SessionError,
    NetworkError,
    CollaborationSystemError,
    MultimediaToolsError,
    DatabaseTablespaceError,
    StoredDataError,
    ForcedShutdown,
    OtherError,
}

use Category::*;

const TABLE: [(Category, &str, &str); 28] = [
    (AllErrors, "ВсеОшибки", "AllErrors"),
    (
        ExceptionRaisedFromScript,
        "ИсключениеВызванноеИзВстроенногоЯзыка",
        "ExceptionRaisedFromScript",
    ),
    (AccessViolation, "НарушениеПравДоступа", "AccessViolation"),
    (
        UnsupportedFormat,
        "НеподдерживаемыйФормат",
        "UnsupportedFormat",
    ),
    (InvalidPassword, "НеправильныйПароль", "InvalidPassword"),
    (
        NoPermissionToUseFunctionality,
        "ОтсутствиеРазрешенияДляИспользованияФункциональности",
        "NoPermissionToUseFunctionality",
    ),
    (
        ExternalDataSourceError,
        "ОшибкаВнешнегоИсточникаДанных",
        "ExternalDataSourceError",
    ),
    (
        ScriptRuntimeError,
        "ОшибкаВоВремяВыполненияВстроенногоЯзыка",
        "ScriptRuntimeError",
    ),
    (
        LocalFileAccessError,
        "ОшибкаДоступаКЛокальномуФайлу",
        "LocalFileAccessError",
    ),
    (
        ScriptUseError,
        "ОшибкаИспользованияВстроенногоЯзыка",
        "ScriptUseError",
    ),
    (
        ScriptCompileError,
        "ОшибкаКомпиляцииВстроенногоЯзыка",
        "ScriptCompileError",
    ),
    (
        ConfigurationError,
        "ОшибкаКонфигурации",
        "ConfigurationError",
    ),
    (
        DatabaseCopyError,
        "ОшибкаКопииБазыДанных",
        "DatabaseCopyError",
    ),
    (
        DataCompositionSettingsError,
        "ОшибкаНастроекКомпоновкиДанных",
        "DataCompositionSettingsError",
    ),
    (
        GotoUrlError,
        "ОшибкаПереходаПоНавигационнойСсылке",
        "GotoURLError",
    ),
    (
        FullTextSearchError,
        "ОшибкаПолнотекстовогоПоиска",
        "FullTextSearchError",
    ),
    (
        DocumentConversionError,
        "ОшибкаПреобразованияДокумента",
        "DocumentConversionError",
    ),
    (
        SignatureVerificationError,
        "ОшибкаПроверкиПодписи",
        "SignatureVerificationError",
    ),
    (PrinterError, "ОшибкаРаботыСПринтером", "PrinterError"),
    (
        SpeechProcessingError,
        "ОшибкаРаботыСРечью",
        "SpeechProcessingError",
    ),
    (SessionError, "ОшибкаСеанса", "SessionError"),
    (NetworkError, "ОшибкаСети", "NetworkError"),
    (
        CollaborationSystemError,
        "ОшибкаСистемыВзаимодействия",
        "CollaborationSystemError",
    ),
    (
        MultimediaToolsError,
        "ОшибкаСредствМультимедиа",
        "MultimediaToolsError",
    ),
    (
        DatabaseTablespaceError,
        "ОшибкаТабличногоПространстваБазыДанных",
        "DatabaseTablespaceError",
    ),
    (StoredDataError, "ОшибкаХранимыхДанных", "StoredDataError"),
    (
        ForcedShutdown,
        "ПринудительноеЗавершениеРаботы",
        "ForcedShutdown",
    ),
    (OtherError, "ПрочаяОшибка", "OtherError"),
];

const _: () = {
    let mut i = 0;
    while i < TABLE.len() {
        assert!(TABLE[i].0 as usize == i);
        i += 1;
    }
};

const ALL: [Category; 28] = {
    let mut all = [AllErrors; 28];
    let mut i = 0;
    while i < TABLE.len() {
        all[i] = TABLE[i].0;
        i += 1;
    }
    all
};

impl Category {
    pub fn all() -> &'static [Category] {
        &ALL
    }

    pub fn name_ru(self) -> &'static str {
        TABLE[self as usize].1
    }

    pub fn name_en(self) -> &'static str {
        TABLE[self as usize].2
    }

    pub fn from_name(s: &str) -> Option<Category> {
        let f = fold_name(s);
        TABLE
            .iter()
            .find(|(_, ru, en)| fold_name(ru) == f || fold_name(en) == f)
            .map(|(c, _, _)| *c)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct CategorySet(pub u32);

impl CategorySet {
    pub fn of(cats: &[Category]) -> CategorySet {
        cats.iter().fold(CategorySet::default(), |s, c| s.with(*c))
    }

    pub fn with(self, c: Category) -> CategorySet {
        CategorySet(self.0 | 1 << (c as u32))
    }

    pub fn contains(self, c: Category) -> bool {
        self.0 & 1 << (c as u32) != 0
    }

    pub fn union(self, o: CategorySet) -> CategorySet {
        CategorySet(self.0 | o.0)
    }

    pub fn iter(self) -> impl Iterator<Item = Category> {
        ALL.into_iter().filter(move |c| self.contains(*c))
    }
}
