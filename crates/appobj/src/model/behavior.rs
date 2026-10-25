use super::kinds::ObjKind;
use super::{Usage, Uuid16};
use md_model::{DataLockControlMode, MdObject, MdRef, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CodeType {
    #[default]
    String,
    Number,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CodeSeries {
    #[default]
    WholeCatalog,
    WithinSubordination,
    WithinOwnerSubordination,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum NumberPeriodicity {
    #[default]
    Nonperiodical,
    Year,
    Quarter,
    Month,
    Day,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Posting {
    #[default]
    Allow,
    Deny,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum RecordsDeletion {
    #[default]
    AutoDelete,
    AutoDeleteOnUnpost,
    DontDelete,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum RecordsWriting {
    #[default]
    WriteModified,
    WriteSelected,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum InfoRegWriteMode {
    #[default]
    Independent,
    RecorderSubordinate,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Periodicity {
    #[default]
    Nonperiodical,
    Second,
    Day,
    Month,
    Quarter,
    Year,
    RecorderPosition,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum AccumRegType {
    #[default]
    Balance,
    Turnovers,
}

#[derive(Clone, Debug)]
pub struct Behavior {
    pub hierarchical: bool,
    pub hierarchy_folders: bool,
    pub levels: Option<u32>,
    pub folders_on_top: bool,
    pub owners: Vec<Uuid16>,
    pub subordination_use: Usage,
    pub code_length: u32,
    pub code_type: CodeType,
    pub code_series: CodeSeries,
    pub check_unique: bool,
    pub autonumbering: bool,
    pub description_length: u32,
    pub present_by_code: bool,
    pub number_length: u32,
    pub number_type: CodeType,
    pub number_periodicity: NumberPeriodicity,
    pub numerator: Option<Uuid16>,
    pub posting: Posting,
    pub realtime_posting: Posting,
    pub register_records: Vec<Uuid16>,
    pub register_records_deletion: RecordsDeletion,
    pub register_records_writing: RecordsWriting,
    pub post_in_privileged_mode: bool,
    pub unpost_in_privileged_mode: bool,
    pub sequences: Vec<Uuid16>,
    pub write_mode: Option<InfoRegWriteMode>,
    pub periodicity: Option<Periodicity>,
    pub register_type: Option<AccumRegType>,
    pub data_lock_control: DataLockControlMode,
    pub predefined_count: usize,
    pub correspondence: bool,
    pub action_period: bool,
    pub base_period: bool,
    pub action_period_use: bool,
    pub order_length: u32,
}

impl Default for Behavior {
    fn default() -> Self {
        Behavior {
            hierarchical: false,
            hierarchy_folders: false,
            levels: None,
            folders_on_top: false,
            owners: Vec::new(),
            subordination_use: Usage::ForItem,
            code_length: 0,
            code_type: CodeType::default(),
            code_series: CodeSeries::default(),
            check_unique: false,
            autonumbering: false,
            description_length: 0,
            present_by_code: false,
            number_length: 0,
            number_type: CodeType::default(),
            number_periodicity: NumberPeriodicity::default(),
            numerator: None,
            posting: Posting::default(),
            realtime_posting: Posting::Deny,
            register_records: Vec::new(),
            register_records_deletion: RecordsDeletion::default(),
            register_records_writing: RecordsWriting::default(),
            post_in_privileged_mode: true,
            unpost_in_privileged_mode: true,
            sequences: Vec::new(),
            write_mode: None,
            periodicity: None,
            register_type: None,
            data_lock_control: DataLockControlMode::default(),
            predefined_count: 0,
            correspondence: false,
            action_period: false,
            base_period: false,
            action_period_use: false,
            order_length: 0,
        }
    }
}

fn text<'a>(o: &'a MdObject, name: &str) -> Option<&'a str> {
    match o.prop(name) {
        Some(Value::Text(t)) => Some(t.as_str()),
        _ => None,
    }
}

fn flag(o: &MdObject, name: &str) -> Option<bool> {
    match o.prop(name) {
        Some(Value::Bool(b)) => Some(*b),
        Some(Value::Text(t)) => match t.as_str() {
            "true" | "1" => Some(true),
            "false" | "0" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn number(o: &MdObject, name: &str) -> Option<u32> {
    text(o, name).and_then(|t| t.trim().parse().ok())
}

fn refs(o: &MdObject, name: &str, resolve: &dyn Fn(&MdRef) -> Option<Uuid16>) -> Vec<Uuid16> {
    match o.prop(name) {
        Some(Value::RefList(l)) => l.iter().filter_map(resolve).collect(),
        _ => Vec::new(),
    }
}

pub fn parse_usage(t: &str) -> Usage {
    match t {
        "ForFolder" | "ToFolders" => Usage::ForFolder,
        "ForFolderAndItem" | "ToFoldersAndItems" => Usage::ForFolderAndItem,
        _ => Usage::ForItem,
    }
}

fn code_type(t: Option<&str>) -> CodeType {
    match t {
        Some("Number") => CodeType::Number,
        _ => CodeType::String,
    }
}

fn posting(t: Option<&str>, default: Posting) -> Posting {
    match t {
        Some("Allow") => Posting::Allow,
        Some("Deny") => Posting::Deny,
        _ => default,
    }
}

fn periodicity(t: &str) -> Periodicity {
    match t {
        "Second" => Periodicity::Second,
        "Day" => Periodicity::Day,
        "Month" => Periodicity::Month,
        "Quarter" => Periodicity::Quarter,
        "Year" => Periodicity::Year,
        "RecorderPosition" => Periodicity::RecorderPosition,
        _ => Periodicity::Nonperiodical,
    }
}

#[allow(clippy::field_reassign_with_default)]
pub(crate) fn parse(
    kind: ObjKind,
    o: &MdObject,
    config_lock: DataLockControlMode,
    resolve: &dyn Fn(&MdRef) -> Option<Uuid16>,
) -> Behavior {
    let mut b = Behavior::default();
    let has_code = matches!(
        kind,
        ObjKind::Catalog
            | ObjKind::ChartOfCharacteristicTypes
            | ObjKind::ChartOfAccounts
            | ObjKind::ChartOfCalculationTypes
            | ObjKind::ExchangePlan
    );
    b.hierarchical = flag(o, "Hierarchical").unwrap_or(false);
    b.hierarchy_folders = text(o, "HierarchyType").is_none_or(|t| t != "HierarchyOfItems");
    b.levels = match flag(o, "LimitLevelCount") {
        Some(true) => number(o, "LevelCount"),
        _ => None,
    };
    b.folders_on_top = flag(o, "FoldersOnTop").unwrap_or(false);
    b.owners = refs(o, "Owners", resolve);
    b.subordination_use = parse_usage(text(o, "SubordinationUse").unwrap_or(""));
    b.code_length = number(o, "CodeLength").unwrap_or(if has_code { 9 } else { 0 });
    b.code_type = code_type(text(o, "CodeType"));
    b.code_series = match text(o, "CodeSeries") {
        Some("WithinSubordination") => CodeSeries::WithinSubordination,
        Some("WithinOwnerSubordination") => CodeSeries::WithinOwnerSubordination,
        _ => CodeSeries::WholeCatalog,
    };
    b.check_unique = flag(o, "CheckUnique").unwrap_or(false);
    b.autonumbering = flag(o, "Autonumbering").unwrap_or(false);
    b.description_length = number(o, "DescriptionLength").unwrap_or(if has_code { 25 } else { 0 });
    b.present_by_code = text(o, "DefaultPresentation") == Some("AsCode");
    b.order_length = number(o, "OrderLength").unwrap_or(0);
    b.action_period_use = flag(o, "ActionPeriodUse").unwrap_or(false);
    b.number_length = number(o, "NumberLength").unwrap_or(0);
    b.number_type = code_type(text(o, "NumberType"));
    b.number_periodicity = match text(o, "NumberPeriodicity") {
        Some("Year") => NumberPeriodicity::Year,
        Some("Quarter") => NumberPeriodicity::Quarter,
        Some("Month") => NumberPeriodicity::Month,
        Some("Day") => NumberPeriodicity::Day,
        _ => NumberPeriodicity::Nonperiodical,
    };
    b.numerator = match o.prop("Numerator") {
        Some(Value::Ref(r)) => resolve(r),
        _ => None,
    };
    b.posting = posting(text(o, "Posting"), Posting::Allow);
    b.realtime_posting = posting(text(o, "RealTimePosting"), Posting::Deny);
    b.register_records = refs(o, "RegisterRecords", resolve);
    b.register_records_deletion = match text(o, "RegisterRecordsDeletion") {
        Some("AutoDeleteOnUnpost") => RecordsDeletion::AutoDeleteOnUnpost,
        Some("DontDelete") => RecordsDeletion::DontDelete,
        _ => RecordsDeletion::AutoDelete,
    };
    b.register_records_writing = match text(o, "RegisterRecordsWritingOnPost") {
        Some("WriteSelected") => RecordsWriting::WriteSelected,
        _ => RecordsWriting::WriteModified,
    };
    b.post_in_privileged_mode = flag(o, "PostInPrivilegedMode").unwrap_or(true);
    b.unpost_in_privileged_mode = flag(o, "UnpostInPrivilegedMode").unwrap_or(true);
    match kind {
        ObjKind::InformationRegister => {
            b.write_mode = Some(match text(o, "WriteMode") {
                Some("RecorderSubordinate") => InfoRegWriteMode::RecorderSubordinate,
                _ => InfoRegWriteMode::Independent,
            });
            b.periodicity = Some(periodicity(
                text(o, "InformationRegisterPeriodicity").unwrap_or(""),
            ));
        }
        ObjKind::CalculationRegister => {
            b.periodicity = Some(periodicity(text(o, "Periodicity").unwrap_or("")));
            b.action_period = flag(o, "ActionPeriod").unwrap_or(false);
            b.base_period = flag(o, "BasePeriod").unwrap_or(false);
        }
        ObjKind::AccumulationRegister => {
            b.register_type = Some(match text(o, "RegisterType") {
                Some("Turnovers") => AccumRegType::Turnovers,
                _ => AccumRegType::Balance,
            });
        }
        ObjKind::AccountingRegister => {
            b.correspondence = flag(o, "Correspondence").unwrap_or(false)
        }
        _ => {}
    }
    b.data_lock_control = match text(o, "DataLockControlMode") {
        Some("Automatic") => DataLockControlMode::Automatic,
        Some("Managed") => DataLockControlMode::Managed,
        Some("AutomaticAndManaged") => DataLockControlMode::AutomaticAndManaged,
        _ => config_lock,
    };
    b.predefined_count = o.predefined().map_or(0, |p| p.iter().count());
    b
}
