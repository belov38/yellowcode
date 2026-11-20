use super::builder::{Builder, list, parse};
use super::{Result, invalid, xml};
use crate::forms::fe::E;
use brace::Node;

pub(super) fn resource_key(file: &str, table: &str, value: &str) -> Option<&'static str> {
    mdschema::static_schema()
        .table(file)?
        .get(table)?
        .as_object()?
        .iter()
        .find(|(_, v)| v.as_str() == Some(value))
        .map(|(k, _)| *k)
}
impl Builder<'_> {
    pub fn color(&self, value: &str) -> Result<Node> {
        if value == "auto" {
            return parse("{4,4,{0},4}");
        }
        if let Some(rgb) = value.strip_prefix('#') {
            if rgb.len() != 6 || !rgb.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(invalid("Color", "invalid RGB color"));
            }
            let n =
                u32::from_str_radix(rgb, 16).map_err(|_| invalid("Color", "invalid RGB color"))?;
            let bgr = (n & 255) << 16 | (n & 0xff00) | (n >> 16);
            return Ok(list(vec![
                Node::int(4),
                Node::int(0),
                list(vec![Node::int(i64::from(bgr))]),
                Node::int(0),
            ]));
        }
        if let Some(name) = value.strip_prefix("pal:") {
            let id = md_xmlout::codec::ui::PALETTE
                .iter()
                .find(|(_, n)| *n == name)
                .map(|(id, _)| *id)
                .ok_or_else(|| invalid("Color", "unknown palette color"))?;
            return Ok(list(vec![
                Node::int(4),
                Node::int(4),
                list(vec![Node::atom(id)]),
                Node::int(5),
            ]));
        }
        if let Some(name) = value.strip_prefix("style:")
            && let Some(id) = self.resolution.metadata_id(&format!("StyleItem.{name}"))
        {
            return Ok(list(vec![
                Node::int(4),
                Node::int(3),
                list(vec![Node::int(0), Node::guid(id)]),
                Node::int(3),
            ]));
        }
        let (prefix, name) = value
            .split_once(':')
            .ok_or_else(|| invalid("Color", "invalid color name"))?;
        let (kind, table) = match prefix {
            "win" => (1, "win_colors"),
            "web" => (2, "web_colors"),
            "style" => (3, "style_colors"),
            _ => return Err(invalid("Color", "unknown color category")),
        };
        let id = resource_key("ui_tables.json", table, name)
            .or_else(|| resource_key("ui.json", table, name))
            .map(str::to_owned)
            .or_else(|| {
                self.tables
                    .misc
                    .get("colors")?
                    .as_object()?
                    .iter()
                    .find(|(_, v)| v.as_str() == Some(value))
                    .and_then(|(key, _)| key.split_once('|').map(|(_, id)| id.to_owned()))
            })
            .ok_or_else(|| invalid("Color", "unknown color name"))?;
        Ok(list(vec![
            Node::int(4),
            Node::int(kind),
            list(vec![Node::atom(id)]),
            Node::int(kind),
        ]))
    }
    pub fn font(&self, e: &E) -> Result<Node> {
        let kind = match xml::attr(e, "kind") {
            Some("Absolute") => 0,
            Some("WindowsFont") => 1,
            Some("StyleItem") => 2,
            Some("AutoFont") => 3,
            _ => return Err(invalid("Font", "unknown font kind")),
        };
        if !e.kids.is_empty()
            || e.attrs.iter().any(|(k, _)| {
                !matches!(
                    k.as_str(),
                    "kind"
                        | "ref"
                        | "height"
                        | "bold"
                        | "italic"
                        | "underline"
                        | "strikeout"
                        | "faceName"
                        | "scale"
                )
            })
        {
            return Err(invalid("Font", "unknown font property"));
        }
        let height = || -> Result<Node> {
            let h: f64 = xml::attr(e, "height")
                .unwrap_or("0")
                .parse()
                .map_err(|_| invalid("Font", "invalid height"))?;
            let h = (h * 10.0).round_ties_even();
            if !h.is_finite() || h < f64::from(i32::MIN) || h > f64::from(i32::MAX) {
                return Err(invalid("Font", "font height out of range"));
            }
            Ok(Node::int(h as i64))
        };
        let flag =
            |key| -> Result<Node> { super::values::boolean(xml::attr(e, key).unwrap_or("false")) };
        let weight = || -> Result<Node> {
            Ok(Node::int(if flag("bold")?.as_i64() == Some(1) {
                700
            } else {
                400
            }))
        };
        let scale: i32 = xml::attr(e, "scale")
            .unwrap_or("100")
            .parse()
            .map_err(|_| invalid("Font", "invalid scale"))?;
        if kind == 0 {
            return Ok(list(vec![
                Node::int(8),
                Node::int(0),
                Node::int(if xml::attr(e, "scale").is_some() {
                    575
                } else {
                    63
                }),
                height()?,
                Node::int(0),
                Node::int(0),
                Node::int(0),
                weight()?,
                flag("italic")?,
                flag("underline")?,
                flag("strikeout")?,
                Node::int(0),
                Node::int(0),
                Node::int(0),
                Node::int(0),
                Node::int(0),
                Node::str(xml::attr(e, "faceName").unwrap_or_default()),
                Node::int(1),
                Node::int(i64::from(scale)),
                Node::int(0),
            ]));
        }
        let mut mask = 0;
        let mut values = Vec::new();
        if xml::attr(e, "height").is_some() {
            mask |= 2;
            values.push(height()?);
        }
        if xml::attr(e, "bold").is_some() {
            mask |= 4;
            values.push(weight()?);
        }
        for (bit, key) in [(8, "italic"), (16, "underline"), (32, "strikeout")] {
            if xml::attr(e, key).is_some() {
                mask |= bit;
                values.push(flag(key)?);
            }
        }
        if let Some(face) = xml::attr(e, "faceName") {
            mask |= 1;
            values.push(Node::str(face));
        }
        if xml::attr(e, "scale").is_some() {
            mask |= 512;
        }
        let mut out = vec![Node::int(8), Node::int(kind), Node::int(mask)];
        if kind != 3 {
            let reference = xml::attr(e, "ref").unwrap_or_default();
            if let Some(name) = reference.strip_prefix("style:")
                && let Some(id) = self.resolution.metadata_id(&format!("StyleItem.{name}"))
            {
                out.push(list(vec![Node::int(0), Node::guid(id)]));
            } else {
                let id = self
                    .tables
                    .misc
                    .get("fonts")
                    .and_then(|v| v.as_object())
                    .and_then(|v| {
                        v.iter().find(|(k, v)| {
                            k.starts_with(&format!("{kind}|")) && v.as_str() == Some(reference)
                        })
                    })
                    .and_then(|(k, _)| k.split_once('|').map(|(_, id)| id.to_owned()))
                    .or_else(|| {
                        let (_, name) = reference.split_once(':')?;
                        resource_key(
                            "ui_tables.json",
                            if kind == 1 {
                                "windows_fonts"
                            } else {
                                "style_fonts"
                            },
                            name,
                        )
                        .map(str::to_owned)
                    })
                    .ok_or_else(|| invalid("Font", "unknown font reference"))?;
                out.push(list(vec![Node::atom(id)]));
            }
        }
        out.extend(values);
        out.extend([Node::int(1), Node::int(i64::from(scale))]);
        Ok(list(out))
    }
    pub fn shortcut(&self, value: &str) -> Result<Node> {
        if value.is_empty() {
            return parse("{0,0,0}");
        }
        let mut rest = value;
        let mut mask = 0;
        loop {
            let next = if let Some(r) = rest.strip_prefix("Ctrl+") {
                mask |= 8;
                Some(r)
            } else if let Some(r) = rest.strip_prefix("Alt+") {
                mask |= 16;
                Some(r)
            } else if let Some(r) = rest.strip_prefix("Shift+") {
                mask |= 4;
                Some(r)
            } else {
                None
            };
            match next {
                Some(r) => rest = r,
                None => break,
            }
        }
        let key = if let Some(f) = rest
            .strip_prefix('F')
            .and_then(|s| s.parse::<u8>().ok())
            .filter(|n| (1..=24).contains(n))
        {
            i64::from(f) + 111
        } else if rest.len() == 1
            && rest
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        {
            i64::from(
                rest.as_bytes()
                    .first()
                    .copied()
                    .ok_or_else(|| invalid("Shortcut", "missing key"))?,
            )
        } else if let Some(key) = match rest {
            "Num *" => Some(106),
            "Num +" => Some(107),
            "Num -" => Some(109),
            "Num ." => Some(110),
            "Num /" => Some(111),
            _ => None,
        } {
            key
        } else if let Some(n) = rest
            .strip_prefix("Num ")
            .and_then(|s| s.parse::<u8>().ok())
            .filter(|n| *n <= 9)
        {
            i64::from(n) + 96
        } else {
            resource_key("ui.json", "keys", rest)
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| invalid("Shortcut", "unknown key"))?
        };
        Ok(list(vec![Node::int(0), Node::int(key), Node::int(mask)]))
    }
}
