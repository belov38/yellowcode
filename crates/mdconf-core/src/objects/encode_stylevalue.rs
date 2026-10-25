use super::ui::PALETTE;
use crate::model::Value;
use brace::Node;
use mdschema::{Ui, UiTables};
const T_COLOR: &str = "9cd510c7-abfc-11d4-9434-004095e12fc7";
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StyleValueEncodeError {
    WrongValue,
    InvalidShape,
    InvalidPrefix,
    UnsupportedValue,
    UnknownColor,
    AmbiguousColor,
    ResourceLimit,
}
fn inverse<'a>(
    entries: impl Iterator<Item = (&'a str, &'a str)>,
    name: &str,
) -> Result<i64, StyleValueEncodeError> {
    let mut found = None;
    for (key, value) in entries {
        if value != name {
            continue;
        }
        let Ok(code) = key.parse::<i64>() else {
            continue;
        };
        if code.to_string() != key {
            continue;
        }
        if found.replace(code).is_some() {
            return Err(StyleValueEncodeError::AmbiguousColor);
        }
    }
    found.ok_or(StyleValueEncodeError::UnknownColor)
}
pub(crate) fn encode_stylevalue(
    ui: &Ui,
    tables: &UiTables,
    tag: &str,
    value: &Value,
    prefix0: &Node,
    prefix2: &Node,
) -> Result<Node, StyleValueEncodeError> {
    use StyleValueEncodeError::*;
    let Value::Xml(root) = value else {
        return Err(WrongValue);
    };
    let [(key, kind)] = root.attrs.as_slice() else {
        return Err(InvalidShape);
    };
    if root.name != tag || root.expanded || !root.children.is_empty() || key != "xsi:type" {
        return Err(InvalidShape);
    }
    if kind != "v8ui:Color" {
        return Err(UnsupportedValue);
    }
    let text = root.text.as_deref().ok_or(InvalidShape)?;
    if matches!(prefix0, Node::List(_)) || matches!(prefix2, Node::List(_)) {
        return Err(InvalidPrefix);
    }
    let (kind, number, qualifier) = if text == "auto" {
        (4, 0, 4)
    } else if let Some(name) = text.strip_prefix("pal:") {
        (4, inverse(PALETTE.iter().copied(), name)?, 5)
    } else if let Some(hex) = text.strip_prefix('#') {
        if hex.len() != 6
            || !hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b))
        {
            return Err(UnsupportedValue);
        }
        let rgb = u32::from_str_radix(hex, 16).map_err(|_| UnsupportedValue)?;
        (
            0,
            i64::from(((rgb & 255) << 16) | (rgb & 0xff00) | (rgb >> 16)),
            0,
        )
    } else {
        let (prefix, name) = text.split_once(':').ok_or(UnsupportedValue)?;
        let (kind, table) = match prefix {
            "win" => (1, &tables.win_colors),
            "web" => (2, &ui.web_colors),
            "style" => (3, &tables.style_colors),
            _ => return Err(UnsupportedValue),
        };
        let code = inverse(table.iter().map(|(k, v)| (k.as_str(), v.as_str())), name)?;
        (kind, code, kind)
    };
    let mut out = Vec::new();
    out.try_reserve_exact(4).map_err(|_| ResourceLimit)?;
    out.push(prefix0.clone());
    out.push(Node::atom(T_COLOR));
    out.push(prefix2.clone());
    out.push(Node::list(vec![
        Node::int(4),
        Node::int(kind),
        Node::list(vec![Node::int(number)]),
        Node::int(qualifier),
    ]));
    Ok(Node::list(out))
}
