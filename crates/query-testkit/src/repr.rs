mod managed;
pub use managed::{type_repr_budgeted, write_type_repr_budgeted};

use query_ir::{DateQ, Other, QType, QuerySchema};
use query_syntax::Lang;

pub fn type_repr(s: &QuerySchema, t: &QType, lang: Lang) -> String {
    let parts: Vec<String> = s
        .atom_uuid(t)
        .into_iter()
        .map(|(_, a)| atom(s, &a, lang))
        .collect();
    format!("ОписаниеТипов({})", parts.join(", "))
}

fn pick(lang: Lang, ru: &str, en: &str) -> String {
    match lang {
        Lang::Ru => ru,
        Lang::En => en,
    }
    .to_string()
}

fn atom(s: &QuerySchema, a: &QType, lang: Lang) -> String {
    if a.null {
        return platform("Null", lang);
    }
    if a.boolean {
        return platform("Boolean", lang);
    }
    if let Some(q) = a.number {
        let sign = if q.nonneg {
            ",Неотрицательный".to_string()
        } else {
            String::new()
        };
        return format!("Число({},{}{sign})", q.digits, q.scale);
    }
    if let Some(q) = a.string {
        let kind = if q.fixed {
            pick(lang, "Фиксированная", "Fixed-length")
        } else {
            pick(lang, "Переменная", "Variable-length")
        };
        return format!("Строка({},{kind})", q.len);
    }
    if let Some(q) = a.date {
        let part = match q {
            DateQ::Date => pick(lang, "Дата", "Date"),
            DateQ::Time => pick(lang, "Время", "Time"),
            DateQ::DateTime => pick(lang, "Дата и время", "Date and time"),
        };
        return format!("Дата({part})");
    }
    if let Some(r) = a.refs.first() {
        return s.ref_info(*r).xml_name.clone();
    }
    match a.others.first() {
        Some(Other::Uuid) => platform("UUID", lang),
        Some(Other::Binary) => platform("BinaryData", lang),
        Some(Other::Storage) => platform("ValueStorage", lang),
        Some(Other::Type) => platform("Type", lang),
        Some(Other::TypeDescription) => platform("TypeDescription", lang),
        Some(Other::PointInTime) => platform("PointInTime", lang),
        Some(Other::Boundary) => platform("Boundary", lang),
        Some(Other::QueryResult) => platform("QueryResult", lang),
        Some(Other::ValueTable) => platform("ValueTable", lang),
        Some(Other::RoutePoint(r)) => s.ref_info(*r).xml_name.replacen(
            "BusinessProcessRef",
            "BusinessProcessRoutePointRef",
            1,
        ),
        Some(Other::SysEnum(idx)) => {
            let ti = platform_catalog::catalog().type_info(*idx);
            presentation(ti, lang)
        }
        None => String::new(),
    }
}

pub fn platform(en_name: &str, lang: Lang) -> String {
    platform_text(en_name, lang).to_string()
}

pub fn platform_text(en_name: &str, lang: Lang) -> &str {
    fn folded(value: &str) -> impl Iterator<Item = char> + '_ {
        value.chars().map(|c| {
            let mut lower = c.to_lowercase();
            match (lower.next(), lower.next()) {
                (Some(value), None) => value,
                _ => c,
            }
        })
    }
    let mut bytes = [0u8; 128];
    let mut len = 0usize;
    let mut fits = true;
    for value in folded(en_name) {
        let mut encoded = [0; 4];
        let text = value.encode_utf8(&mut encoded);
        let Some(end) = len.checked_add(text.len()) else {
            fits = false;
            break;
        };
        let Some(target) = bytes.get_mut(len..end) else {
            fits = false;
            break;
        };
        target.copy_from_slice(text.as_bytes());
        len = end;
    }
    let cat = platform_catalog::catalog();
    if fits
        && let Some(key) = bytes
            .get(..len)
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
    {
        return cat.find_type_folded(key).map_or(en_name, |index| {
            presentation_text(cat.type_info(index), lang)
        });
    }
    let matches = |value: &str| folded(value).eq(folded(en_name));
    let mut first = None;
    for info in platform_catalog::catalog()
        .types()
        .iter()
        .take(usize::from(u16::MAX) + 1)
    {
        if matches(&info.name_ru)
            || matches(&info.name_en)
            || info.alt_names.iter().any(|name| matches(name))
        {
            if info.presentation_ru.is_some() {
                return presentation_text(info, lang);
            }
            if first.is_none() {
                first = Some(info);
            }
        }
    }
    first.map_or(en_name, |info| presentation_text(info, lang))
}

fn presentation(ti: &platform_catalog::TypeInfo, lang: Lang) -> String {
    presentation_text(ti, lang).to_string()
}

fn presentation_text(ti: &platform_catalog::TypeInfo, lang: Lang) -> &str {
    match lang {
        Lang::Ru => ti.presentation_ru.as_deref().unwrap_or(&ti.name_ru),
        Lang::En => ti.presentation_en.as_deref().unwrap_or(&ti.name_en),
    }
}
