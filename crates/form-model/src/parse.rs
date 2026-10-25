use crate::desc::{Attribute, Column, Command, FormDesc, Item, ItemKind, ParseStats};
use crate::dom::{self, XNode};
use crate::props::{Props, lstr, picture};
use crate::types::{DateParts, TypeDesc};
use crate::{MAX_DEPTH, ModelError};

pub fn parse_form_xml(xml: &str) -> Result<FormDesc, ModelError> {
    let root = dom::parse(xml)?;
    if root.name != "Form" {
        return Err(ModelError::NotAForm(root.name));
    }
    let mut stats = ParseStats::default();
    let mut f = FormDesc {
        title: None,
        width: None,
        auto_command_bar: None,
        items: Vec::new(),
        attributes: Vec::new(),
        commands: Vec::new(),
        events: Vec::new(),
        stats: ParseStats::default(),
        role: None,
        name_lang: Default::default(),
    };
    for n in &root.children {
        match n.name.as_str() {
            "Title" => f.title = Some(lstr(n)),
            "Width" => match n.text.trim().parse() {
                Ok(w) => f.width = Some(w),
                Err(_) => bump(
                    &mut stats.bad_values,
                    format!("Form.Width={}", n.text.trim()),
                ),
            },
            "AutoCommandBar" => f.auto_command_bar = Some(item(n, 1, &mut stats)?),
            "Events" => {
                for e in n.children.iter().filter(|c| c.name == "Event") {
                    f.events.push((
                        e.attr("name").unwrap_or("").to_string(),
                        e.text.trim().to_string(),
                    ));
                }
            }
            "ChildItems" => f.items = items(n, 1, &mut stats)?,
            "Attributes" => {
                f.attributes = n
                    .children
                    .iter()
                    .filter(|c| c.name == "Attribute")
                    .map(attribute)
                    .collect()
            }
            "Commands" => {
                f.commands = n
                    .children
                    .iter()
                    .filter(|c| c.name == "Command")
                    .map(command)
                    .collect()
            }
            other => bump(&mut stats.skipped, format!("Form.{other}")),
        }
    }
    f.stats = stats;
    Ok(f)
}

fn bump(m: &mut std::collections::BTreeMap<String, u32>, key: String) {
    *m.entry(key).or_insert(0) += 1;
}

fn is_part(tag: &str) -> bool {
    matches!(
        tag,
        "ContextMenu"
            | "ExtendedTooltip"
            | "AutoCommandBar"
            | "SearchStringAddition"
            | "ViewStatusAddition"
            | "SearchControlAddition"
    )
}

fn items(
    child_items: &XNode,
    depth: usize,
    stats: &mut ParseStats,
) -> Result<Vec<Item>, ModelError> {
    child_items
        .children
        .iter()
        .map(|c| item(c, depth, stats))
        .collect()
}

fn item(n: &XNode, depth: usize, stats: &mut ParseStats) -> Result<Item, ModelError> {
    if depth > MAX_DEPTH {
        return Err(ModelError::TooDeep);
    }
    let kind = ItemKind::from_xml(&n.name);
    if matches!(kind, ItemKind::Stub(_)) {
        bump(&mut stats.stubs, n.name.clone());
    }
    let mut it = Item {
        id: n
            .attr("id")
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0),
        name: n.attr("name").unwrap_or("").to_string(),
        kind,
        props: Props::default(),
        children: Vec::new(),
        parts: Vec::new(),
    };
    for c in &n.children {
        match c.name.as_str() {
            "ChildItems" => it.children = items(c, depth + 1, stats)?,
            t if is_part(t) => it.parts.push(item(c, depth + 1, stats)?),
            _ => {
                if !it.props.set(&it.kind, c, &mut stats.bad_values) {
                    bump(&mut stats.skipped, format!("{}.{}", it.kind.xml(), c.name));
                }
            }
        }
    }
    if let Some(crate::PictureRef::Embedded(file)) = &mut it.props.picture
        && !file.contains('/')
    {
        *file = format!("Items/{}/{file}", it.name);
    }
    Ok(it)
}

fn num(n: &XNode, name: &str) -> Option<u32> {
    n.text_of(name).and_then(|t| t.parse().ok())
}

fn type_desc(t: &XNode) -> TypeDesc {
    let mut d = TypeDesc::default();
    for c in &t.children {
        match c.name.as_str() {
            "Type" | "TypeSet" => d.types.push(c.text.trim().to_string()),
            "StringQualifiers" => d.string_len = num(c, "Length"),
            "NumberQualifiers" => {
                d.number = Some((
                    num(c, "Digits").unwrap_or(0),
                    num(c, "FractionDigits").unwrap_or(0),
                ))
            }
            "DateQualifiers" => {
                d.date = Some(match c.text_of("DateFractions") {
                    Some("Date") => DateParts::Date,
                    Some("Time") => DateParts::Time,
                    _ => DateParts::DateTime,
                })
            }
            _ => {}
        }
    }
    d
}

fn attribute(a: &XNode) -> Attribute {
    Attribute {
        id: a
            .attr("id")
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0),
        name: a.attr("name").unwrap_or("").to_string(),
        title: a.child("Title").map(lstr),
        ty: a.child("Type").map(type_desc).unwrap_or_default(),
        main: a.text_of("MainAttribute") == Some("true"),
        columns: a
            .child("Columns")
            .map(|cs| {
                cs.children
                    .iter()
                    .filter(|c| c.name == "Column")
                    .map(|c| Column {
                        name: c.attr("name").unwrap_or("").to_string(),
                        title: c.child("Title").map(lstr),
                        ty: c.child("Type").map(type_desc).unwrap_or_default(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        main_table: a
            .child("Settings")
            .and_then(|s| s.text_of("MainTable"))
            .filter(|t| !t.is_empty())
            .map(str::to_string),
    }
}

fn command(c: &XNode) -> Command {
    Command {
        name: c.attr("name").unwrap_or("").to_string(),
        title: c.child("Title").map(lstr),
        picture: c.child("Picture").and_then(picture),
        representation: c.text_of("Representation").map(str::to_string),
    }
}
