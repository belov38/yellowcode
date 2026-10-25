use super::{ObjMeta, QField, QTable, QTableId, QuerySchema, TableKind, VtParam, VtParamKind};
use crate::schema::FieldRole;
use mdschema::MdKind;
use platform_catalog::{Catalog, QueryTableInfo};
use std::collections::{HashMap, HashSet};

pub(crate) const P_DIM: &str = "<Имя измерения>";
pub(crate) const P_RES: &str = "<Имя ресурса>";
pub(crate) const P_ATTR: &str = "<Имя реквизита>";
pub(crate) const P_COMMON: &str = "<Имя общего реквизита>";
pub(crate) const P_EXT: &str = "<Номер субконто>";
pub(crate) const P_SCHED: &str = "<Имя ресурса графика>";
pub(crate) const P_BASE_RES: &str = "<Имя ресурса базового регистра>";

fn key_of(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut depth = 0u32;
    for ch in name.chars() {
        match ch {
            '<' => {
                if depth == 0 {
                    out.push('*');
                }
                depth = depth.saturating_add(1);
            }
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    out
}

fn seg_match(pat: &str, seg: &str) -> Option<usize> {
    match pat.split_once('*') {
        None => (pat == seg).then_some(pat.len()),
        Some((pre, suf)) => {
            (seg.len() >= pre.len() + suf.len() && seg.starts_with(pre) && seg.ends_with(suf))
                .then_some(pre.len() + suf.len())
        }
    }
}

fn key_match(tpl_key: &str, ours: &str) -> Option<usize> {
    let a: Vec<&str> = tpl_key.split('.').collect();
    let b: Vec<&str> = ours.split('.').collect();
    if a.len() != b.len() {
        return None;
    }
    a.iter()
        .zip(&b)
        .try_fold(0usize, |acc, (p, s)| Some(acc + seg_match(p, s)?))
}

fn without_correspondence(cat: &Catalog, t: &QueryTableInfo) -> bool {
    t.group
        .as_ref()
        .and_then(|g| cat.query_table_groups().iter().find(|x| &x.id == g))
        .is_some_and(|g| g.name_en.contains("without correspondence"))
}

pub(crate) fn find<'c>(cat: &'c Catalog, key: &str, nc: bool) -> Option<&'c QueryTableInfo> {
    let mut best: Option<(usize, bool, &QueryTableInfo)> = None;
    for t in cat.query_tables() {
        let Some(score) = key_match(&key_of(&t.name_en), key) else {
            continue;
        };
        let group_ok = without_correspondence(cat, t) == nc;
        let better = match best {
            None => true,
            Some((s, g, _)) => (group_ok, score) > (g, s),
        };
        if better {
            best = Some((score, group_ok, t));
        }
    }
    best.map(|(_, _, t)| t)
}

pub(crate) fn first_segment(t: &QueryTableInfo) -> (String, String) {
    let first = |n: &str| n.split('.').next().unwrap_or_default().to_string();
    (first(&t.name_ru), first(&t.name_en))
}

pub(crate) fn last_segment(t: &QueryTableInfo) -> (String, String) {
    let last = |n: &str| key_of(n.rsplit('.').next().unwrap_or_default()).replace('*', "");
    (last(&t.name_ru), last(&t.name_en))
}

fn split_placeholder(n: &str) -> Option<(&str, String, &str)> {
    let (pre, rest) = n.split_once('<')?;
    let (body, suf) = rest.split_once('>')?;
    Some((pre, format!("<{body}>"), suf))
}

#[derive(Clone, Debug)]
struct Pattern {
    ph: String,
    ru_pre: String,
    ru_suf: String,
    en_pre: String,
    en_suf: String,
}

impl Pattern {
    fn parse(ru: &str, en: &str) -> Option<Pattern> {
        let (ru_pre, ph, ru_suf) = split_placeholder(ru)?;
        let (en_pre, en_suf) = match split_placeholder(en) {
            Some((p, _, s)) => (p.to_string(), s.to_string()),
            None => (String::new(), String::new()),
        };
        Some(Pattern {
            ph,
            ru_pre: ru_pre.to_string(),
            ru_suf: ru_suf.to_string(),
            en_pre,
            en_suf,
        })
    }

    fn ru(&self, item: &str) -> String {
        format!("{}{item}{}", self.ru_pre, self.ru_suf)
    }

    fn en(&self, item: &str) -> String {
        format!("{}{item}{}", self.en_pre, self.en_suf)
    }

    fn item_of<'n>(&self, ru: &'n str) -> Option<&'n str> {
        let x = ru.strip_prefix(&self.ru_pre)?.strip_suffix(&self.ru_suf)?;
        (!x.is_empty()).then_some(x)
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct TplNames {
    fixed: HashMap<String, String>,
    patterns: Vec<Pattern>,
}

impl TplNames {
    pub fn new(t: Option<&QueryTableInfo>) -> TplNames {
        let mut out = TplNames::default();
        for f in t.map(|t| t.fields.as_slice()).unwrap_or_default() {
            match Pattern::parse(&f.name_ru, &f.name_en) {
                Some(p) => out.patterns.push(p),
                None => {
                    out.fixed.insert(f.name_ru.clone(), f.name_en.clone());
                }
            }
        }
        out
    }

    pub fn fixed(&self, ru: &str) -> Option<&str> {
        self.fixed.get(ru).map(String::as_str)
    }

    pub fn pattern(
        &self,
        ph: &str,
        ru_pre: &str,
        ru_suf: &str,
        item: &str,
    ) -> Option<(String, String)> {
        let p = self
            .patterns
            .iter()
            .find(|p| p.ph == ph && p.ru_pre == ru_pre && p.ru_suf == ru_suf)?;
        Some((p.ru(item), p.en(item)))
    }
}

fn param_kind(en: &str) -> Option<VtParamKind> {
    Some(match en {
        "Period" | "Beginning of period" | "End of period" => VtParamKind::Period,
        "Periodicity" => VtParamKind::Periodicity,
        "Condition" => VtParamKind::Condition,
        "Complement method" => VtParamKind::ComplementMethod,
        "Account condition"
        | "Balanced account condition"
        | "AccountDr condition"
        | "Account Cr condition" => VtParamKind::AccountCondition,
        "Extra dimensions"
        | "Balanced extra dimensions"
        | "Extra dimensions Dr"
        | "Extra dimensions Cr" => VtParamKind::ExtDimensions,
        "Order" => VtParamKind::Order,
        "Top" => VtParamKind::Top,
        "Main register dimensions" | "Base register dimensions" | "View points" => {
            VtParamKind::DimensionList
        }
        "Performer" | "Value" => VtParamKind::Value,
        _ => return None,
    })
}

pub(crate) fn params(t: &QueryTableInfo) -> Result<Vec<VtParam>, String> {
    t.params
        .iter()
        .map(|p| match param_kind(&p.name_en) {
            Some(kind) => Ok(VtParam {
                name_ru: p.name_ru.clone(),
                name_en: p.name_en.clone(),
                kind,
            }),
            None => Err(format!("{}: unknown parameter {}", t.name_en, p.name_en)),
        })
        .collect()
}

fn table_key(t: &QTable) -> Option<String> {
    match t.kind {
        TableKind::Tabular => None,
        TableKind::Recalculation => Some("CalculationRegister.*.*".to_string()),
        TableKind::Constants => Some(t.name_en.clone()),
        _ => {
            let mut segs: Vec<&str> = t.name_en.split('.').collect();
            if let Some(s) = segs.get_mut(1) {
                *s = "*";
            }
            Some(segs.join("."))
        }
    }
}

fn is_nc(meta: Option<&ObjMeta>) -> bool {
    meta.is_some_and(|m| m.kind == MdKind::AccountingRegister && !m.corr)
}

pub fn template_of<'c>(
    s: &QuerySchema,
    cat: &'c Catalog,
    t: QTableId,
) -> Option<&'c QueryTableInfo> {
    let key = table_key(s.table(t))?;
    find(cat, &key, is_nc(s.meta(s.root(t))))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cond {
    Required,
    Optional,
    Never,
    If(bool),
}

const OPTIONAL_REF_FIELDS: [&str; 12] = [
    "Владелец",
    "Родитель",
    "ЭтоГруппа",
    "Код",
    "Наименование",
    "Номер",
    "Порядок",
    "ВидыСубконто",
    "ПериодДействияБазовый",
    "ВедущиеВидыРасчета",
    "БазовыеВидыРасчета",
    "ВытесняющиеВидыРасчета",
];

const NOT_IN_TEMPLATE: [(&str, &str); 4] = [
    ("Enum.*", "Presentation"),
    ("DocumentJournal.*", "Presentation"),
    ("ChartOfCalculationTypes.*", "DataVersion"),
    (
        "AccountingRegister.*.RecordsWithExtDimensions",
        "RecordType",
    ),
];

const EN_QUIRKS: [(&str, &str, &str); 1] = [(
    "CalculationRegister.*.Base*",
    "ПериодДействияНачало",
    "BegOfActionPeriod",
)];

fn fixed_cond(tkey: &str, ru: &str, meta: Option<&ObjMeta>) -> Cond {
    let kind = tkey.split('.').next().unwrap_or_default();
    let last = tkey.rsplit('.').next().unwrap_or_default();
    let m = meta
        .cloned()
        .unwrap_or_else(|| ObjMeta::new(MdKind::Catalog));
    match kind {
        "InformationRegister" => match (last, ru) {
            ("*", "Период") => Cond::If(m.periodic),
            ("*" | "SliceLast" | "SliceFirst", "Регистратор" | "НомерСтроки" | "Активность") => {
                Cond::If(m.subordinate)
            }
            ("*", "МоментВремени") => Cond::If(m.subordinate),
            ("Changes", "Период") => {
                Cond::If(m.periodic && m.filter_on_period && !m.subordinate)
            }
            ("Changes", "Регистратор") => Cond::If(m.subordinate),
            _ => Cond::Required,
        },
        "AccumulationRegister" if last == "*" && ru == "ВидДвижения" => {
            Cond::If(m.balance_type)
        }
        "AccountingRegister" if ru == "УточнениеПериода" => match last {
            "*" | "ExtDimensions" => Cond::If(m.adjustment),
            _ => Cond::Never,
        },
        "CalculationRegister" => match ru {
            "ПериодДействия" | "ПериодДействияНачало" | "ПериодДействияКонец" => {
                Cond::If(m.action_period)
            }
            "БазовыйПериодНачало" | "БазовыйПериодКонец" => {
                Cond::If(m.base_period)
            }
            _ => Cond::Required,
        },
        "DocumentJournal" if ru == "Номер" => Cond::Optional,
        _ if meta.is_none() && OPTIONAL_REF_FIELDS.contains(&ru) => Cond::Optional,
        _ => Cond::Required,
    }
}

fn names_of(items: &[super::Item], keep: impl Fn(&super::Item) -> bool) -> Vec<String> {
    items
        .iter()
        .filter(|i| keep(i))
        .map(|i| i.name.clone())
        .collect()
}

fn pattern_items(
    s: &QuerySchema,
    t: &QTable,
    tpl: &QueryTableInfo,
    tkey: &str,
    p: &Pattern,
    meta: Option<&ObjMeta>,
) -> Option<Vec<String>> {
    if t.kind == TableKind::Recalculation {
        return (p.ph == P_DIM).then(|| {
            t.fields
                .iter()
                .filter(|f| f.role == FieldRole::Dimension)
                .map(|f| f.name_ru.clone())
                .collect()
        });
    }
    let m = meta?;
    let kind = tkey.split('.').next().unwrap_or_default();
    let last = tkey.rsplit('.').next().unwrap_or_default();
    let corr = m.kind == MdKind::AccountingRegister && m.corr;
    let suf = p.ru_suf.as_str();
    let side = suf == "Дт" || suf == "Кт";
    match p.ph.as_str() {
        P_DIM => Some(match (kind, last) {
            ("AccountingRegister", "*" | "RecordsWithExtDimensions" | "DrCrTurnovers") if corr => {
                names_of(&m.dims, |d| d.balance != side)
            }
            ("AccountingRegister", "Turnovers") if corr && suf == "Кор" => {
                names_of(&m.dims, |d| !d.balance)
            }
            ("InformationRegister", "Changes") if m.subordinate => Vec::new(),
            ("InformationRegister", "Changes") => names_of(&m.dims, |d| d.main_filter),
            _ if last == "Changes" => Vec::new(),
            _ => names_of(&m.dims, |_| true),
        }),
        P_RES => Some(match (kind, last) {
            ("AccumulationRegister", _) if suf == "Приход" || suf == "Расход" => {
                if m.balance_type {
                    names_of(&m.resources, |_| true)
                } else {
                    Vec::new()
                }
            }
            ("AccountingRegister", "*" | "RecordsWithExtDimensions") if corr => {
                names_of(&m.resources, |r| r.balance != side)
            }
            ("AccountingRegister", "Turnovers") if corr && suf.starts_with("Кор") => {
                names_of(&m.resources, |r| !r.balance)
            }
            ("AccountingRegister", "DrCrTurnovers") => {
                let split = suf != "Оборот";
                names_of(&m.resources, |r| r.balance != split)
            }
            _ => names_of(&m.resources, |_| true),
        }),
        P_ATTR => Some(names_of(&m.attrs, |_| true)),
        P_COMMON if last == "Changes" => None,
        P_COMMON => Some(m.commons.clone()),
        P_EXT => Some((1..=m.ext_dims).map(|n| n.to_string()).collect()),
        P_SCHED => Some(m.schedule_resource.iter().map(|r| r.name.clone()).collect()),
        P_BASE_RES => {
            let seg = t.name_ru.rsplit('.').next().unwrap_or_default();
            let base_name = seg.strip_prefix(&last_segment(tpl).0)?;
            let base = s.find_table(&[kind, base_name])?;
            Some(names_of(&s.meta(base)?.resources, |_| true))
        }
        _ => None,
    }
}

pub fn missing_template_fields(s: &QuerySchema, cat: &Catalog) -> Vec<String> {
    let mut out = Vec::new();
    for (i, t) in s.tables().iter().enumerate() {
        let Ok(i) = u32::try_from(i) else { break };
        let id = QTableId(i);
        let Some(key) = table_key(t) else { continue };
        let meta = s.meta(s.root(id));
        let Some(tpl) = find(cat, &key, is_nc(meta)) else {
            out.push(format!("{}: no platform template", t.name_en));
            continue;
        };
        check_table(s, t, tpl, meta, &mut out);
    }
    out
}

#[derive(Clone, Copy)]
enum Look {
    Std,
    Item,
    Any,
}

fn field_by_ru<'t>(t: &'t QTable, ru: &str, look: Look) -> Option<&'t QField> {
    t.fields.iter().find(|f| {
        let std = matches!(f.role, FieldRole::Std(_) | FieldRole::Virtual);
        f.name_ru == ru
            && match look {
                Look::Std => std,
                Look::Item => !std,
                Look::Any => true,
            }
    })
}

fn check_table(
    s: &QuerySchema,
    t: &QTable,
    tpl: &QueryTableInfo,
    meta: Option<&ObjMeta>,
    out: &mut Vec<String>,
) {
    let tkey = key_of(&tpl.name_en);
    let quirk = |ru: &str, en: &str| {
        EN_QUIRKS
            .iter()
            .any(|(k, r, e)| *k == tkey && *r == ru && *e == en)
    };
    let mut patterns = Vec::new();
    for f in &tpl.fields {
        if let Some(p) = Pattern::parse(&f.name_ru, &f.name_en) {
            patterns.push(p);
            continue;
        }
        let cond = fixed_cond(&tkey, &f.name_ru, meta);
        match field_by_ru(t, &f.name_ru, Look::Std) {
            Some(ours) if ours.name_en != f.name_en && !quirk(&f.name_ru, &ours.name_en) => {
                out.push(format!(
                    "{}.{}: English name {}, template {}",
                    t.name_en, f.name_ru, ours.name_en, f.name_en
                ));
            }
            Some(_) => {}
            None if cond == Cond::Required || cond == Cond::If(true) => {
                out.push(format!(
                    "{}: no field {} ({})",
                    t.name_en, f.name_ru, f.name_en
                ));
            }
            None => {}
        }
    }
    let mut seen: HashSet<String> = HashSet::new();
    for p in &patterns {
        let Some(items) = pattern_items(s, t, tpl, &tkey, p, meta) else {
            continue;
        };
        for item in items {
            let (ru, en) = (p.ru(&item), p.en(&item));
            seen.insert(ru.clone());
            let look = if p.ru_pre.is_empty() && p.ru_suf.is_empty() {
                Look::Item
            } else {
                Look::Any
            };
            match field_by_ru(t, &ru, look) {
                Some(ours) if ours.name_en != en => out.push(format!(
                    "{}.{ru}: English name {}, template {en}",
                    t.name_en, ours.name_en
                )),
                Some(_) => {}
                None => out.push(format!("{}: no field {ru} ({en})", t.name_en)),
            }
        }
    }
    for f in &t.fields {
        if !matches!(f.role, FieldRole::Std(_) | FieldRole::Virtual) {
            continue;
        }
        if seen.contains(&f.name_ru) || tpl.fields.iter().any(|x| x.name_ru == f.name_ru) {
            continue;
        }
        let best = patterns
            .iter()
            .filter(|p| !(p.ru_pre.is_empty() && p.ru_suf.is_empty()))
            .filter(|p| p.item_of(&f.name_ru).is_some())
            .max_by_key(|p| p.ru_pre.len() + p.ru_suf.len());
        match best.and_then(|p| Some((p, p.item_of(&f.name_ru)?))) {
            Some((p, item)) if p.en(item) != f.name_en => out.push(format!(
                "{}.{}: English name {}, template {}",
                t.name_en,
                f.name_ru,
                f.name_en,
                p.en(item)
            )),
            Some(_) => {}
            None if NOT_IN_TEMPLATE
                .iter()
                .any(|(k, e)| *k == tkey && *e == f.name_en) => {}
            None => out.push(format!(
                "{}.{} ({}): not in the template",
                t.name_en, f.name_ru, f.name_en
            )),
        }
    }
}
