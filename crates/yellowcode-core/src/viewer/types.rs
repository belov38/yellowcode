use std::collections::HashMap;
use std::sync::OnceLock;

use mdconf::{DateFractions, TypeDescription, TypeRef};
use platform_catalog::catalog;

use super::out::Lang;

fn category_ru() -> &'static HashMap<String, String> {
    static MAP: OnceLock<HashMap<String, String>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut m = HashMap::new();
        for t in catalog().types() {
            if let (Some((en, _)), Some((ru, _))) =
                (t.name_en.split_once(".<"), t.name_ru.split_once(".<"))
            {
                m.entry(en.to_string()).or_insert_with(|| ru.to_string());
            }
        }
        m
    })
}

const FALLBACK_RU: &[(&str, &str)] = &[
    ("DefinedType", "ОпределяемыйТип"),
    ("Characteristic", "Характеристика"),
    ("AnyIBRef", "ЛюбаяСсылка"),
    ("AnyRef", "ЛюбаяСсылка"),
    ("ValueListType", "СписокЗначений"),
    ("SettingsComposer", "КомпоновщикНастроек"),
];

fn plain_ru(name: &str) -> Option<String> {
    let cat = catalog();
    cat.find_type(name)
        .map(|i| cat.type_info(i).name_ru.clone())
        .or_else(|| {
            FALLBACK_RU
                .iter()
                .find(|(en, _)| *en == name)
                .map(|(_, ru)| (*ru).to_string())
        })
}

fn primitive(xml: &str, lang: Lang) -> Option<&'static str> {
    let (ru, en) = match xml {
        "xs:string" => ("Строка", "String"),
        "xs:decimal" => ("Число", "Number"),
        "xs:boolean" => ("Булево", "Boolean"),
        "xs:dateTime" => ("Дата", "Date"),
        "v8:ValueStorage" => ("ХранилищеЗначения", "ValueStorage"),
        "v8:UUID" => ("УникальныйИдентификатор", "UUID"),
        _ => return None,
    };
    Some(if lang == Lang::Ru { ru } else { en })
}

fn one(t: &TypeDescription, r: &TypeRef, lang: Lang) -> String {
    let ru = lang == Lang::Ru;
    match r.qname.as_str() {
        "xs:string" => {
            let name = primitive("xs:string", lang).unwrap_or("String");
            match t.string {
                Some(q) if q.length > 0 => format!("{name}({})", q.length),
                _ => name.to_string(),
            }
        }
        "xs:decimal" => {
            let name = primitive("xs:decimal", lang).unwrap_or("Number");
            match t.number {
                Some(q) => {
                    let sign = match (q.nonnegative, ru) {
                        (true, true) => ", неотр.",
                        (true, false) => ", nonnegative",
                        _ => "",
                    };
                    format!("{name}({}, {}{sign})", q.digits, q.fraction)
                }
                None => name.to_string(),
            }
        }
        "xs:dateTime" => match (t.date, ru) {
            (Some(DateFractions::Date), true) => "Дата".into(),
            (Some(DateFractions::Time), true) => "Время".into(),
            (Some(DateFractions::Date), false) => "Date".into(),
            (Some(DateFractions::Time), false) => "Time".into(),
            (_, true) => "ДатаВремя".into(),
            (_, false) => "DateTime".into(),
        },
        q => {
            if let Some(p) = primitive(q, lang) {
                return p.to_string();
            }
            let local = q.split_once(':').map_or(q, |(_, l)| l);
            if !ru {
                return local.to_string();
            }
            let (prefix, rest) = match local.split_once('.') {
                Some((p, r)) => (p, Some(r)),
                None => (local, None),
            };
            let ru_prefix = category_ru()
                .get(prefix)
                .cloned()
                .or_else(|| plain_ru(prefix));
            match (ru_prefix, rest) {
                (Some(p), Some(r)) => format!("{p}.{r}"),
                (Some(p), None) => p,
                (None, _) => local.to_string(),
            }
        }
    }
}

pub fn type_text(t: &TypeDescription, lang: Lang) -> String {
    t.types
        .iter()
        .map(|r| one(t, r, lang))
        .collect::<Vec<_>>()
        .join(", ")
}
