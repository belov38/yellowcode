use super::StdRole;
use mdschema::MdKind;
use platform_catalog::{Catalog, QueryTableInfo};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KindNames {
    pub ru: String,
    pub en: String,
    pub ref_ru: String,
    pub ref_en: String,
}

fn segments(name: &str) -> Vec<&str> {
    name.split('.').collect()
}

pub fn main_template(cat: &Catalog, kind: MdKind) -> Option<&QueryTableInfo> {
    template(cat, kind, None)
}

pub fn template<'c>(
    cat: &'c Catalog,
    kind: MdKind,
    suffix_en: Option<&str>,
) -> Option<&'c QueryTableInfo> {
    cat.query_tables().iter().find(|t| {
        let en = segments(&t.name_en);
        let ru = segments(&t.name_ru);
        let same_shape = en.len() == ru.len();
        match (suffix_en, en.as_slice()) {
            (None, [k, obj]) => same_shape && *k == kind.as_str() && obj.starts_with('<'),
            (Some(s), [k, obj, last]) => {
                same_shape && *k == kind.as_str() && obj.starts_with('<') && *last == s
            }
            _ => false,
        }
    })
}

pub fn template_suffix(cat: &Catalog, kind: MdKind, suffix_en: &str) -> Option<(String, String)> {
    let t = template(cat, kind, Some(suffix_en))?;
    let ru = segments(&t.name_ru).last()?.to_string();
    let en = segments(&t.name_en).last()?.to_string();
    Some((ru, en))
}

fn ref_type_names(t: &QueryTableInfo) -> Option<(String, String)> {
    let f = t.fields.iter().find(|f| f.name_en == "Ref")?;
    let ty = f.types.first()?;
    let ru = segments(&ty.name_ru).first()?.to_string();
    let en = segments(&ty.name_en).first()?.to_string();
    Some((ru, en))
}

pub fn kind_names(cat: &Catalog, kind: MdKind) -> Option<KindNames> {
    let t = main_template(cat, kind)?;
    let ru = segments(&t.name_ru).first()?.to_string();
    let en = segments(&t.name_en).first()?.to_string();
    let (ref_ru, ref_en) = ref_type_names(t)?;
    Some(KindNames {
        ru,
        en,
        ref_ru,
        ref_en,
    })
}

pub fn route_point_ref_names(cat: &Catalog) -> Option<(String, String)> {
    ref_type_names(template(cat, MdKind::BusinessProcess, Some("Points"))?)
}

pub fn std_name(role: StdRole, kind: MdKind) -> (String, String) {
    let (ru, en) = match role {
        StdRole::Ref => ("Ссылка", "Ref"),
        StdRole::DataVersion => ("ВерсияДанных", "DataVersion"),
        StdRole::DeletionMark => ("ПометкаУдаления", "DeletionMark"),
        StdRole::Owner => ("Владелец", "Owner"),
        StdRole::Parent => ("Родитель", "Parent"),
        StdRole::IsFolder => ("ЭтоГруппа", "IsFolder"),
        StdRole::Code => ("Код", "Code"),
        StdRole::Description => ("Наименование", "Description"),
        StdRole::Predefined => ("Предопределенный", "Predefined"),
        StdRole::PredefinedDataName => ("ИмяПредопределенныхДанных", "PredefinedDataName"),
        StdRole::Presentation => ("Представление", "Presentation"),
        StdRole::Number => ("Номер", "Number"),
        StdRole::Date => ("Дата", "Date"),
        StdRole::Posted => ("Проведен", "Posted"),
        StdRole::PointInTime => ("МоментВремени", "PointInTime"),
        StdRole::Order => ("Порядок", "Order"),
        StdRole::Type if kind == MdKind::ChartOfAccounts => ("Вид", "Type"),
        StdRole::Type => ("Тип", "Type"),
        StdRole::LineNumber => ("НомерСтроки", "LineNumber"),
        StdRole::Period => ("Период", "Period"),
        StdRole::Recorder => ("Регистратор", "Recorder"),
        StdRole::Active => ("Активность", "Active"),
        StdRole::RecordKind => ("ВидДвижения", "RecordType"),
        StdRole::Account => ("Счет", "Account"),
        StdRole::ExtDimension(n) => {
            return (format!("Субконто{n}"), format!("ExtDimension{n}"));
        }
        StdRole::ExtDimensionType(0) => ("ВидСубконто", "ExtDimensionType"),
        StdRole::ExtDimensionType(n) => {
            return (format!("ВидСубконто{n}"), format!("ExtDimensionType{n}"));
        }
        StdRole::ValueType => ("ТипЗначения", "ValueType"),
        StdRole::OffBalance => ("Забалансовый", "OffBalance"),
        StdRole::ExtDimensionTypes => ("ВидыСубконто", "ExtDimensionTypes"),
        StdRole::TurnoversOnly => ("ТолькоОбороты", "TurnoversOnly"),
        StdRole::ActionPeriodIsBasic => ("ПериодДействияБазовый", "ActionPeriodIsBasic"),
        StdRole::LeadingCalculationTypes => ("ВедущиеВидыРасчета", "LeadingCalculationTypes"),
        StdRole::BaseCalculationTypes => ("БазовыеВидыРасчета", "BaseCalculationTypes"),
        StdRole::DisplacingCalculationTypes => {
            ("ВытесняющиеВидыРасчета", "DisplacingCalculationTypes")
        }
        StdRole::CalculationType => ("ВидРасчета", "CalculationType"),
        StdRole::RecalculationObject => ("ОбъектПерерасчета", "RecalculationObject"),
        StdRole::SentNo => ("НомерОтправленного", "SentNo"),
        StdRole::ReceivedNo => ("НомерПринятого", "ReceivedNo"),
        StdRole::ExchangeDate => ("ДатаОбмена", "ExchangeDate"),
        StdRole::ThisNode => ("ЭтотУзел", "ThisNode"),
        StdRole::HeadTask => ("ВедущаяЗадача", "HeadTask"),
        StdRole::Started => ("Стартован", "Started"),
        StdRole::Completed => ("Завершен", "Completed"),
        StdRole::BusinessProcess => ("БизнесПроцесс", "BusinessProcess"),
        StdRole::RoutePoint => ("ТочкаМаршрута", "RoutePoint"),
        StdRole::Executed => ("Выполнена", "Executed"),
        StdRole::AccountDr => ("СчетДт", "AccountDr"),
        StdRole::AccountCr => ("СчетКт", "AccountCr"),
        StdRole::BalancedAccount => ("КорСчет", "BalancedAccount"),
        StdRole::ExtDimensionDr(n) => {
            return (format!("СубконтоДт{n}"), format!("ExtDimensionDr{n}"));
        }
        StdRole::ExtDimensionCr(n) => {
            return (format!("СубконтоКт{n}"), format!("ExtDimensionCr{n}"));
        }
        StdRole::BalancedExtDimension(n) => {
            return (
                format!("КорСубконто{n}"),
                format!("BalancedExtDimension{n}"),
            );
        }
        StdRole::ExtDimensionTypeDr(n) => {
            return (
                format!("ВидСубконтоДт{n}"),
                format!("ExtDimensionTypeDr{n}"),
            );
        }
        StdRole::ExtDimensionTypeCr(n) => {
            return (
                format!("ВидСубконтоКт{n}"),
                format!("ExtDimensionTypeCr{n}"),
            );
        }
        StdRole::ExtDimensionKind => ("Вид", "Type"),
        StdRole::PeriodAdjustment => ("УточнениеПериода", "PeriodAdjustment"),
        StdRole::RegistrationPeriod => ("ПериодРегистрации", "RegistrationPeriod"),
        StdRole::ActionPeriod => ("ПериодДействия", "ActionPeriod"),
        StdRole::BegOfActionPeriod => ("ПериодДействияНачало", "BegOfActionPeriod"),
        StdRole::EndOfActionPeriod => ("ПериодДействияКонец", "EndOfActionPeriod"),
        StdRole::BegOfBasePeriod => ("БазовыйПериодНачало", "BegOfBasePeriod"),
        StdRole::EndOfBasePeriod => ("БазовыйПериодКонец", "EndOfBasePeriod"),
        StdRole::ReversingEntry => ("Сторно", "ReversingEntry"),
        StdRole::Node => ("Узел", "Node"),
        StdRole::MessageNo => ("НомерСообщения", "MessageNo"),
        StdRole::Value => ("Значение", "Value"),
    };
    (ru.to_string(), en.to_string())
}
