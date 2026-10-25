use super::ui::PALETTE;
use crate::model::Value;
use brace::Node;
use mdschema::Ui;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ColorEncodeError {
    WrongValue,
    InvalidColor,
    UnknownColor,
    AmbiguousColor,
}

fn inverse<'a>(
    entries: impl Iterator<Item = (&'a str, &'a str)>,
    name: &str,
) -> Result<i64, ColorEncodeError> {
    let mut found = None;
    for (code, candidate) in entries {
        if candidate != name {
            continue;
        }
        let Ok(number) = code.parse::<i64>() else {
            continue;
        };
        if number.to_string() != code {
            continue;
        }
        if found.replace(number).is_some() {
            return Err(ColorEncodeError::AmbiguousColor);
        }
    }
    found.ok_or(ColorEncodeError::UnknownColor)
}

pub(crate) fn encode_color(ui: &Ui, value: &Value) -> Result<Node, ColorEncodeError> {
    let Value::Text(text) = value else {
        return Err(ColorEncodeError::WrongValue);
    };
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
            return Err(ColorEncodeError::InvalidColor);
        }
        let rgb = u32::from_str_radix(hex, 16).map_err(|_| ColorEncodeError::InvalidColor)?;
        let bgr = ((rgb & 255) << 16) | (rgb & 0xff00) | (rgb >> 16);
        (0, i64::from(bgr), 0)
    } else {
        let (prefix, name) = text.split_once(':').ok_or(ColorEncodeError::InvalidColor)?;
        let (kind, table) = match prefix {
            "win" => (1, &ui.win_colors),
            "web" => (2, &ui.web_colors),
            "style" => (3, &ui.style_colors),
            _ => return Err(ColorEncodeError::InvalidColor),
        };
        let number = inverse(
            table
                .iter()
                .map(|(code, name)| (code.as_str(), name.as_str())),
            name,
        )?;
        (kind, number, kind)
    };
    Ok(Node::list(vec![
        Node::int(4),
        Node::int(kind),
        Node::list(vec![Node::int(number)]),
        Node::int(qualifier),
    ]))
}
