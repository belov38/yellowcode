use super::read::{R, Read};
use super::scalar::Text;
use crate::templates::{
    allocation::Allocation,
    context::{self, Lookup},
};
use crate::xml_out::semantic::SemanticEmit;
use brace::{Guid, Node};
use std::fmt::Write;

const WEB: &str = "http://v8.1c.ru/8.1/data/ui/colors/web";
const WIN: &str = "http://v8.1c.ru/8.1/data/ui/colors/windows";
const PAL: &str = "http://v8.1c.ru/8.1/data/ui/colors/palette";
const STYLE: &str = "http://v8.1c.ru/8.1/data/ui/style";
const PALETTE: [Option<&str>; 16] = [
    Some("FirstBrand"),
    Some("SecondBrand"),
    Some("Red"),
    Some("Orange"),
    Some("Yellow"),
    Some("Green"),
    Some("LightBlue"),
    Some("Blue"),
    Some("Violet"),
    Some("Purple"),
    Some("Pink"),
    None,
    None,
    None,
    None,
    Some("Gray"),
];
enum Value<'a> {
    Plain(&'a str),
    QName { prefix: &'a str, name: &'a str },
    Rgb(Text<16>),
}
pub(crate) struct Color<'a> {
    value: Value<'a>,
    pub(crate) namespace: Option<(&'a str, &'static str)>,
}
impl<'a> Color<'a> {
    fn plain(text: &'a str) -> Self {
        Self {
            value: Value::Plain(text),
            namespace: None,
        }
    }
    fn qname(prefix: &'a str, name: &'a str, namespace: Option<(&'a str, &'static str)>) -> Self {
        Self {
            value: Value::QName { prefix, name },
            namespace,
        }
    }
    fn pref(
        prefix: &'static str,
        name: &'a str,
        declared: &[&str],
        local: Option<&'a str>,
    ) -> Self {
        let uri = match prefix {
            "web" => WEB,
            "win" => WIN,
            "pal" => PAL,
            _ => STYLE,
        };
        match local {
            Some(local) if !declared.contains(&prefix) => {
                Self::qname(local, name, Some((local, uri)))
            }
            _ => Self::qname(prefix, name, None),
        }
    }
    fn rgb(number: i64, read: Read<'_>) -> R<Self> {
        let mut text = Text::new();
        write!(
            &mut text,
            "#{:02X}{:02X}{:02X}",
            number & 255,
            (number >> 8) & 255,
            (number >> 16) & 255
        )
        .map_err(|_| read.invalid("color format overflow"))?;
        Ok(Self {
            value: Value::Rgb(text),
            namespace: None,
        })
    }
    pub(crate) fn graph(
        value: &'a Node,
        read: Read<'_>,
        lookup: &'a dyn Lookup,
        declared: &[&str],
        local: Option<&'a str>,
    ) -> R<Self> {
        let values = read.list(value)?;
        let kind = read.scalar(read.at(values, 1)?)?;
        let val = read.at(values, 2)?;
        match kind {
            "0" => Self::rgb(read.int(read.item(val, 0)?)?, read),
            "4" => {
                let k2 = read.scalar(read.at(values, 3)?)?;
                if k2 == "4" && read.item(val, 0)?.text() == Some("0") {
                    return Ok(Self::plain("auto"));
                }
                if k2 == "5" {
                    let index = read.int(read.item(val, 0)?)?;
                    let name = if index < 16 {
                        *read.at(&PALETTE, index)?
                    } else {
                        None
                    };
                    let name = name.ok_or_else(|| {
                        read.semantic("palette color", || format!("palette color {index}"))
                    })?;
                    return Ok(Self::pref("pal", name, declared, local));
                }
                Err(read.semantic("color", || format!("color {}", brace::to_compact(value))))
            }
            "2" => {
                let key = read.scalar(read.item(val, 0)?)?;
                let name = context::ui("web_colors", key)
                    .ok_or_else(|| read.semantic("web color", || format!("web color {key}")))?;
                Ok(Self::pref("web", name, declared, local))
            }
            "1" => {
                let key = read.scalar(read.item(val, 0)?)?;
                let name = context::merged("win_colors", key).ok_or_else(|| {
                    read.semantic("windows color", || format!("windows color {key}"))
                })?;
                Ok(Self::pref("win", name, declared, local))
            }
            "3" => {
                let first = read.item(val, 0)?;
                let name = if first.text() == Some("0") {
                    let id = read.item(val, 1)?.text().and_then(Guid::parse);
                    id.and_then(|id| lookup.name(&id))
                        .and_then(|name| name.strip_prefix("StyleItem."))
                        .ok_or_else(|| read.invalid("style item"))?
                } else {
                    let key = read.scalar(first)?;
                    context::merged("style_colors", key).ok_or_else(|| {
                        read.semantic("style color", || format!("style color {key}"))
                    })?
                };
                Ok(Self::pref("style", name, declared, local))
            }
            _ => Err(read.semantic("color kind", || format!("color kind {kind}"))),
        }
    }
    pub(crate) fn spreadsheet_lookup(
        values: &'a [Node],
        read: Read<'_>,
        lookup: &'a dyn Lookup,
    ) -> R<Self> {
        let names = |text: &str| {
            Guid::parse(text)
                .and_then(|id| lookup.name(&id))
                .and_then(|name| name.split_once('.').map(|(_, name)| name))
        };
        Self::spreadsheet_names(values, read, Some(&names))
    }
    pub(crate) fn spreadsheet_names(
        values: &'a [Node],
        read: Read<'_>,
        names: Option<&crate::graph::read::NameResolver<'a, '_>>,
    ) -> R<Self> {
        let scalar = |node: &'a Node| node.text().ok_or_else(|| read.invalid("not a scalar"));
        let kind = scalar(read.at(values, 1)?)?;
        let raw = read.at(values, 2)?;
        if kind == "4" {
            return Ok(Self::plain("auto"));
        }
        let val = raw.as_list().ok_or_else(|| read.invalid("not a list"))?;
        match kind {
            "0" => Self::rgb(
                scalar(read.at(val, 0)?)?
                    .trim()
                    .parse()
                    .map_err(|_| read.invalid("not an integer"))?,
                read,
            ),
            "2" => {
                let name = context::ui("web_colors", scalar(read.at(val, 0)?)?)
                    .ok_or_else(|| read.invalid("web color"))?;
                Ok(Self::qname("d3p1", name, Some(("d3p1", WEB))))
            }
            "1" => {
                let name = context::merged("win_colors", scalar(read.at(val, 0)?)?)
                    .ok_or_else(|| read.invalid("windows color"))?;
                Ok(Self::qname("d3p1", name, Some(("d3p1", WIN))))
            }
            "3" => {
                let key = scalar(read.at(val, 0)?)?;
                let name = if key == "0" {
                    names
                        .and_then(|name| name(val.get(1)?.text()?))
                        .ok_or_else(|| read.invalid("style item"))?
                } else {
                    context::merged("style_colors", key)
                        .ok_or_else(|| read.invalid("style color"))?
                };
                Ok(Self::qname("style", name, None))
            }
            _ => Err(read.semantic("color kind", || format!("color kind {kind}"))),
        }
    }
    pub(crate) fn pieces(&self) -> [&str; 3] {
        match &self.value {
            Value::Plain(text) => [text, "", ""],
            Value::QName { prefix, name } => [prefix, ":", name],
            Value::Rgb(text) => [text.as_str(), "", ""],
        }
    }
    pub(crate) fn write(
        &self,
        writer: &mut (impl SemanticEmit + ?Sized),
        tag: &str,
        allocation: Allocation<'_>,
    ) -> R<()> {
        let key = self
            .namespace
            .map(|(prefix, _)| allocation.text(&["xmlns:", prefix]))
            .transpose()?;
        match (&key, self.namespace) {
            (Some(key), Some((_, uri))) => {
                writer.text_fragments(tag, &self.pieces(), &[(key.as_str(), uri)])?
            }
            _ => writer.text_fragments(tag, &self.pieces(), &[])?,
        };
        Ok(())
    }
}
