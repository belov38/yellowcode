use crate::model::Value;
use brace::Node;
use mdschema::Schema;
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::OnceLock;

pub use md_xmlout::codec::overlay::{STS, kind};

const RESOURCE: &[&str] = &[
    "Name",
    "Synonym",
    "Comment",
    "Type",
    "PasswordMode",
    "Format",
    "EditFormat",
    "ToolTip",
    "MarkNegatives",
    "Mask",
    "MultiLine",
    "ExtendedEdit",
    "MinValue",
    "MaxValue",
    "FillChecking",
    "ChoiceFoldersAndItems",
    "ChoiceParameterLinks",
    "ChoiceParameters",
    "QuickChoice",
    "CreateOnInput",
    "ChoiceForm",
    "LinkByType",
    "ChoiceHistoryOnInput",
];

fn with_tail(tail: &[&str]) -> Vec<String> {
    RESOURCE.iter().chain(tail).map(|x| x.to_string()).collect()
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

fn orders() -> &'static HashMap<&'static str, Vec<String>> {
    static CACHE: OnceLock<HashMap<&'static str, Vec<String>>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let mut m: HashMap<&'static str, Vec<String>> = HashMap::new();
        m.insert(
            "CalculationRegister",
            strs(&[
                "Name",
                "Synonym",
                "Comment",
                "UseStandardCommands",
                "DefaultListForm",
                "AuxiliaryListForm",
                "Periodicity",
                "ActionPeriod",
                "BasePeriod",
                "Schedule",
                "ScheduleValue",
                "ScheduleDate",
                "ChartOfCalculationTypes",
                "IncludeHelpInContents",
                "StandardAttributes",
                "DataLockControlMode",
                "FullTextSearch",
                "ListPresentation",
                "ExtendedListPresentation",
                "Explanation",
            ]),
        );
        m.insert(
            "CalculationRegister.Resource",
            with_tail(&["FullTextSearch"]),
        );
        m.insert(
            "CalculationRegister.Attribute",
            with_tail(&["ScheduleLink", "Indexing", "FullTextSearch"]),
        );
        m.insert(
            "CalculationRegister.Dimension",
            with_tail(&[
                "DenyIncompleteValues",
                "BaseDimension",
                "ScheduleLink",
                "Indexing",
                "FullTextSearch",
            ]),
        );
        m.insert(
            "Sequence",
            strs(&[
                "Name",
                "Synonym",
                "Comment",
                "MoveBoundaryOnPosting",
                "Documents",
                "RegisterRecords",
                "DataLockControlMode",
            ]),
        );
        m.insert(
            "Sequence.Dimension",
            strs(&[
                "Name",
                "Synonym",
                "Comment",
                "Type",
                "DocumentMap",
                "RegisterRecordsMap",
            ]),
        );
        m.insert(
            "ChartOfCalculationTypes",
            strs(&[
                "Name",
                "Synonym",
                "Comment",
                "UseStandardCommands",
                "CodeLength",
                "DescriptionLength",
                "CodeType",
                "CodeAllowedLength",
                "DefaultPresentation",
                "EditType",
                "QuickChoice",
                "ChoiceMode",
                "InputByString",
                "SearchStringModeOnInputByString",
                "FullTextSearchOnInputByString",
                "ChoiceDataGetModeOnInputByString",
                "CreateOnInput",
                "ChoiceHistoryOnInput",
                "DefaultObjectForm",
                "DefaultListForm",
                "DefaultChoiceForm",
                "AuxiliaryObjectForm",
                "AuxiliaryListForm",
                "AuxiliaryChoiceForm",
                "BasedOn",
                "DependenceOnCalculationTypes",
                "BaseCalculationTypes",
                "ActionPeriodUse",
                "StandardAttributes",
                "Characteristics",
                "StandardTabularSections",
                "PredefinedDataUpdate",
                "IncludeHelpInContents",
                "DataLockFields",
                "DataLockControlMode",
                "FullTextSearch",
                "ObjectPresentation",
                "ExtendedObjectPresentation",
                "ListPresentation",
                "ExtendedListPresentation",
                "Explanation",
                "DataHistory",
                "UpdateDataHistoryImmediatelyAfterWrite",
                "ExecuteAfterWriteDataHistoryVersionProcessing",
            ]),
        );
        m
    })
}

pub(super) const STD_ATTRS: &[(&str, &[(&str, &str)])] = &[
    (
        "AccountingRegister",
        &[
            ("-30", "PeriodAdjustment"),
            ("-9", "RecordType"),
            ("-7", "AccountCr"),
            ("-6", "AccountDr"),
        ],
    ),
    (
        "CalculationRegister",
        &[
            ("-13", "RegistrationPeriod"),
            ("-11", "ReversingEntry"),
            ("-10", "Active"),
            ("-9", "EndOfBasePeriod"),
            ("-8", "BegOfBasePeriod"),
            ("-7", "EndOfActionPeriod"),
            ("-6", "BegOfActionPeriod"),
            ("-5", "ActionPeriod"),
            ("-4", "CalculationType"),
            ("-3", "LineNumber"),
            ("-2", "Recorder"),
        ],
    ),
    (
        "ChartOfCalculationTypes.LeadingCalculationTypes",
        CCT_SECTION_ATTRS,
    ),
    (
        "ChartOfCalculationTypes.DisplacingCalculationTypes",
        CCT_SECTION_ATTRS,
    ),
    (
        "ChartOfCalculationTypes.BaseCalculationTypes",
        CCT_SECTION_ATTRS,
    ),
    (
        "ChartOfAccounts.ExtDimensionTypes",
        &[
            ("-15", "TurnoversOnly"),
            ("-14", "Predefined"),
            ("-13", "ExtDimensionType"),
            ("-12", "LineNumber"),
        ],
    ),
];

const CCT_SECTION_ATTRS: &[(&str, &str)] = &[
    ("-102", "Predefined"),
    ("-101", "CalculationType"),
    ("-100", "LineNumber"),
];

const STD_SECTIONS: &[(&str, &[(&str, &str)])] = &[
    (
        "ChartOfCalculationTypes",
        &[
            ("-30", "LeadingCalculationTypes"),
            ("-20", "DisplacingCalculationTypes"),
            ("-10", "BaseCalculationTypes"),
        ],
    ),
    ("ChartOfAccounts", &[("-12", "ExtDimensionTypes")]),
];

pub(crate) fn std_attr(kind: &str, id: &str) -> Option<&'static str> {
    let (_, l) = STD_ATTRS.iter().find(|(k, _)| *k == kind)?;
    l.iter().find(|(i, _)| *i == id).map(|(_, n)| *n)
}

const EXT_DIMENSION: &str = "91162600-3161-4326-89a0-4a7cecd5092a";
const EXT_DIMENSION_TYPE: &str = "b3b48b29-d652-47ab-9d21-7e06768c31b5";

pub(crate) fn ext_dimension_attr(kind: &str, index: &str, class: &str) -> Option<String> {
    let base = match class {
        EXT_DIMENSION => "ExtDimension",
        EXT_DIMENSION_TYPE => "ExtDimensionType",
        _ => return None,
    };
    if kind != "AccountingRegister" {
        return None;
    }
    let n: u32 = index.parse().ok()?;
    Some(format!("{base}{}", n.checked_add(1)?))
}

pub(crate) fn std_section(kind: &str, id: &str) -> Option<&'static str> {
    let (_, l) = STD_SECTIONS.iter().find(|(k, _)| *k == kind)?;
    l.iter().find(|(i, _)| *i == id).map(|(_, n)| *n)
}

pub(crate) fn std_section_id(kind: &str, name: &str) -> Option<&'static str> {
    let (_, l) = STD_SECTIONS.iter().find(|(k, _)| *k == kind)?;
    l.iter().find(|(_, n)| *n == name).map(|(i, _)| *i)
}

fn body_version(raw: &Node) -> Option<i64> {
    raw.get(&[1, 0]).and_then(Node::as_i64)
}

pub(crate) fn normalize<'a>(kind: &str, tree: &'a Node) -> Cow<'a, Node> {
    if kind == "AccountingRegister" && body_version(tree) == Some(22) {
        let mut t = tree.clone();
        if let Some(body) = t.get_mut(&[1]).and_then(Node::as_list_mut)
            && body.len() > 1
        {
            body.remove(1);
        }
        return Cow::Owned(t);
    }
    Cow::Borrowed(tree)
}

pub(crate) fn special_prop(kind: &str, name: &str, raw: &Node) -> Option<Value> {
    if kind != "AccountingRegister" || name != "PeriodAdjustmentLength" {
        return None;
    }
    let body = raw.get(&[1]).and_then(Node::as_list)?;
    match body.first().and_then(Node::as_i64)? {
        21 => Some(Value::Text("0".into())),
        22 => body
            .last()
            .filter(|_| body.len() > 1)
            .and_then(Node::text)
            .map(|t| Value::Text(t.to_string())),
        _ => None,
    }
}

pub(crate) fn observed_order(s: &'static Schema, tag: &str) -> Option<&'static [String]> {
    match orders().get(tag) {
        Some(v) => Some(v.as_slice()),
        None if is_tabular_attribute(tag) => s
            .observed_order(tag)
            .or_else(|| s.observed_order(DOCUMENT_TABULAR_ATTRIBUTE)),
        None => s.observed_order(tag),
    }
}

const DOCUMENT_TABULAR_ATTRIBUTE: &str = "Document.TabularSection.Attribute";

fn is_tabular_attribute(context: &str) -> bool {
    context.ends_with(".TabularSection.Attribute")
}

const FULL_CONTEXTS: [&str; 3] = [
    "AccountingRegister.Dimension",
    "AccountingRegister.Resource",
    "AccountingRegister.Attribute",
];

pub(crate) fn order_count(s: &'static Schema, context: &str) -> u32 {
    if orders().contains_key(context)
        || FULL_CONTEXTS.contains(&context)
        || is_tabular_attribute(context)
    {
        10
    } else {
        s.order_count(context)
    }
}
