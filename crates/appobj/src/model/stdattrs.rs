use super::behavior::{AccumRegType, Behavior, CodeType, InfoRegWriteMode, Periodicity};
use super::kinds::ObjKind;
use super::types::{TypeAtom, TypeSet};
use super::{FieldDef, FieldRole, ObjId, Usage};
use md_model::{DateFractions, MdObject, ScriptVariant, Value};

macro_rules! std_attrs_table {
    ($($v:ident => $ru:literal, $en:literal;)*) => {
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
        pub enum StdAttr { $($v),* }

        impl StdAttr {
            pub fn names(self) -> (&'static str, &'static str) {
                match self { $(StdAttr::$v => ($ru, $en)),* }
            }
            pub const ALL: &'static [StdAttr] = &[$(StdAttr::$v),*];
        }
    };
}

std_attrs_table! {
    Ref => "Ссылка", "Ref";
    DeletionMark => "ПометкаУдаления", "DeletionMark";
    Predefined => "Предопределенный", "Predefined";
    PredefinedDataName => "ИмяПредопределенныхДанных", "PredefinedDataName";
    Code => "Код", "Code";
    Description => "Наименование", "Description";
    IsFolder => "ЭтоГруппа", "IsFolder";
    Parent => "Родитель", "Parent";
    Owner => "Владелец", "Owner";
    Date => "Дата", "Date";
    Number => "Номер", "Number";
    Posted => "Проведен", "Posted";
    LineNumber => "НомерСтроки", "LineNumber";
    Recorder => "Регистратор", "Recorder";
    Period => "Период", "Period";
    Active => "Активность", "Active";
    RecordType => "ВидДвижения", "RecordType";
    Order => "Порядок", "Order";
    ValueType => "ТипЗначения", "ValueType";
    AccountType => "Вид", "Type";
    OffBalance => "Забалансовый", "OffBalance";
    ActionPeriodIsBasic => "ПериодДействияБазовый", "ActionPeriodIsBasic";
    ExchangeDate => "ДатаОбмена", "ExchangeDate";
    SentNo => "НомерОтправленного", "SentNo";
    ReceivedNo => "НомерПринятого", "ReceivedNo";
    ThisNode => "ЭтотУзел", "ThisNode";
    HeadTask => "ВедущаяЗадача", "HeadTask";
    Completed => "Завершен", "Completed";
    Started => "Стартован", "Started";
    BusinessProcess => "БизнесПроцесс", "BusinessProcess";
    Executed => "Выполнена", "Executed";
    RoutePoint => "ТочкаМаршрута", "RoutePoint";
    Account => "Счет", "Account";
    AccountDr => "СчетДт", "AccountDr";
    AccountCr => "СчетКт", "AccountCr";
    CalculationType => "ВидРасчета", "CalculationType";
    ActionPeriod => "ПериодДействия", "ActionPeriod";
    BegOfActionPeriod => "ПериодДействияНачало", "BegOfActionPeriod";
    EndOfActionPeriod => "ПериодДействияКонец", "EndOfActionPeriod";
    BegOfBasePeriod => "БазовыйПериодНачало", "BegOfBasePeriod";
    EndOfBasePeriod => "БазовыйПериодКонец", "EndOfBasePeriod";
    RegistrationPeriod => "ПериодРегистрации", "RegistrationPeriod";
    ReversingEntry => "Сторно", "ReversingEntry";
    ActualActionPeriod => "ФактическийПериодДействия", "ActualActionPeriod";
}

impl StdAttr {
    pub fn name(self, script: ScriptVariant) -> &'static str {
        let (ru, en) = self.names();
        match script {
            ScriptVariant::Russian => ru,
            ScriptVariant::English => en,
        }
    }
}

pub fn std_attrs(kind: ObjKind, b: &Behavior) -> Vec<StdAttr> {
    use StdAttr as S;
    let mut v = Vec::new();
    match kind {
        ObjKind::Catalog => {
            v.extend([
                S::PredefinedDataName,
                S::Predefined,
                S::Ref,
                S::DeletionMark,
            ]);
            if b.hierarchical {
                if b.hierarchy_folders {
                    v.push(S::IsFolder);
                }
                v.push(S::Parent);
            }
            if !b.owners.is_empty() {
                v.push(S::Owner);
            }
            if b.description_length > 0 {
                v.push(S::Description);
            }
            if b.code_length > 0 {
                v.push(S::Code);
            }
        }
        ObjKind::Document => v.extend([S::Posted, S::Ref, S::DeletionMark, S::Date, S::Number]),
        ObjKind::Enum => v.extend([S::Order, S::Ref]),
        ObjKind::ChartOfCharacteristicTypes => {
            v.push(S::PredefinedDataName);
            code_description(&mut v, b);
            v.extend([S::DeletionMark, S::Predefined]);
            if b.hierarchical {
                v.push(S::Parent);
            }
            v.extend([S::Ref, S::ValueType]);
            if b.hierarchical {
                v.push(S::IsFolder);
            }
        }
        ObjKind::ChartOfAccounts => {
            v.extend([S::PredefinedDataName, S::AccountType, S::OffBalance]);
            code_description(&mut v, b);
            v.push(S::DeletionMark);
            if b.order_length > 0 {
                v.push(S::Order);
            }
            v.extend([S::Predefined, S::Parent, S::Ref]);
        }
        ObjKind::ChartOfCalculationTypes => {
            v.push(S::PredefinedDataName);
            code_description(&mut v, b);
            if b.action_period_use {
                v.push(S::ActionPeriodIsBasic);
            }
            v.extend([S::DeletionMark, S::Predefined, S::Ref]);
        }
        ObjKind::ExchangePlan => {
            v.push(S::ExchangeDate);
            code_description(&mut v, b);
            v.extend([
                S::SentNo,
                S::ReceivedNo,
                S::DeletionMark,
                S::Ref,
                S::ThisNode,
            ]);
        }
        ObjKind::BusinessProcess => v.extend([
            S::HeadTask,
            S::Date,
            S::Completed,
            S::Number,
            S::DeletionMark,
            S::Ref,
            S::Started,
        ]),
        ObjKind::Task => v.extend([
            S::BusinessProcess,
            S::Executed,
            S::Date,
            S::Description,
            S::Number,
            S::DeletionMark,
            S::Ref,
            S::RoutePoint,
        ]),
        ObjKind::InformationRegister => {
            let sub = b.write_mode == Some(InfoRegWriteMode::RecorderSubordinate);
            if sub {
                v.push(S::Active);
                v.push(S::LineNumber);
            }
            if !matches!(b.periodicity, None | Some(Periodicity::Nonperiodical)) {
                v.push(S::Period);
            }
            if sub {
                v.push(S::Recorder);
            }
        }
        ObjKind::AccumulationRegister => {
            if b.register_type == Some(AccumRegType::Balance) {
                v.push(S::RecordType);
            }
            v.extend([S::Active, S::LineNumber, S::Recorder, S::Period]);
        }
        ObjKind::AccountingRegister => {
            v.push(S::Active);
            if !b.correspondence {
                v.push(S::RecordType);
            }
            v.extend([S::LineNumber, S::Period, S::Recorder]);
            if b.correspondence {
                v.extend([S::AccountDr, S::AccountCr]);
            } else {
                v.push(S::Account);
            }
        }
        ObjKind::CalculationRegister => {
            v.push(S::Active);
            if b.base_period {
                v.extend([S::EndOfBasePeriod, S::BegOfBasePeriod]);
            }
            v.extend([S::CalculationType, S::LineNumber]);
            if b.action_period {
                v.extend([S::ActionPeriod, S::EndOfActionPeriod, S::BegOfActionPeriod]);
            }
            v.extend([
                S::RegistrationPeriod,
                S::Recorder,
                S::ReversingEntry,
                S::ActualActionPeriod,
            ]);
        }
        ObjKind::Sequence => v.extend([S::Period, S::Recorder]),
        ObjKind::DocumentJournal | ObjKind::Constant | ObjKind::DataProcessor | ObjKind::Report => {
        }
    }
    v
}

fn code_description(v: &mut Vec<StdAttr>, b: &Behavior) {
    if b.code_length > 0 {
        v.push(StdAttr::Code);
    }
    if b.description_length > 0 {
        v.push(StdAttr::Description);
    }
}

pub(crate) struct StdCtx<'a> {
    pub kind: ObjKind,
    pub me: ObjId,
    pub owners: &'a [ObjId],
    pub script: ScriptVariant,
}

fn std_type(a: StdAttr, b: &Behavior, cx: &StdCtx<'_>) -> TypeSet {
    use StdAttr as S;
    let one = |x: TypeAtom| TypeSet::of_atom(x);
    match a {
        S::Ref | S::Parent | S::ThisNode => one(TypeAtom::Ref(cx.me)),
        S::DeletionMark
        | S::Predefined
        | S::IsFolder
        | S::Posted
        | S::Active
        | S::OffBalance
        | S::ActionPeriodIsBasic
        | S::Completed
        | S::Started
        | S::Executed
        | S::ReversingEntry => one(TypeAtom::Bool),
        S::PredefinedDataName => TypeSet::string(0, false),
        S::Code => match b.code_type {
            CodeType::String => TypeSet::string(b.code_length, false),
            CodeType::Number => TypeSet::number(b.code_length, 0, true),
        },
        S::Description => TypeSet::string(b.description_length, false),
        S::Number => match b.number_type {
            CodeType::String => TypeSet::string(b.number_length, false),
            CodeType::Number => TypeSet::number(b.number_length, 0, true),
        },
        S::Owner => TypeSet {
            atoms: cx.owners.iter().map(|o| TypeAtom::Ref(*o)).collect(),
            ..TypeSet::default()
        },
        S::Date
        | S::Period
        | S::ExchangeDate
        | S::ActionPeriod
        | S::BegOfActionPeriod
        | S::EndOfActionPeriod
        | S::BegOfBasePeriod
        | S::EndOfBasePeriod
        | S::RegistrationPeriod
        | S::ActualActionPeriod => TypeSet::date(DateFractions::DateTime),
        S::LineNumber => TypeSet::number(9, 0, true),
        S::Order => TypeSet::number(10, 0, true),
        S::SentNo | S::ReceivedNo => TypeSet::number(15, 0, true),
        S::Recorder => one(TypeAtom::AnyRef(Some(ObjKind::Document))),
        S::RecordType => one(TypeAtom::Platform(
            match cx.kind {
                ObjKind::AccountingRegister => "AccountingRecordType",
                _ => "AccumulationRecordType",
            }
            .into(),
        )),
        S::ValueType => one(TypeAtom::Platform("TypeDescription".into())),
        S::AccountType => one(TypeAtom::Platform("AccountType".into())),
        S::HeadTask => one(TypeAtom::AnyRef(Some(ObjKind::Task))),
        S::BusinessProcess => one(TypeAtom::AnyRef(Some(ObjKind::BusinessProcess))),
        S::RoutePoint => one(TypeAtom::Platform("BusinessProcessRoutePointRef".into())),
        S::Account | S::AccountDr | S::AccountCr => {
            one(TypeAtom::AnyRef(Some(ObjKind::ChartOfAccounts)))
        }
        S::CalculationType => one(TypeAtom::AnyRef(Some(ObjKind::ChartOfCalculationTypes))),
    }
}

fn default_fill_check(a: StdAttr) -> bool {
    matches!(a, StdAttr::Description | StdAttr::Date | StdAttr::Owner)
}

pub(crate) fn std_fields(o: &MdObject, b: &Behavior, cx: &StdCtx<'_>) -> Vec<FieldDef> {
    std_attrs(cx.kind, b)
        .into_iter()
        .map(|a| {
            let (_, en) = a.names();
            let over = o.standard_attributes().iter().find(|s| s.name == en);
            let prop = |n: &str| {
                over.and_then(|s| s.props.iter().find(|p| p.name == n))
                    .map(|p| &p.value)
            };
            let fill_check = match prop("FillChecking") {
                Some(Value::Text(t)) => t == "ShowError",
                _ => default_fill_check(a),
            };
            let synonym = match prop("Synonym") {
                Some(Value::LStr(l)) => l.clone(),
                _ => Default::default(),
            };
            FieldDef {
                md: None,
                name: a.name(cx.script).to_string(),
                synonym,
                std: Some(a),
                role: FieldRole::Standard,
                types: std_type(a, b, cx),
                usage: Usage::ForFolderAndItem,
                fill_check,
                indexed: false,
            }
        })
        .collect()
}

pub(crate) fn line_number_field(script: ScriptVariant, length: u32) -> FieldDef {
    FieldDef {
        md: None,
        name: StdAttr::LineNumber.name(script).to_string(),
        synonym: Default::default(),
        std: Some(StdAttr::LineNumber),
        role: FieldRole::Standard,
        types: TypeSet::number(length, 0, true),
        usage: Usage::ForFolderAndItem,
        fill_check: false,
        indexed: false,
    }
}
