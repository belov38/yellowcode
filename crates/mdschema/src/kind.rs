macro_rules! kinds {
    ($($v:ident => $folder:expr),* $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum MdKind { $($v),* }

        impl MdKind {
            pub fn as_str(self) -> &'static str {
                match self { $(MdKind::$v => stringify!($v)),* }
            }

            pub fn from_name(s: &str) -> Option<MdKind> {
                match s { $(stringify!($v) => Some(MdKind::$v),)* _ => None }
            }

            pub fn folder(self) -> Option<&'static str> {
                match self { $(MdKind::$v => $folder),* }
            }

            pub fn all() -> &'static [MdKind] {
                &[$(MdKind::$v),*]
            }
        }
    };
}

kinds! {
    Configuration => None,
    Language => Some("Languages"),
    Subsystem => Some("Subsystems"),
    StyleItem => Some("StyleItems"),
    Style => Some("Styles"),
    CommonPicture => Some("CommonPictures"),
    Interface => Some("Interfaces"),
    SessionParameter => Some("SessionParameters"),
    Role => Some("Roles"),
    CommonTemplate => Some("CommonTemplates"),
    FilterCriterion => Some("FilterCriteria"),
    CommonModule => Some("CommonModules"),
    CommonAttribute => Some("CommonAttributes"),
    ExchangePlan => Some("ExchangePlans"),
    XDTOPackage => Some("XDTOPackages"),
    WebService => Some("WebServices"),
    HTTPService => Some("HTTPServices"),
    WSReference => Some("WSReferences"),
    EventSubscription => Some("EventSubscriptions"),
    ScheduledJob => Some("ScheduledJobs"),
    SettingsStorage => Some("SettingsStorages"),
    FunctionalOption => Some("FunctionalOptions"),
    FunctionalOptionsParameter => Some("FunctionalOptionsParameters"),
    DefinedType => Some("DefinedTypes"),
    CommonCommand => Some("CommonCommands"),
    CommandGroup => Some("CommandGroups"),
    Constant => Some("Constants"),
    CommonForm => Some("CommonForms"),
    Catalog => Some("Catalogs"),
    Document => Some("Documents"),
    DocumentNumerator => Some("DocumentNumerators"),
    Sequence => Some("Sequences"),
    DocumentJournal => Some("DocumentJournals"),
    Enum => Some("Enums"),
    Report => Some("Reports"),
    DataProcessor => Some("DataProcessors"),
    InformationRegister => Some("InformationRegisters"),
    AccumulationRegister => Some("AccumulationRegisters"),
    ChartOfCharacteristicTypes => Some("ChartsOfCharacteristicTypes"),
    ChartOfAccounts => Some("ChartsOfAccounts"),
    AccountingRegister => Some("AccountingRegisters"),
    ChartOfCalculationTypes => Some("ChartsOfCalculationTypes"),
    CalculationRegister => Some("CalculationRegisters"),
    BusinessProcess => Some("BusinessProcesses"),
    Task => Some("Tasks"),
    ExternalDataSource => Some("ExternalDataSources"),
    IntegrationService => Some("IntegrationServices"),
    Bot => Some("Bots"),
    WebSocketClient => Some("WebSocketClients"),
    PaletteColor => Some("PaletteColors"),
}
