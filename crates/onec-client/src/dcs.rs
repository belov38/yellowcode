use std::collections::HashMap;

use roxmltree::{Document, Node};

use crate::answer::{AnswerError, NS_LF, NS_VRS, NS_WS, boolean, unsigned};
use crate::dlist::{Comparison, Filter};
use crate::formdescr::NS_UO;
use crate::metadata::{MdKind, MdType};
use crate::report::{EMPTY_PCHS, FormAttribute};
use crate::value::{Date, Decimal, NS_CONFIG, NS_CORE, NS_XS, Uuid, fold_key};
use crate::xml::{self, escape_text};

pub const NS_SETTINGS: &str = "http://v8.1c.ru/8.1/data-composition-system/settings";
pub const NS_DCS_CORE: &str = "http://v8.1c.ru/8.1/data-composition-system/core";
pub const NS_DETAILS: &str = "http://v8.1c.ru/8.1/data-composition-system/details";
const NS_XSI: &str = "http://www.w3.org/2001/XMLSchema-instance";

pub const COMPOSE_RESULT: &str = "controller::composeResult";
pub const VARIANT_PARAM: &str = "КлючВарианта";
const COMPOSER_FIELD: &str = "SettingsComposer";

const PROP_VARIANT_KEY: u32 = 13;
const PROP_VARIANTS: u32 = 17;
const PROP_VARIANT_PRESENTATION: u32 = 18;

const PERIOD_VARIANTS: &[&str] = &[
    "Custom",
    "Today",
    "ThisWeek",
    "ThisTenDays",
    "ThisMonth",
    "ThisQuarter",
    "ThisHalfYear",
    "ThisYear",
    "FromBeginningOfThisWeek",
    "FromBeginningOfThisTenDays",
    "FromBeginningOfThisMonth",
    "FromBeginningOfThisQuarter",
    "FromBeginningOfThisHalfYear",
    "FromBeginningOfThisYear",
    "Yesterday",
    "LastWeek",
    "LastTenDays",
    "LastMonth",
    "LastQuarter",
    "LastHalfYear",
    "LastYear",
    "LastWeekTillSameWeekDay",
    "LastTenDaysTillSameDayNumber",
    "LastMonthTillSameDate",
    "LastQuarterTillSameDate",
    "LastHalfYearTillSameDate",
    "LastYearTillSameDate",
    "Tomorrow",
    "NextWeek",
    "NextTenDays",
    "NextMonth",
    "NextQuarter",
    "NextHalfYear",
    "NextYear",
    "NextWeekTillSameWeekDay",
    "NextTenDaysTillSameDayNumber",
    "NextMonthTillSameDate",
    "NextQuarterTillSameDate",
    "NextHalfYearTillSameDate",
    "NextYearTillSameDate",
    "TillEndOfThisWeek",
    "TillEndOfThisTenDays",
    "TillEndOfThisMonth",
    "TillEndOfThisQuarter",
    "TillEndOfThisHalfYear",
    "TillEndOfThisYear",
    "Month",
];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DcsError {
    #[error(transparent)]
    Answer(#[from] AnswerError),
    #[error(
        "the form has no settings of a data composition schema (no report object whose {COMPOSER_FIELD} has \
         settings): it is not a report form on a schema; a server procedure of the form is called with --call"
    )]
    NoComposer,
    #[error(
        "the form gave no spreadsheet document: «Сформировать» of this form is code of the configuration (e.g. \
         a report form of the standard subsystems library), not the platform command; call a server procedure of \
         the form with --call (with --poll when it generates the report in the background)"
    )]
    NoDocument,
    #[error(
        "the details of the report have more than {limit} items; the report is too large to read whole"
    )]
    TooManyDetails { limit: usize },
    #[error("the report has no variant {name:?}; its variants: {known}")]
    NoVariant { name: String, known: String },
    #[error("the server opened the form on the variant {got:?}, not {asked:?}")]
    VariantNotOpened { asked: String, got: String },
    #[error("{name:?} is not a {what} of the user settings of the report; they are: {known}")]
    NoSetting {
        name: String,
        what: &'static str,
        known: String,
    },
    #[error("{name:?} names {count} user settings of the report; name one by its title or its id")]
    Ambiguous { name: String, count: usize },
    #[error("the user setting {name:?} is changed twice")]
    Twice { name: String },
    #[error("{name}: {reason}")]
    Value { name: String, reason: String },
    #[error("{name}: the value of the type {ty} is not implemented")]
    NotImplemented { name: String, ty: String },
    #[error(
        "{name}: the setting has the types {types}; a value of a composite type is not implemented"
    )]
    Composite { name: String, types: String },
    #[error(
        "{name}: the type of the value is not known (neither the schema nor the setting names it)"
    )]
    NoType { name: String },
    #[error("{name}: no object of {ty} is found by {text:?}; give its uuid")]
    NotFound {
        name: String,
        ty: String,
        text: String,
    },
    #[error(
        "{name}: {count} objects of {ty} are found by {text:?} ({found}); give the uuid of one"
    )]
    Several {
        name: String,
        ty: String,
        text: String,
        count: usize,
        found: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variant {
    pub key: String,
    pub presentation: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingKind {
    Parameter {
        name: String,
    },
    Filter {
        field: Option<String>,
        comparison: String,
    },
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingValue {
    pub type_name: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserSetting {
    pub id: String,
    pub kind: SettingKind,
    pub used: bool,
    pub value: Option<SettingValue>,
    pub title: Option<String>,
    raw: String,
    raw_value: Option<String>,
}

impl UserSetting {
    pub fn name(&self) -> &str {
        match &self.kind {
            SettingKind::Parameter { name } => name,
            SettingKind::Filter {
                field: Some(field), ..
            } => field,
            _ => self.title.as_deref().unwrap_or(&self.id),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Available {
    pub name: String,
    pub title: Option<String>,
    pub types: Vec<String>,
    pub visible: bool,
    pub folder: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ComposerPlace {
    attribute: usize,
    field: usize,
    raw_fields: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DcsForm {
    pub variant: Option<Variant>,
    pub variants: Vec<Variant>,
    pub settings: Vec<UserSetting>,
    pub parameters: Vec<Available>,
    pub fields: Vec<Available>,
    place: ComposerPlace,
}

pub fn read_variants(query: &[u8]) -> Result<(Option<Variant>, Vec<Variant>), DcsError> {
    let text = utf8(query);
    let doc = parse("logForm/query", &text)?;
    Ok(variants_of(doc.root_element()))
}

pub fn read_form(
    query: &[u8],
    descr: &[u8],
    attributes: &[FormAttribute],
) -> Result<DcsForm, DcsError> {
    const ANSWER: &str = "logForm/query";
    let (attribute, field, fields) = composer_of(descr, attributes)?;
    let text = utf8(query);
    let doc = parse(ANSWER, &text)?;
    let root = doc.root_element();
    let (variant, variants) = variants_of(root);
    let Some(object) =
        uo_child(root, NS_WS, "props").and_then(|p| uo_values(p).get(attribute).copied())
    else {
        return Err(DcsError::NoComposer);
    };
    let values = uo_values(object);
    if values.is_empty() {
        return Err(DcsError::NoComposer);
    }
    if values.len() != fields {
        return Err(AnswerError::Bad {
            answer: ANSWER,
            field: "the value of the report object".into(),
            value: format!("{} values for {fields} fields", values.len()),
        }
        .into());
    }
    let raw_fields = values.iter().map(|v| raw(&text, *v)).collect();
    let composer = values[field];
    if !composer
        .children()
        .filter(|n| is(*n, NS_SETTINGS, "settings"))
        .any(|n| n.children().any(|c| c.is_element()))
    {
        return Err(DcsError::NoComposer);
    }
    let (parameters, fields) = available(root);
    let settings = settings_of(&text, composer, &parameters, &fields)?;
    Ok(DcsForm {
        variant,
        variants,
        settings,
        parameters,
        fields,
        place: ComposerPlace {
            attribute,
            field,
            raw_fields,
        },
    })
}

impl DcsForm {
    pub fn find_variant(&self, name: &str) -> Result<&Variant, DcsError> {
        find_variant(&self.variants, name)
    }

    pub fn compose_pchs(&self, edits: &[Edit]) -> Result<String, DcsError> {
        if edits.is_empty() {
            return Ok(EMPTY_PCHS.to_string());
        }
        let mut items = String::new();
        for (i, s) in self.settings.iter().enumerate() {
            match edits.iter().find(|e| e.setting == i) {
                Some(e) => items.push_str(&item_xml(s, e)?),
                None => items.push_str(&s.raw),
            }
        }
        let mut object = String::new();
        for (i, r) in self.place.raw_fields.iter().enumerate() {
            if i == self.place.field {
                object.push_str(&format!(
                    "<uo:v><SettingsComposer xmlns=\"{NS_SETTINGS}\" xmlns:dcscor=\"{NS_DCS_CORE}\">\
<userSettings>{items}</userSettings></SettingsComposer></uo:v>"
                ));
            } else {
                object.push_str(r);
            }
        }
        Ok(format!(
            "<pchs sinDesc=\"0\" sin=\"0\" seq=\"0\"><uo:structure fields=\"{}\"><uo:v>{object}</uo:v>\
</uo:structure></pchs>",
            self.place.attribute
        ))
    }
}

pub fn find_variant<'v>(variants: &'v [Variant], name: &str) -> Result<&'v Variant, DcsError> {
    let k = fold_key(name.trim());
    variants
        .iter()
        .find(|v| fold_key(&v.key) == k)
        .or_else(|| variants.iter().find(|v| fold_key(&v.presentation) == k))
        .ok_or_else(|| DcsError::NoVariant {
            name: name.to_string(),
            known: variant_list(variants),
        })
}

fn variant_list(v: &[Variant]) -> String {
    if v.is_empty() {
        return "none".into();
    }
    v.iter()
        .map(|v| {
            if v.key == v.presentation {
                v.key.clone()
            } else {
                format!("{} ({})", v.key, v.presentation)
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn compose_params() -> [crate::value::Value; 5] {
    use crate::value::Value;
    let n = |v: i64| Value::Number(Decimal::from_i64(v));
    [n(18), n(2), n(0), n(0), Value::Bool(false)]
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Set { name: String, value: String },
    Filter(Filter),
    Off { name: String },
}

impl Change {
    pub fn parse_set(s: &str) -> Option<Change> {
        let (name, value) = s.split_once('=')?;
        let name = name.trim();
        (!name.is_empty()).then(|| Change::Set {
            name: name.to_string(),
            value: value.to_string(),
        })
    }

    fn name(&self) -> &str {
        match self {
            Change::Set { name, .. } | Change::Off { name } => name,
            Change::Filter(f) => &f.field,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Typed {
    Xml {
        type_name: String,
        inner: String,
    },
    Search {
        type_name: String,
        clsid: Uuid,
        text: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub setting: usize,
    pub used: bool,
    pub comparison: Option<&'static str>,
    pub value: Option<Typed>,
}

#[derive(Debug, Clone, Default)]
pub struct RefTypes {
    by_name: HashMap<String, (Uuid, Option<Vec<String>>)>,
}

impl RefTypes {
    pub fn new(types: &[MdType]) -> RefTypes {
        let mut by_name = HashMap::new();
        for t in types {
            let (Ok(clsid), Some((en, _))) = (Uuid::parse(&t.type_id), t.kind.value_type()) else {
                continue;
            };
            let values =
                (t.kind == MdKind::Enum).then(|| t.values.iter().map(|v| v.name.clone()).collect());
            by_name.insert(format!("{en}.{}", t.name), (clsid, values));
        }
        RefTypes { by_name }
    }
}

pub fn resolve(
    form: &DcsForm,
    changes: &[Change],
    types: &RefTypes,
) -> Result<Vec<Edit>, DcsError> {
    let mut edits: Vec<Edit> = Vec::new();
    for c in changes {
        let index = find_setting(form, c)?;
        if edits.iter().any(|e| e.setting == index) {
            return Err(DcsError::Twice {
                name: c.name().to_string(),
            });
        }
        let s = &form.settings[index];
        let edit = match c {
            Change::Off { .. } => Edit {
                setting: index,
                used: false,
                comparison: None,
                value: None,
            },
            Change::Set { value, .. } => Edit {
                setting: index,
                used: true,
                comparison: None,
                value: Some(typed(form, s, value, types)?),
            },
            Change::Filter(f) if matches!(f.op, Comparison::Contains | Comparison::NotContains) => {
                if setting_types(form, s) != ["xs:string"] {
                    return Err(DcsError::Value {
                        name: s.name().to_string(),
                        reason: "~ and !~ compare strings; the field is not a string".into(),
                    });
                }
                Edit {
                    setting: index,
                    used: true,
                    comparison: Some(comparison_xml(f.op)),
                    value: Some(Typed::Xml {
                        type_name: "xs:string".into(),
                        inner: escape_text(&f.value),
                    }),
                }
            }
            Change::Filter(f) => Edit {
                setting: index,
                used: true,
                comparison: Some(comparison_xml(f.op)),
                value: Some(typed(form, s, &f.value, types)?),
            },
        };
        edits.push(edit);
    }
    Ok(edits)
}

fn find_setting(form: &DcsForm, c: &Change) -> Result<usize, DcsError> {
    let name = c.name();
    let k = fold_key(name.trim());
    let (what, fits): (&'static str, fn(&SettingKind) -> bool) = match c {
        Change::Set { .. } => ("parameter", |k| matches!(k, SettingKind::Parameter { .. })),
        Change::Filter(_) => ("filter", |k| matches!(k, SettingKind::Filter { .. })),
        Change::Off { .. } => ("parameter or filter", |k| {
            matches!(
                k,
                SettingKind::Parameter { .. } | SettingKind::Filter { .. }
            )
        }),
    };
    let fits = |s: &UserSetting| fits(&s.kind);
    let by = |key: &dyn Fn(&UserSetting) -> Option<String>| -> Vec<usize> {
        form.settings
            .iter()
            .enumerate()
            .filter(|(_, s)| fits(s) && key(s).is_some_and(|n| fold_key(&n) == k))
            .map(|(i, _)| i)
            .collect()
    };
    let mut found = by(&|s| Some(s.name().to_string()));
    if found.is_empty() {
        found = by(&|s| s.title.clone());
    }
    if found.is_empty() {
        found = by(&|s| Some(s.id.clone()));
    }
    match found.as_slice() {
        [i] => Ok(*i),
        [] => Err(DcsError::NoSetting {
            name: name.to_string(),
            what,
            known: {
                let names: Vec<String> = form
                    .settings
                    .iter()
                    .filter(|s| fits(s))
                    .map(|s| match &s.title {
                        Some(t) if t != s.name() => format!("{} ({t})", s.name()),
                        _ => s.name().to_string(),
                    })
                    .collect();
                if names.is_empty() {
                    "none".into()
                } else {
                    names.join(", ")
                }
            },
        }),
        many => Err(DcsError::Ambiguous {
            name: name.to_string(),
            count: many.len(),
        }),
    }
}

fn comparison_xml(c: Comparison) -> &'static str {
    match c {
        Comparison::Equal => "Equal",
        Comparison::NotEqual => "NotEqual",
        Comparison::Less => "Less",
        Comparison::LessOrEqual => "LessOrEqual",
        Comparison::Greater => "Greater",
        Comparison::GreaterOrEqual => "GreaterOrEqual",
        Comparison::Contains => "Contains",
        Comparison::NotContains => "NotContains",
    }
}

fn typed(form: &DcsForm, s: &UserSetting, text: &str, types: &RefTypes) -> Result<Typed, DcsError> {
    let name = s.name().to_string();
    let ty = setting_types(form, s);
    let t = match ty.as_slice() {
        [] => return Err(DcsError::NoType { name }),
        [t] => t.clone(),
        many => {
            return Err(DcsError::Composite {
                name,
                types: many.join(", "),
            });
        }
    };
    let err = |reason: String| DcsError::Value {
        name: name.clone(),
        reason,
    };
    let xml_of = |inner: String| Typed::Xml {
        type_name: t.clone(),
        inner,
    };
    let v = if t == "xs:string" { text } else { text.trim() };
    Ok(match t.as_str() {
        "xs:string" => xml_of(escape_text(v)),
        "xs:decimal" => xml_of(
            Decimal::parse(v)
                .map_err(|e| err(e.to_string()))?
                .to_string(),
        ),
        "xs:boolean" => xml_of(
            match fold_key(v).as_str() {
                "true" | "истина" | "да" => "true",
                "false" | "ложь" | "нет" => "false",
                _ => return Err(err(format!("not a boolean: {v:?}"))),
            }
            .to_string(),
        ),
        "xs:dateTime" => xml_of(date(v, false).map_err(err)?),
        "v8:StandardPeriod" => xml_of(period(v).map_err(err)?),
        _ => {
            let Some(local) = t.strip_prefix("cfg:") else {
                return Err(DcsError::NotImplemented {
                    name: name.clone(),
                    ty: t.clone(),
                });
            };
            let Some((clsid, values)) = types.by_name.get(local) else {
                return Err(err(format!(
                    "its type {t} is not among the types of the session"
                )));
            };
            match values {
                Some(values) => {
                    let k = fold_key(v);
                    let value = values
                        .iter()
                        .find(|n| fold_key(n) == k)
                        .ok_or_else(|| err(format!("{v:?} is not a value of {local}")))?;
                    xml_of(escape_text(value))
                }
                None => match Uuid::parse(v) {
                    Ok(u) => xml_of(u.to_string()),
                    Err(_) if v.is_empty() => return Err(err("an empty reference".into())),
                    Err(_) => Typed::Search {
                        type_name: t.clone(),
                        clsid: *clsid,
                        text: v.to_string(),
                    },
                },
            }
        }
    })
}

fn setting_types(form: &DcsForm, s: &UserSetting) -> Vec<String> {
    let schema = match &s.kind {
        SettingKind::Parameter { name } => form.parameters.iter().find(|p| &p.name == name),
        SettingKind::Filter { field: Some(f), .. } => form.fields.iter().find(|p| &p.name == f),
        _ => None,
    };
    let mut ty: Vec<String> = schema.map(|a| a.types.clone()).unwrap_or_default();
    if ty.is_empty()
        && let Some(t) = s.value.as_ref().and_then(|v| v.type_name.clone())
    {
        ty.push(t);
    }
    ty
}

fn date(v: &str, end: bool) -> Result<String, String> {
    let full = if v.len() == 10 {
        format!("{v}T{}", if end { "23:59:59" } else { "00:00:00" })
    } else {
        v.to_string()
    };
    Date::parse(&full)
        .map(|d| d.to_string())
        .map_err(|e| e.to_string())
}

fn period(v: &str) -> Result<String, String> {
    if let Some((from, to)) = v.split_once("..") {
        let (from, to) = (date(from.trim(), false)?, date(to.trim(), true)?);
        return Ok(format!(
            "<v8:variant xsi:type=\"v8:StandardPeriodVariant\">Custom</v8:variant><v8:startDate>{from}</v8:startDate>\
<v8:endDate>{to}</v8:endDate>"
        ));
    }
    let k = fold_key(v);
    let variant = PERIOD_VARIANTS
        .iter()
        .find(|p| fold_key(p) == k && **p != "Custom")
        .ok_or_else(|| {
            format!(
                "not a period: {v:?} (a period is 'ГГГГ-ММ-ДД..ГГГГ-ММ-ДД' or a variant of a standard period, e.g. \
                 ThisYear, LastMonth)"
            )
        })?;
    Ok(format!(
        "<v8:variant xsi:type=\"v8:StandardPeriodVariant\">{variant}</v8:variant>"
    ))
}

fn item_xml(s: &UserSetting, e: &Edit) -> Result<String, DcsError> {
    let value = |tag: &str| -> Result<String, DcsError> {
        Ok(match &e.value {
            Some(Typed::Xml { type_name, inner }) => format!(
                "<{tag} xsi:type=\"{}\">{inner}</{tag}>",
                xml::escape_attr(type_name)
            ),
            Some(Typed::Search { text, .. }) => {
                return Err(DcsError::Value {
                    name: s.name().to_string(),
                    reason: format!("the reference {text:?} is not found yet"),
                });
            }
            None => s.raw_value.clone().unwrap_or_default(),
        })
    };
    let id = escape_text(&s.id);
    Ok(match &s.kind {
        SettingKind::Parameter { name } => format!(
            "<item xsi:type=\"SettingsParameterValue\">{}<dcscor:parameter>{}</dcscor:parameter>{}\
<userSettingID>{id}</userSettingID></item>",
            if e.used {
                ""
            } else {
                "<dcscor:use>false</dcscor:use>"
            },
            escape_text(name),
            value("dcscor:value")?
        ),
        SettingKind::Filter { comparison, .. } => format!(
            "<item xsi:type=\"FilterItemComparison\">{}<comparisonType>{}</comparisonType>{}\
<userSettingID>{id}</userSettingID></item>",
            if e.used { "" } else { "<use>false</use>" },
            escape_text(e.comparison.unwrap_or(comparison)),
            value("right")?
        ),
        SettingKind::Other(_) => s.raw.clone(),
    })
}

fn composer_of(
    descr: &[u8],
    attributes: &[FormAttribute],
) -> Result<(usize, usize, usize), DcsError> {
    const ANSWER: &str = "logForm/dataDescrWMethods";
    let text = utf8(descr);
    let doc = parse(ANSWER, &text)?;
    let structures: Vec<Node> = doc
        .descendants()
        .filter(|n| is(*n, NS_UO, "structure"))
        .collect();
    for a in attributes {
        let Some(object) = &a.object else {
            continue;
        };
        let Some(s) = structures
            .iter()
            .find(|s| s.attribute("name") == Some(object.as_str()))
        else {
            continue;
        };
        let fields: Vec<Node> = s.children().filter(|n| is(*n, NS_UO, "field")).collect();
        if let Some(i) = fields
            .iter()
            .position(|f| f.attribute("name") == Some(COMPOSER_FIELD))
        {
            return Ok((a.position, i, fields.len()));
        }
    }
    Err(DcsError::NoComposer)
}

fn variants_of(root: Node) -> (Option<Variant>, Vec<Variant>) {
    let mut key = None;
    let mut presentation = None;
    let mut list = Vec::new();
    let props = root
        .children()
        .filter(|n| is(*n, NS_WS, "elchs"))
        .flat_map(|e| e.children().filter(|n| is(*n, NS_LF, "elemcs")))
        .filter(|e| e.attribute("type") == Some("3") && e.attribute("id") == Some("0"))
        .flat_map(|e| e.descendants().filter(|n| is(*n, NS_LF, "prop")));
    for p in props {
        let id = p
            .children()
            .find(|n| is(*n, NS_LF, "id"))
            .and_then(|n| n.text())
            .and_then(unsigned);
        let Some(val) = p.children().find(|n| is(*n, NS_LF, "val")) else {
            continue;
        };
        match id {
            Some(PROP_VARIANT_KEY) => key = val.text().map(str::to_string),
            Some(PROP_VARIANT_PRESENTATION) => presentation = val.text().map(str::to_string),
            Some(PROP_VARIANTS) => {
                list = val
                    .children()
                    .filter(|n| is(*n, NS_CORE, "item"))
                    .filter_map(|i| {
                        let text_of = |local: &str| {
                            i.children()
                                .find(|n| is(*n, NS_CORE, local))
                                .and_then(|n| n.text())
                                .map(str::to_string)
                        };
                        let key = text_of("value")?;
                        Some(Variant {
                            presentation: text_of("presentation").unwrap_or_else(|| key.clone()),
                            key,
                        })
                    })
                    .collect();
            }
            _ => {}
        }
    }
    let variant = match key.filter(|k| !k.is_empty()) {
        Some(key) => Some(Variant {
            presentation: presentation.unwrap_or_else(|| key.clone()),
            key,
        }),
        None => list.first().cloned(),
    };
    (variant, list)
}

fn available(root: Node) -> (Vec<Available>, Vec<Available>) {
    let Some(ccd) = root
        .children()
        .filter(|n| is(*n, NS_WS, "cd"))
        .flat_map(|cd| cd.children().filter(|n| is(*n, NS_WS, "m")))
        .find(|m| {
            m.children()
                .any(|n| is(n, NS_WS, "ccn") && n.text() == Some("report"))
        })
        .and_then(|m| m.children().find(|n| is(*n, NS_WS, "ccd")))
    else {
        return (Vec::new(), Vec::new());
    };
    let item = |n: Node| -> Option<Available> {
        let text_of = |local: &str| {
            n.children()
                .find(|c| c.is_element() && c.tag_name().name() == local)
                .and_then(|c| c.text())
                .map(str::to_string)
        };
        let types = n
            .children()
            .find(|c| c.is_element() && c.tag_name().name() == "valueType")
            .map(|vt| {
                vt.children()
                    .filter(|c| is(*c, NS_CORE, "Type"))
                    .filter_map(|c| c.text().map(|t| usual_qname(c, t)))
                    .collect()
            })
            .unwrap_or_default();
        Some(Available {
            name: text_of("dataPathInt")?,
            title: text_of("title"),
            types,
            visible: text_of("visible").and_then(|v| boolean(&v)).unwrap_or(true),
            folder: text_of("isFolder")
                .and_then(|v| boolean(&v))
                .unwrap_or(false),
        })
    };
    let mut parameters = Vec::new();
    let mut fields = Vec::new();
    for n in ccd.descendants().filter(Node::is_element) {
        match n.tag_name().name() {
            "parameter" => parameters.extend(item(n)),
            "field" if n.parent().is_some_and(|p| p.tag_name().name() == "fields") => {
                fields.extend(item(n))
            }
            _ => {}
        }
    }
    (parameters, fields)
}

fn settings_of(
    text: &str,
    composer: Node,
    parameters: &[Available],
    fields: &[Available],
) -> Result<Vec<UserSetting>, DcsError> {
    let settings = composer
        .children()
        .find(|n| is(*n, NS_SETTINGS, "settings"));
    let Some(user) = composer
        .children()
        .find(|n| is(*n, NS_SETTINGS, "userSettings"))
    else {
        return Ok(Vec::new());
    };
    let mut of_variant: HashMap<String, Node> = HashMap::new();
    if let Some(s) = settings {
        for n in s.descendants().filter(Node::is_element) {
            if let Some(id) = child_any(n, "userSettingID").and_then(|i| i.text()) {
                of_variant.entry(id.to_string()).or_insert(n);
            }
        }
    }
    let mut out = Vec::new();
    for item in user.children().filter(|n| is(*n, NS_SETTINGS, "item")) {
        let Some(id) = child_any(item, "userSettingID")
            .and_then(|i| i.text())
            .map(str::to_string)
        else {
            continue;
        };
        let ty = item
            .attribute((NS_XSI, "type"))
            .map(|t| t.rsplit(':').next().unwrap_or(t).to_string())
            .unwrap_or_default();
        let variant_item = of_variant.get(&id).copied();
        let used = child_any(item, "use")
            .and_then(|u| u.text())
            .and_then(boolean)
            .unwrap_or(true);
        let presentation = variant_item
            .and_then(|n| child_any(n, "userSettingPresentation"))
            .or_else(|| child_any(item, "userSettingPresentation"))
            .and_then(|n| n.text())
            .map(str::to_string);
        let (kind, value_node, title) = match ty.as_str() {
            "SettingsParameterValue" => {
                let name = child_any(item, "parameter")
                    .or_else(|| variant_item.and_then(|n| child_any(n, "parameter")))
                    .and_then(|n| n.text())
                    .unwrap_or("")
                    .to_string();
                let title = parameters
                    .iter()
                    .find(|p| p.name == name)
                    .and_then(|p| p.title.clone());
                (
                    SettingKind::Parameter { name },
                    child_any(item, "value"),
                    title,
                )
            }
            "FilterItemComparison" => {
                let field = child_any(item, "left")
                    .or_else(|| variant_item.and_then(|n| child_any(n, "left")))
                    .and_then(|n| n.text())
                    .map(str::to_string);
                let comparison = child_any(item, "comparisonType")
                    .or_else(|| variant_item.and_then(|n| child_any(n, "comparisonType")))
                    .and_then(|n| n.text())
                    .unwrap_or("Equal")
                    .to_string();
                let title = field
                    .as_ref()
                    .and_then(|f| fields.iter().find(|a| &a.name == f))
                    .and_then(|a| a.title.clone());
                (
                    SettingKind::Filter { field, comparison },
                    child_any(item, "right"),
                    title,
                )
            }
            _ => (SettingKind::Other(ty.clone()), None, None),
        };
        out.push(UserSetting {
            id,
            kind,
            used,
            value: value_node.map(value_of),
            title: presentation.or(title),
            raw: raw(text, item),
            raw_value: value_node.map(|v| raw(text, v)),
        });
    }
    Ok(out)
}

fn value_of(v: Node) -> SettingValue {
    let type_name = v.attribute((NS_XSI, "type")).map(|t| usual_qname(v, t));
    if v.attribute((NS_XSI, "nil")) == Some("true") {
        return SettingValue {
            type_name,
            text: String::new(),
        };
    }
    let core = |local: &str| {
        v.children()
            .find(|n| is(*n, NS_CORE, local))
            .and_then(|n| n.text())
            .unwrap_or("")
            .to_string()
    };
    let text = if type_name.as_deref() == Some("v8:StandardPeriod") {
        match core("variant").as_str() {
            "Custom" => format!("{}..{}", core("startDate"), core("endDate")),
            other => other.to_string(),
        }
    } else if v.children().any(|n| n.is_element()) {
        v.descendants()
            .filter(Node::is_text)
            .filter_map(|n| n.text())
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        v.text().unwrap_or("").to_string()
    };
    SettingValue { type_name, text }
}

pub fn details_address(call: &[u8]) -> Result<Option<String>, DcsError> {
    let text = utf8(call);
    let doc = parse("logForm/call", &text)?;
    let Some(pchs) = doc
        .root_element()
        .children()
        .find(|n| n.is_element() && n.tag_name().name() == "pchs")
    else {
        return Ok(None);
    };
    let mut found: Vec<String> = pchs
        .descendants()
        .filter(|n| is(*n, NS_UO, "v"))
        .filter_map(|n| {
            let t = n.text()?;
            (n.children().all(|c| c.is_text()) && t.starts_with("e1cib/tempstorage/"))
                .then(|| t.to_string())
        })
        .collect();
    found.sort_unstable();
    found.dedup();
    match found.len() {
        0 => Ok(None),
        1 => Ok(found.pop()),
        n => Err(AnswerError::Bad {
            answer: "logForm/call",
            field: "the address of the details data".into(),
            value: format!("{n} addresses of the temporary storage among the changes of the form"),
        }
        .into()),
    }
}

pub fn search_request(clsid: Uuid, text: &str) -> Vec<u8> {
    format!(
        "\u{feff}<?xml version=\"1.0\" encoding=\"UTF-8\"?><request xmlns=\"{NS_WS}\" xmlns:cfg=\"{NS_CONFIG}\" \
xmlns:v8=\"{NS_CORE}\" xmlns:xs=\"{NS_XS}\" xmlns:xsi=\"{NS_XSI}\"><type>{clsid}</type><choiceParams/><text>{}</text>\
<queryValuesCount>10</queryValuesCount><fillFmtdPres>true</fillFmtdPres><pres>false</pres>\
<needLimitSearchString>true</needLimitSearchString></request>",
        escape_text(text)
    )
    .into_bytes()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub uuid: Uuid,
    pub presentation: String,
}

pub fn read_search(body: &[u8]) -> Result<Vec<Found>, DcsError> {
    const ANSWER: &str = "logForm/querySearchResult";
    let text = utf8(body);
    let doc = parse(ANSWER, &text)?;
    let root = doc.root_element();
    exception(ANSWER, root, body)?;
    let Some(values) = root.children().find(|n| is(*n, NS_WS, "values")) else {
        return Ok(Vec::new());
    };
    values
        .children()
        .filter(|n| is(*n, NS_CORE, "item"))
        .map(|i| {
            let text_of = |local: &str| {
                i.children()
                    .find(|n| is(*n, NS_CORE, local))
                    .and_then(|n| n.text())
                    .unwrap_or("")
            };
            let v = text_of("value");
            let uuid = Uuid::parse(v).map_err(|_| AnswerError::Bad {
                answer: ANSWER,
                field: "value".into(),
                value: v.to_string(),
            })?;
            let shown = match text_of("formatPresentation") {
                "" => text_of("presentation").to_string(),
                f => strip_markup(f),
            };
            Ok(Found {
                uuid,
                presentation: shown,
            })
        })
        .collect()
}

fn strip_markup(s: &str) -> String {
    let mut out = String::new();
    let mut tag = false;
    for c in s.chars() {
        match c {
            '<' => tag = true,
            '>' if tag => tag = false,
            _ if !tag => out.push(c),
            _ => {}
        }
    }
    out
}

pub fn settle_searches(
    edits: &mut [Edit],
    form: &DcsForm,
    mut find: impl FnMut(Uuid, &str) -> Result<Vec<Found>, DcsError>,
) -> Result<(), DcsError> {
    for e in edits.iter_mut() {
        let Some(Typed::Search {
            type_name,
            clsid,
            text,
        }) = &e.value
        else {
            continue;
        };
        let (ty, clsid, text) = (type_name.clone(), *clsid, text.clone());
        let name = form
            .settings
            .get(e.setting)
            .map_or_else(|| e.setting.to_string(), |s| s.name().to_string());
        let found = find(clsid, &text)?;
        match found.as_slice() {
            [one] => {
                e.value = Some(Typed::Xml {
                    type_name: ty,
                    inner: one.uuid.to_string(),
                });
            }
            [] => {
                return Err(DcsError::NotFound { name, ty, text });
            }
            many => {
                return Err(DcsError::Several {
                    name,
                    ty,
                    text,
                    count: many.len(),
                    found: many
                        .iter()
                        .map(|f| format!("{} {}", f.presentation, f.uuid))
                        .collect::<Vec<_>>()
                        .join("; "),
                });
            }
        }
    }
    Ok(())
}

pub fn details_intervals(mut ids: Vec<u32>, max_len: u32) -> Vec<(u32, u32)> {
    ids.sort_unstable();
    ids.dedup();
    let mut out: Vec<(u32, u32)> = Vec::new();
    for id in ids {
        match out.last_mut() {
            Some((from, to)) if id - *from < max_len => *to = id,
            _ => out.push((id, id)),
        }
    }
    out
}

pub fn details_bound(items: usize, limit: usize) -> Result<(), DcsError> {
    if items > limit {
        return Err(DcsError::TooManyDetails { limit });
    }
    Ok(())
}

pub fn details_request(address: &str, from: u32, to: u32) -> Vec<u8> {
    format!(
        "\u{feff}<?xml version=\"1.0\" encoding=\"UTF-8\"?><request xmlns=\"{NS_WS}\" xmlns:xs=\"{NS_XS}\" \
xmlns:xsi=\"{NS_XSI}\"><detailsInformationKey>{}</detailsInformationKey><needParents>true</needParents>\
<idsInterval><from>{from}</from><to>{to}</to></idsInterval></request>",
        escape_text(address)
    )
    .into_bytes()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailsValue {
    pub field: String,
    pub type_name: Option<String>,
    pub text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailsItem {
    pub id: u32,
    pub parent: Option<u32>,
    pub values: Vec<DetailsValue>,
    pub main_action: Option<String>,
}

pub fn read_details(body: &[u8]) -> Result<Vec<DetailsItem>, DcsError> {
    const ANSWER: &str = "dcs/loadDetailsPart";
    let text = utf8(body);
    let doc = parse(ANSWER, &text)?;
    let root = doc.root_element();
    exception(ANSWER, root, body)?;
    let Some(details) = root.children().find(|n| is(*n, NS_WS, "details")) else {
        return Err(AnswerError::Missing {
            answer: ANSWER,
            field: "details".into(),
        }
        .into());
    };
    let number = |n: Option<Node>, field: &str| -> Result<Option<u32>, DcsError> {
        let Some(n) = n else { return Ok(None) };
        let t = n.text().unwrap_or("");
        unsigned(t).map(Some).ok_or_else(|| {
            AnswerError::Bad {
                answer: ANSWER,
                field: field.into(),
                value: t.to_string(),
            }
            .into()
        })
    };
    let mut out = Vec::new();
    for item in details.children().filter(|n| is(*n, NS_DETAILS, "item")) {
        let id = number(child_any(item, "ID"), "ID")?.ok_or(AnswerError::Missing {
            answer: ANSWER,
            field: "ID".into(),
        })?;
        let values = item
            .children()
            .filter(|n| n.is_element() && n.tag_name().name() == "fieldValue")
            .map(|fv| {
                let field = child_any(fv, "field")
                    .and_then(|f| f.text())
                    .unwrap_or("")
                    .to_string();
                let v = child_any(fv, "value");
                let nil = v.is_some_and(|v| v.attribute((NS_XSI, "nil")) == Some("true"));
                DetailsValue {
                    field,
                    type_name: v
                        .and_then(|v| v.attribute((NS_XSI, "type")).map(|t| usual_qname(v, t))),
                    text: v
                        .filter(|_| !nil)
                        .map(|v| value_of(v).text)
                        .filter(|t| !t.is_empty()),
                }
            })
            .collect();
        out.push(DetailsItem {
            id,
            parent: number(child_any(item, "parentID"), "parentID")?,
            values,
            main_action: child_any(item, "mainAction")
                .and_then(|n| n.text())
                .map(str::to_string),
        });
    }
    Ok(out)
}

fn utf8(body: &[u8]) -> String {
    String::from_utf8_lossy(body.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(body)).into_owned()
}

fn parse<'i>(answer: &'static str, text: &'i str) -> Result<Document<'i>, AnswerError> {
    Document::parse(text).map_err(|e| AnswerError::Parse {
        answer,
        message: e.to_string(),
    })
}

fn exception(answer: &'static str, root: Node, body: &[u8]) -> Result<(), AnswerError> {
    if is(root, NS_VRS, "exception") {
        return Err(AnswerError::Exception {
            answer,
            descr: xml::parse_exception(body),
        });
    }
    Ok(())
}

fn is(n: Node, ns: &str, local: &str) -> bool {
    n.is_element() && n.tag_name().namespace() == Some(ns) && n.tag_name().name() == local
}

fn uo_child<'a, 'i>(n: Node<'a, 'i>, ns: &str, local: &str) -> Option<Node<'a, 'i>> {
    n.children().find(|c| is(*c, ns, local))
}

fn uo_values<'a, 'i>(n: Node<'a, 'i>) -> Vec<Node<'a, 'i>> {
    n.children().filter(|c| is(*c, NS_UO, "v")).collect()
}

fn child_any<'a, 'i>(n: Node<'a, 'i>, local: &str) -> Option<Node<'a, 'i>> {
    n.children()
        .find(|c| c.is_element() && c.tag_name().name() == local)
}

fn usual_qname(n: Node, value: &str) -> String {
    let Some((prefix, local)) = value.split_once(':') else {
        return value.to_string();
    };
    let usual = match n.lookup_namespace_uri(Some(prefix)) {
        Some(NS_CORE) => "v8",
        Some(NS_CONFIG) => "cfg",
        Some(NS_XS) => "xs",
        _ => return value.to_string(),
    };
    format!("{usual}:{local}")
}

fn raw(text: &str, n: Node) -> String {
    let src = &text[n.range()];
    let name_end = src
        .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
        .unwrap_or(src.len());
    let parent: Vec<(Option<&str>, &str)> = n
        .parent_element()
        .map(|p| p.namespaces().map(|ns| (ns.name(), ns.uri())).collect())
        .unwrap_or_default();
    let own = |prefix: Option<&str>, uri: &str| {
        !parent.contains(&(prefix, uri)) || declares(start_tag_of(src), prefix)
    };
    let mut decls = String::new();
    for ns in n.namespaces() {
        if ns.name() == Some("xml") || own(ns.name(), ns.uri()) {
            continue;
        }
        match ns.name() {
            Some(p) => decls.push_str(&format!(" xmlns:{p}=\"{}\"", xml::escape_attr(ns.uri()))),
            None => decls.push_str(&format!(" xmlns=\"{}\"", xml::escape_attr(ns.uri()))),
        }
    }
    format!("{}{decls}{}", &src[..name_end], &src[name_end..])
}

fn start_tag_of(src: &str) -> &str {
    let mut quote = None;
    for (i, c) in src.char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, '>') => return &src[..i],
            _ => {}
        }
    }
    src
}

fn declares(start_tag: &str, prefix: Option<&str>) -> bool {
    let name = prefix.map_or_else(|| "xmlns".to_string(), |p| format!("xmlns:{p}"));
    let mut rest = start_tag;
    while let Some(i) = rest.find(&name) {
        let before = rest[..i].chars().next_back();
        let after = rest[i + name.len()..].trim_start();
        if before.is_some_and(char::is_whitespace) && after.starts_with('=') {
            return true;
        }
        rest = &rest[i + name.len()..];
    }
    false
}
