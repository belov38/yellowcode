use std::collections::BTreeSet;
use std::sync::LazyLock;

use regex::Regex;

pub(crate) const RU_KINDS: [(&str, &str); 20] = [
    ("Справочник", "Catalog"),
    ("Документ", "Document"),
    ("РегистрСведений", "InformationRegister"),
    ("РегистрНакопления", "AccumulationRegister"),
    ("Перечисление", "Enum"),
    ("ПланВидовХарактеристик", "ChartOfCharacteristicTypes"),
    ("ПланСчетов", "ChartOfAccounts"),
    ("РегистрБухгалтерии", "AccountingRegister"),
    ("ПланВидовРасчета", "ChartOfCalculationTypes"),
    ("РегистрРасчета", "CalculationRegister"),
    ("БизнесПроцесс", "BusinessProcess"),
    ("Задача", "Task"),
    ("ПланОбмена", "ExchangePlan"),
    ("ЖурналДокументов", "DocumentJournal"),
    ("Константа", "Constant"),
    ("КритерийОтбора", "FilterCriterion"),
    ("Catalog", "Catalog"),
    ("Document", "Document"),
    ("InformationRegister", "InformationRegister"),
    ("AccumulationRegister", "AccumulationRegister"),
];

pub(crate) const STD_RU: [(&str, &str); 33] = [
    ("Ссылка", "Ref"),
    ("ПометкаУдаления", "DeletionMark"),
    ("Наименование", "Description"),
    ("Код", "Code"),
    ("Номер", "Number"),
    ("Дата", "Date"),
    ("Проведен", "Posted"),
    ("Родитель", "Parent"),
    ("Владелец", "Owner"),
    ("ЭтоГруппа", "IsFolder"),
    ("Предопределенный", "Predefined"),
    ("ИмяПредопределенныхДанных", "PredefinedDataName"),
    ("Регистратор", "Recorder"),
    ("Период", "Period"),
    ("НомерСтроки", "LineNumber"),
    ("Активность", "Active"),
    ("ВидДвижения", "RecordType"),
    ("Завершен", "Completed"),
    ("Стартован", "Started"),
    ("ВедущаяЗадача", "HeadTask"),
    ("Выполнена", "Executed"),
    ("БизнесПроцесс", "BusinessProcess"),
    ("ТочкаМаршрута", "RoutePoint"),
    ("Порядок", "Order"),
    ("Вид", "Kind"),
    ("ТипЗначения", "ValueType"),
    ("Забалансовый", "OffBalance"),
    ("ПериодДействия", "ActionPeriod"),
    ("ПериодРегистрации", "RegistrationPeriod"),
    ("Счет", "Account"),
    ("ЭтотУзел", "ThisNode"),
    ("НомерОтправленного", "SentNo"),
    ("НомерПринятого", "ReceivedNo"),
];

const KW_SELECT: &str = "(?:ВЫБРАТЬ|SELECT)";
const KW_FROM: &str = "(?:ИЗ|FROM)";

struct Res {
    comment: Regex,
    string: Regex,
    select: Regex,
    select_word: Regex,
    from_or_paren: Regex,
    lead: Regex,
    alias: Regex,
    tail_name: Regex,
    name: Regex,
    table: Regex,
}

static RES: LazyLock<Option<Res>> = LazyLock::new(|| {
    let kinds: Vec<&str> = RU_KINDS.iter().map(|(k, _)| *k).collect();
    Some(Res {
        comment: Regex::new(r"//[^\n]*").ok()?,
        string: Regex::new(r#""(?:[^"]|"")*""#).ok()?,
        select: Regex::new(&format!("(?i){KW_SELECT}")).ok()?,
        select_word: Regex::new(&format!(r"(?i){KW_SELECT}\b")).ok()?,
        from_or_paren: Regex::new(&format!(r"(?i)[(){{}}]|{KW_FROM}\b")).ok()?,
        lead: Regex::new(
            r"(?i)^\s*(?:РАЗЛИЧНЫЕ|DISTINCT|ПЕРВЫЕ\s+\d+|TOP\s+\d+|РАЗРЕШЕННЫЕ|ALLOWED)\b",
        )
        .ok()?,
        alias: Regex::new(r"(?i)(?:КАК|AS)\s+([^\W\d][\w]*)\s*$").ok()?,
        tail_name: Regex::new(r"([^\W\d][\w]*)\s*$").ok()?,
        name: Regex::new(r"\A[^\W\d]\w*\z").ok()?,
        table: Regex::new(&format!(r"({})\.([^\W\d]\w*)", kinds.join("|"))).ok()?,
    })
});

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn after_word_or_dot(s: &str, at: usize) -> bool {
    s.get(..at)
        .and_then(|h| h.chars().next_back())
        .is_some_and(|c| is_word(c) || c == '.')
}

pub(crate) fn py_strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
}

pub(crate) fn strip(q: &str) -> String {
    let Some(r) = RES.as_ref() else {
        return q.to_string();
    };
    let q = r.comment.replace_all(q, "");
    r.string.replace_all(&q, "\"\"").into_owned()
}

fn split_top(s: &str, sep: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth: i64 = 0;
    let mut cur = String::new();
    for ch in s.chars() {
        if ch == '(' || ch == '{' {
            depth = depth.saturating_add(1);
        } else if ch == ')' || ch == '}' {
            depth = depth.saturating_sub(1);
        }
        if ch == sep && depth == 0 {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(ch);
        }
    }
    out.push(cur);
    out
}

pub(crate) fn result_fields(q: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let Some(r) = RES.as_ref() else {
        return out;
    };
    let q = strip(q);
    let parts: Vec<String> = split_top(&q, ';')
        .into_iter()
        .filter(|p| r.select.is_match(p))
        .collect();
    let Some(last) = parts.last() else {
        return out;
    };
    let Some(m) = r.select_word.find(last) else {
        return out;
    };
    let rest = last.get(m.end()..).unwrap_or_default();
    let mut depth: i64 = 0;
    let mut end = None;
    for f in r.from_or_paren.find_iter(rest) {
        match f.as_str() {
            "(" | "{" => depth = depth.saturating_add(1),
            ")" | "}" => depth = depth.saturating_sub(1),
            _ if after_word_or_dot(rest, f.start()) => {}
            _ if depth == 0 => {
                end = m.end().checked_add(f.start());
                break;
            }
            _ => {}
        }
    }
    let Some(sel) = end.and_then(|e| last.get(m.end()..e)) else {
        return out;
    };
    let sel = r.lead.replace(py_strip(sel), "");
    for it in split_top(&sel, ',') {
        let it = py_strip(py_strip(&it).trim_matches(['{', '}']));
        if it.is_empty() {
            continue;
        }
        if let Some(a) = r.alias.captures(it).and_then(|c| c.get(1)) {
            out.insert(a.as_str().to_string());
            continue;
        }
        if let Some(b) = r.tail_name.captures(it).and_then(|c| c.get(1)) {
            out.insert(b.as_str().to_string());
        }
        let segs: Vec<&str> = it.split('.').collect();
        if segs.len() >= 3 && segs.iter().all(|x| r.name.is_match(py_strip(x))) {
            out.insert(segs.iter().skip(1).map(|x| py_strip(x)).collect());
        }
    }
    out
}

#[allow(dead_code)]
pub(crate) fn tables_exist(q: &str, existing: &dyn Fn(&str) -> bool) -> bool {
    let Some(r) = RES.as_ref() else {
        return true;
    };
    let q = strip(q);
    for c in r.table.captures_iter(&q) {
        let (Some(all), Some(kind), Some(name)) = (c.get(0), c.get(1), c.get(2)) else {
            continue;
        };
        if after_word_or_dot(&q, all.start()) {
            continue;
        }
        let Some((_, en)) = RU_KINDS.iter().find(|(ru, _)| *ru == kind.as_str()) else {
            continue;
        };
        if !existing(&format!("{en}.{}", name.as_str())) {
            return false;
        }
    }
    true
}
