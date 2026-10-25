use platform_catalog::{
    Avail, Catalog, Context, MethodInfo, ParamInfo, PropertyInfo, Signature, TypeKind, catalog,
};
use serde::Serialize;
use serde_json::{Value, json};

use super::out::Lang;

const MAX_QUERY: usize = 100;

#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct HelperHit {
    pub kind: &'static str,
    pub name: String,
    pub owner: Option<String>,
}

#[derive(Serialize, Debug)]
pub struct HelperType {
    pub name: String,
    pub name_other: String,
    pub kind: String,
    pub avail: Vec<String>,
    pub properties: Vec<Value>,
    pub methods: Vec<Value>,
    pub values: Vec<Value>,
}

fn fold(s: &str) -> String {
    s.chars()
        .take(MAX_QUERY)
        .flat_map(char::to_lowercase)
        .map(|c| if c == 'ё' { 'е' } else { c })
        .collect()
}

fn pick(lang: Lang, ru: &str, en: &str) -> (String, String) {
    match lang {
        Lang::Ru => (ru.to_string(), en.to_string()),
        Lang::En => (en.to_string(), ru.to_string()),
    }
}

fn rank(q: &str, ru: &str, en: &str) -> Option<u8> {
    [ru, en]
        .iter()
        .filter_map(|n| {
            let n = fold(n);
            if n == q {
                Some(0)
            } else if n.starts_with(q) {
                Some(1)
            } else {
                n.contains(q).then_some(2)
            }
        })
        .min()
}

pub fn helper_find(q: &str, lang: Lang, limit: usize) -> (Vec<HelperHit>, usize) {
    let q = fold(q.trim());
    if q.is_empty() {
        return (Vec::new(), 0);
    }
    let cat = catalog();
    let mut found: Vec<(u8, u8, HelperHit)> = Vec::new();
    let mut add = |rank: Option<u8>,
                   order: u8,
                   kind: &'static str,
                   ru: &str,
                   en: &str,
                   owner: Option<String>| {
        if let Some(r) = rank {
            found.push((
                r,
                order,
                HelperHit {
                    kind,
                    name: pick(lang, ru, en).0,
                    owner,
                },
            ));
        }
    };
    for t in cat.types() {
        add(
            rank(&q, &t.name_ru, &t.name_en),
            0,
            "type",
            &t.name_ru,
            &t.name_en,
            None,
        );
    }
    for m in cat.global_methods() {
        let i = &m.info;
        add(
            rank(&q, &i.name_ru, &i.name_en),
            1,
            "global",
            &i.name_ru,
            &i.name_en,
            None,
        );
    }
    for p in cat.global_properties() {
        add(
            rank(&q, &p.name_ru, &p.name_en),
            1,
            "global",
            &p.name_ru,
            &p.name_en,
            None,
        );
    }
    for t in cat.types() {
        let owner = pick(lang, &t.name_ru, &t.name_en).0;
        for m in &t.methods {
            add(
                rank(&q, &m.name_ru, &m.name_en),
                2,
                "method",
                &m.name_ru,
                &m.name_en,
                Some(owner.clone()),
            );
        }
        for p in &t.properties {
            add(
                rank(&q, &p.name_ru, &p.name_en),
                3,
                "property",
                &p.name_ru,
                &p.name_en,
                Some(owner.clone()),
            );
        }
    }
    let total = found.len();
    found.sort_by(|a, b| (a.0, a.1, &a.2.name, &a.2.owner).cmp(&(b.0, b.1, &b.2.name, &b.2.owner)));
    let hits = found.into_iter().take(limit).map(|(_, _, h)| h).collect();
    (hits, total)
}

const CONTEXTS: [(Context, &str, &str); 10] = [
    (Context::ThinClient, "Тонкий клиент", "Thin client"),
    (Context::WebClient, "Веб-клиент", "Web client"),
    (Context::MobileClient, "Мобильный клиент", "Mobile client"),
    (Context::Server, "Сервер", "Server"),
    (Context::ThickClient, "Толстый клиент", "Thick client"),
    (
        Context::ExternalConnection,
        "Внешнее соединение",
        "External connection",
    ),
    (
        Context::MobileAppClient,
        "Клиент мобильного приложения",
        "Mobile app client",
    ),
    (
        Context::MobileAppServer,
        "Сервер мобильного приложения",
        "Mobile app server",
    ),
    (
        Context::MobileStandaloneServer,
        "Сервер автономного мобильного приложения",
        "Mobile standalone server",
    ),
    (Context::Integration, "Интеграция", "Integration"),
];

fn avail_names(a: Avail, lang: Lang) -> Vec<String> {
    CONTEXTS
        .iter()
        .filter(|(c, _, _)| a.has(*c))
        .map(|(_, ru, en)| pick(lang, ru, en).0)
        .collect()
}

fn signature(name: &str, s: &Signature, lang: Lang) -> String {
    let params: Vec<String> = s
        .params
        .iter()
        .map(|p: &ParamInfo| {
            let n = pick(lang, &p.name_ru, &p.name_en).0;
            let n = if p.variadic { format!("{n}...") } else { n };
            if p.optional { format!("[{n}]") } else { n }
        })
        .collect();
    format!("{name}({})", params.join(", "))
}

fn method_json(m: &MethodInfo, lang: Lang) -> Value {
    let (name, other) = pick(lang, &m.name_ru, &m.name_en);
    let signatures: Vec<String> = if m.variants.is_empty() {
        vec![format!("{name}()")]
    } else {
        m.variants
            .iter()
            .map(|s| signature(&name, s, lang))
            .collect()
    };
    json!({
        "name": name, "name_other": other, "signatures": signatures,
        "returns": m.returns_value, "avail": avail_names(m.avail, lang),
    })
}

fn property_json(p: &PropertyInfo, lang: Lang) -> Value {
    let (name, other) = pick(lang, &p.name_ru, &p.name_en);
    json!({
        "name": name, "name_other": other, "readable": p.readable, "writable": p.writable,
        "avail": avail_names(p.avail, lang),
    })
}

pub fn helper_type(name: &str, lang: Lang) -> Option<HelperType> {
    let cat: &Catalog = catalog();
    let t = cat.type_info(cat.find_type(name.trim())?);
    let (n, other) = pick(lang, &t.name_ru, &t.name_en);
    Some(HelperType {
        name: n,
        name_other: other,
        kind: match t.kind {
            TypeKind::Primitive => "primitive",
            TypeKind::Class => "class",
            TypeKind::Enum => "enum",
        }
        .to_string(),
        avail: avail_names(t.avail, lang),
        properties: t
            .properties
            .iter()
            .map(|p| property_json(p, lang))
            .collect(),
        methods: t.methods.iter().map(|m| method_json(m, lang)).collect(),
        values: t
            .values
            .iter()
            .map(|v| {
                let (n, o) = pick(lang, &v.name_ru, &v.name_en);
                json!({ "name": n, "name_other": o })
            })
            .collect(),
    })
}
