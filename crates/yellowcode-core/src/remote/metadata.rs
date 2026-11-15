use onec_client::Step;
use onec_client::metadata::{EnumValue, MdSection, MdType, Metadata};
use serde_json::{Value as Json, json};

pub(super) fn metadata_lines(m: &Metadata) -> Vec<String> {
    let mut l = Vec::new();
    l.push(format!("[types] ({})", m.types.len()));
    if m.types.is_empty() {
        l.push("-".into());
    }
    for t in &m.types {
        l.push(type_line(t));
        for v in &t.values {
            match &v.synonym {
                Some(syn) => l.push(format!("  value: {} {} {syn:?}", v.name, v.id)),
                None => l.push(format!("  value: {} {}", v.name, v.id)),
            }
        }
        for d in &t.dims {
            l.push(format!("  dim: {} {}", d.name, shown(&d.type_name)));
        }
    }
    l.push("[sections]".into());
    if m.sections.is_empty() {
        l.push("-".into());
    }
    for s in &m.sections {
        section_lines(s, 0, &mut l);
    }
    l.push("[unrecognized]".into());
    if m.unrecognized.is_empty() {
        l.push("-".into());
    }
    for u in &m.unrecognized {
        l.push(format!("{}: {}", u.answer, u.name));
    }
    l
}

fn type_line(t: &MdType) -> String {
    let mut details = vec![format!("id {}", t.type_id)];
    if let Some(n) = t.table {
        details.push(format!("table {n}"));
    }
    if let Some(a) = t.has_access {
        details.push(format!("access {a}"));
    }
    if let Some(r) = &t.recorder {
        details.push(format!("recorder {r}"));
    }
    if let Some(p) = t.periodic {
        details.push(format!("periodic {p}"));
    }
    format!(
        "{}.{}: {} ({})",
        t.kind.en(),
        t.name,
        shown(&t.view),
        details.join(", ")
    )
}

fn section_lines(s: &MdSection, depth: usize, l: &mut Vec<String>) {
    let pad = "  ".repeat(depth);
    let hidden = if s.include == Some(false) {
        ", not included"
    } else {
        ""
    };
    l.push(format!(
        "{pad}section: {} ({}{hidden}) {}",
        shown(&s.text),
        shown(&s.name),
        s.id
    ));
    for c in &s.commands {
        l.push(format!(
            "{pad}  command: {} -> {} [{}]",
            shown(&c.text),
            shown(&c.url),
            shown(&c.group)
        ));
    }
    for child in &s.children {
        section_lines(child, depth + 1, l);
    }
}

pub(super) fn metadata_json(
    m: &Metadata,
    steps: &[Step],
    enum_synonyms_error: Option<&str>,
) -> Json {
    let mut j = json!({
        "types": m.types.iter().map(|t| json!({
            "kind": t.kind.en(),
            "kind_ru": t.kind.ru(),
            "name": t.name,
            "view": t.view,
            "type_id": t.type_id,
            "table": t.table,
            "has_access": t.has_access,
            "values": t.values.iter().map(value_json).collect::<Vec<_>>(),
            "recorder": t.recorder,
            "periodic": t.periodic,
            "dims": t.dims.iter().map(|d| json!({"name": d.name, "type": d.type_name})).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "sections": m.sections.iter().map(section_json).collect::<Vec<_>>(),
        "unrecognized": m.unrecognized.iter().map(|x| json!({"answer": x.answer, "name": x.name})).collect::<Vec<_>>(),
    });
    if let (Some(o), Some(e)) = (j.as_object_mut(), enum_synonyms_error) {
        o.insert("enum_synonyms_error".into(), json!(e));
    }
    if let Some(o) = j.as_object_mut() {
        o.insert(
            "steps".into(),
            steps
                .iter()
                .map(|s| json!({"name": s.name, "status": s.status}))
                .collect::<Vec<_>>()
                .into(),
        );
    }
    j
}

fn value_json(v: &EnumValue) -> Json {
    let mut o = serde_json::Map::new();
    o.insert("id".into(), json!(v.id));
    o.insert("name".into(), json!(v.name));
    if let Some(syn) = &v.synonym {
        o.insert("synonym".into(), json!(syn));
    }
    Json::Object(o)
}

fn section_json(s: &MdSection) -> Json {
    json!({
        "id": s.id,
        "name": s.name,
        "text": s.text,
        "include": s.include,
        "commands": s.commands.iter().map(|c| json!({
            "id": c.id,
            "name": c.name,
            "name_ru": c.name_ru,
            "md": c.md,
            "url": c.url,
            "text": c.text,
            "group": c.group,
        })).collect::<Vec<_>>(),
        "children": s.children.iter().map(section_json).collect::<Vec<_>>(),
    })
}

fn shown<T: std::fmt::Display>(v: &Option<T>) -> String {
    v.as_ref()
        .map_or_else(|| "-".to_string(), ToString::to_string)
}
