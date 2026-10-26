use std::io::Write;

use mdconf::{Configuration, Prop, TypeDescription, TypedValue, Value, XmlElem};
use serde::Serialize;
use serde_json::{Map, Value as Json, json};

use super::configurator::palette_prop;
use super::names::{child_tag_en, property_name_of, sub_kind_en};
use super::out::Lang;
use super::path::Target;
use super::types::type_text;

#[derive(Serialize)]
pub struct PropView {
    pub name: String,
    pub title: String,
    pub value: Json,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub view: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tooltip: Option<String>,
}

pub fn show(_cfg: &Configuration, t: &Target, lang: Lang) -> Vec<PropView> {
    match t {
        Target::Configuration(c) => views("Configuration", "Configuration", &c.props, lang),
        Target::Recalc { recalc, .. } => {
            views("Recalculation", "Recalculation", &recalc.props, lang)
        }
        Target::Object(o) => views(o.kind.as_str(), o.kind.as_str(), &o.props, lang),
        Target::Child { owner, chain } => match chain.last() {
            Some(c) => {
                let ctx = chain
                    .iter()
                    .fold(owner.kind.as_str().to_string(), |acc, c| {
                        format!("{acc}.{}", child_tag_en(c.tag))
                    });
                views(child_tag_en(c.tag), &ctx, &c.props, lang)
            }
            None => Vec::new(),
        },
        Target::Sub { owner, sub } => {
            let ctx = format!("{}.{}", owner.kind.as_str(), sub_kind_en(sub.kind));
            views(sub_kind_en(sub.kind), &ctx, &sub.props, lang)
        }
    }
}

fn views(type_en: &str, context: &str, props: &[Prop], lang: Lang) -> Vec<PropView> {
    props
        .iter()
        .map(|p| {
            let (value, text) = convert(&p.value, lang);
            let pal = palette_prop(context, &p.name);
            PropView {
                name: p.name.clone(),
                title: pal
                    .map(|q| q.caption(lang))
                    .filter(|c| !c.is_empty())
                    .map_or_else(|| property_name_of(type_en, &p.name, lang), str::to_string),
                value,
                text,
                group: pal.and_then(|q| q.group(lang)).map(str::to_string),
                view: pal.map(|q| q.view.clone()).filter(|v| !v.is_empty()),
                tooltip: pal
                    .filter(|_| lang == Lang::Ru)
                    .map(|q| q.tooltip_ru.clone())
                    .filter(|v| !v.is_empty()),
            }
        })
        .collect()
}

fn yes_no(b: bool, lang: Lang) -> String {
    match (b, lang) {
        (true, Lang::Ru) => "Да",
        (false, Lang::Ru) => "Нет",
        (true, Lang::En) => "Yes",
        (false, Lang::En) => "No",
    }
    .to_string()
}

fn type_json(t: &TypeDescription) -> Json {
    let mut m = Map::new();
    m.insert(
        "types".into(),
        Json::Array(t.types.iter().map(|r| json!(r.name.to_string())).collect()),
    );
    if let Some(n) = t.number {
        m.insert(
            "number".into(),
            json!({"digits": n.digits, "fraction": n.fraction, "nonnegative": n.nonnegative}),
        );
    }
    if let Some(s) = t.string {
        m.insert(
            "string".into(),
            json!({"length": s.length, "fixed": s.fixed}),
        );
    }
    if let Some(d) = t.date {
        m.insert("date".into(), json!(format!("{d:?}")));
    }
    if let Some(b) = t.binary {
        m.insert(
            "binary".into(),
            json!({"length": b.length, "fixed": b.fixed}),
        );
    }
    Json::Object(m)
}

fn typed(v: &TypedValue, lang: Lang) -> (Json, String) {
    match v {
        TypedValue::Nil | TypedValue::EmptyTypeDescription => (Json::Null, String::new()),
        TypedValue::Str(s)
        | TypedValue::Number(s)
        | TypedValue::Date(s)
        | TypedValue::DesignTimeRef(s) => (json!(s), s.clone()),
        TypedValue::Bool(b) => (json!(b), yes_no(*b, lang)),
        TypedValue::FixedArray(a) => {
            let items: Vec<(Json, String)> = a.iter().map(|x| typed(x, lang)).collect();
            let text = items
                .iter()
                .map(|i| i.1.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            (Json::Array(items.into_iter().map(|i| i.0).collect()), text)
        }
    }
}

fn xml_string(e: &XmlElem, out: &mut String) {
    out.push('<');
    out.push_str(&e.name);
    for (k, v) in &e.attrs {
        out.push_str(&format!(" {k}=\"{}\"", escape(v)));
    }
    if e.children.is_empty() && e.text.is_none() {
        out.push_str("/>");
        return;
    }
    out.push('>');
    if let Some(t) = &e.text {
        out.push_str(&escape(t));
    }
    for c in &e.children {
        xml_string(c, out);
    }
    out.push_str("</");
    out.push_str(&e.name);
    out.push('>');
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn convert(v: &Value, lang: Lang) -> (Json, String) {
    match v {
        Value::Text(s) => (json!(s), s.clone()),
        Value::Bool(b) => (json!(b), yes_no(*b, lang)),
        Value::LStr(l) => {
            let mut m = Map::new();
            for (k, c) in &l.0 {
                m.insert(k.clone(), json!(c));
            }
            let wanted = match lang {
                Lang::Ru => "ru",
                Lang::En => "en",
            };
            let text =
                l.0.iter()
                    .find(|(k, _)| k == wanted)
                    .or_else(|| l.0.first())
                    .map(|(_, c)| c.clone())
                    .unwrap_or_default();
            (Json::Object(m), text)
        }
        Value::Ref(r) => (json!({"ref": r.name}), r.name.clone()),
        Value::Type(t) => {
            let mut j = type_json(t);
            if let Json::Object(m) = &mut j {
                m.entry("types").or_insert(Json::Array(Vec::new()));
            }
            (j, type_text(t, lang))
        }
        Value::Typed(t) => {
            let (j, text) = typed(t, lang);
            (json!({"typed": j}), text)
        }
        Value::RefList(l) => (
            json!({"refs": l.iter().map(|r| json!(r.name)).collect::<Vec<_>>()}),
            l.iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        ),
        Value::FieldList(l) => (json!({"fields": l}), l.join(", ")),
        Value::StandardAttributes(l) => {
            let items: Vec<Json> = l
                .iter()
                .map(|a| {
                    json!({
                        "name": a.name,
                        "props": serde_json::to_value(views("StandardAttribute", "StandardAttribute", &a.props, lang))
                            .unwrap_or(Json::Null),
                    })
                })
                .collect();
            let text = l
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            (json!({"standard_attributes": items}), text)
        }
        Value::Xml(e) => {
            let mut s = String::new();
            xml_string(e, &mut s);
            let text = e.text.clone().unwrap_or_else(|| format!("<{}>", e.name));
            (json!({"xml": s}), text)
        }
    }
}

pub fn render_text(props: &[PropView], out: &mut dyn Write) -> crate::Result<()> {
    render_at(props, out, 0)
}

fn render_at(props: &[PropView], out: &mut dyn Write, indent: usize) -> crate::Result<()> {
    let pad = " ".repeat(indent);
    for p in props {
        let sa = p.value.get("standard_attributes").and_then(Json::as_array);
        match sa {
            Some(items) => {
                writeln!(out, "{pad}{}:", p.title)?;
                for a in items {
                    let name = a.get("name").and_then(Json::as_str).unwrap_or("");
                    writeln!(out, "{pad}  {name}")?;
                    let sub: Vec<PropView> = a
                        .get("props")
                        .and_then(Json::as_array)
                        .map(|l| {
                            l.iter()
                                .filter_map(|x| {
                                    Some(PropView {
                                        name: x.get("name")?.as_str()?.to_string(),
                                        title: x.get("title")?.as_str()?.to_string(),
                                        value: x.get("value")?.clone(),
                                        text: x.get("text")?.as_str()?.to_string(),
                                        group: None,
                                        view: None,
                                        tooltip: None,
                                    })
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    render_at(&sub, out, indent + 4)?;
                }
            }
            None => writeln!(out, "{pad}{} — {}", p.title, p.text)?,
        }
    }
    Ok(())
}
