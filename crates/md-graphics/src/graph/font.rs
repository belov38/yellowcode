use super::read::{R, Read};
use super::scalar::Text;
use crate::templates::context::{self, Lookup};
use crate::xml_out::semantic::SemanticEmit;
use brace::{Guid, Node};
use std::fmt::Write;
const SYS: &str = "http://v8.1c.ru/8.1/data/ui/fonts/system";
#[derive(Clone, Copy)]
pub(crate) enum Grammar {
    Graph,
    Spreadsheet,
}
enum Value<'a> {
    Text(&'a str),
    QName(&'static str, &'a str),
    Number(Text<64>),
}
impl Value<'_> {
    fn pieces(&self) -> [&str; 3] {
        match self {
            Self::Text(text) => [text, "", ""],
            Self::QName(prefix, name) => [prefix, ":", name],
            Self::Number(text) => [text.as_str(), "", ""],
        }
    }
}
struct Attr<'a> {
    key: &'static str,
    value: Value<'a>,
}
pub(crate) struct Font<'a> {
    attrs: [Option<Attr<'a>>; 16],
    len: usize,
}
impl<'a> Font<'a> {
    fn new() -> Self {
        Self {
            attrs: std::array::from_fn(|_| None),
            len: 0,
        }
    }
    fn push(&mut self, key: &'static str, value: Value<'a>, read: Read<'_>) -> R<()> {
        let slot = self
            .attrs
            .get_mut(self.len)
            .ok_or_else(|| read.invalid("font attribute count"))?;
        *slot = Some(Attr { key, value });
        self.len += 1;
        Ok(())
    }
    fn scalar<'n>(node: &'n Node, read: Read<'_>, grammar: Grammar) -> R<&'n str> {
        match grammar {
            Grammar::Graph => read.scalar(node),
            Grammar::Spreadsheet => node.text().ok_or_else(|| read.invalid("not a scalar")),
        }
    }
    fn int(node: &Node, read: Read<'_>, grammar: Grammar) -> R<i64> {
        match grammar {
            Grammar::Graph => read.int(node),
            Grammar::Spreadsheet => Self::scalar(node, read, grammar)?
                .trim()
                .parse()
                .map_err(|_| read.invalid("not an integer")),
        }
    }
    fn boolean(node: &Node, read: Read<'_>, grammar: Grammar) -> R<Value<'a>> {
        let on = match grammar {
            Grammar::Graph => node.text() == Some("1"),
            Grammar::Spreadsheet => Self::scalar(node, read, grammar)? == "1",
        };
        Ok(Value::Text(if on { "true" } else { "false" }))
    }
    fn height(node: &Node, read: Read<'_>, grammar: Grammar) -> R<Value<'a>> {
        let number = Self::int(node, read, grammar)?;
        let mut text = Text::new();
        if number.rem_euclid(10) == 0 {
            write!(&mut text, "{}", number.div_euclid(10))
        } else {
            write!(
                &mut text,
                "{}.{}",
                number.div_euclid(10),
                number.rem_euclid(10)
            )
        }
        .map_err(|_| read.invalid("font height overflow"))?;
        Ok(Value::Number(text))
    }
    pub(crate) fn lookup(
        values: &'a [Node],
        read: Read<'_>,
        grammar: Grammar,
        lookup: &'a dyn Lookup,
        sys_declared: bool,
    ) -> R<Self> {
        let names = |key: &str| {
            Guid::parse(key)
                .and_then(|id| lookup.name(&id))
                .and_then(|name| match grammar {
                    Grammar::Graph => name.strip_prefix("StyleItem."),
                    Grammar::Spreadsheet => name.split_once('.').map(|(_, name)| name),
                })
        };
        Self::read(values, read, grammar, Some(&names), sys_declared)
    }
    pub(crate) fn read(
        values: &'a [Node],
        read: Read<'_>,
        grammar: Grammar,
        names: Option<&crate::graph::read::NameResolver<'a, '_>>,
        sys_declared: bool,
    ) -> R<Self> {
        let scalar = |node| Self::scalar(node, read, grammar);
        let int = |node| Self::int(node, read, grammar);
        let kind = scalar(read.at(values, 1)?)?;
        let mask = int(read.at(values, 2)?)?;
        let bit = |bit: u32| mask >> bit & 1_i64 == 1;
        let mut out = Self::new();
        if kind == "0" {
            out.push("faceName", Value::Text(scalar(read.at(values, 16)?)?), read)?;
            out.push(
                "height",
                Self::height(read.at(values, 3)?, read, grammar)?,
                read,
            )?;
            out.push(
                "bold",
                Value::Text(if int(read.at(values, 7)?)? >= 700 {
                    "true"
                } else {
                    "false"
                }),
                read,
            )?;
            out.push(
                "italic",
                Self::boolean(read.at(values, 8)?, read, grammar)?,
                read,
            )?;
            out.push(
                "underline",
                Self::boolean(read.at(values, 9)?, read, grammar)?,
                read,
            )?;
            out.push(
                "strikeout",
                Self::boolean(read.at(values, 10)?, read, grammar)?,
                read,
            )?;
            out.push("kind", Value::Text("Absolute"), read)?;
            out.push("scale", Value::Text(scalar(read.at(values, 18)?)?), read)?;
            if bit(8) {
                out.push(
                    "lineHeight",
                    Value::Text(scalar(read.at(values, 19)?)?),
                    read,
                )?;
            }
            return Ok(out);
        }
        if kind == "3" {
            out.push("kind", Value::Text("AutoFont"), read)?;
            return Ok(out);
        }
        let rf = read.at(values, 3)?;
        let rf = match grammar {
            Grammar::Graph => rf.as_list(),
            Grammar::Spreadsheet => Some(rf.as_list().ok_or_else(|| read.invalid("not a list"))?),
        };
        let item = |index| -> R<&Node> {
            match rf {
                Some(values) => read.at(values, index),
                None => Err(read.invalid("scalar where a list is expected")),
            }
        };
        let kind_name = match kind {
            "2" => {
                let first = item(0)?;
                let common = match grammar {
                    Grammar::Graph => first.text() == Some("0"),
                    Grammar::Spreadsheet => scalar(first)? == "0",
                };
                let name = if common {
                    let key = match grammar {
                        Grammar::Graph => item(1)?.text(),
                        Grammar::Spreadsheet => {
                            rf.and_then(|values| values.get(1)).and_then(Node::text)
                        }
                    };
                    key.and_then(|key| names.and_then(|names| names(key)))
                        .ok_or_else(|| read.invalid("style font"))?
                } else {
                    let key = scalar(first)?;
                    context::mxl("style_fonts", key).ok_or_else(|| match grammar {
                        Grammar::Graph => {
                            read.semantic("style font", || format!("style font {key}"))
                        }
                        Grammar::Spreadsheet => read.invalid("style font"),
                    })?
                };
                out.push("ref", Value::QName("style", name), read)?;
                "StyleItem"
            }
            "1" => {
                if !sys_declared || matches!(grammar, Grammar::Spreadsheet) {
                    out.push("xmlns:sys", Value::Text(SYS), read)?;
                }
                let key = scalar(item(0)?)?;
                let name = match grammar {
                    Grammar::Graph => context::mxl("sys_fonts", key).or(match key {
                        "0" => Some("DefaultGUIFont"),
                        "1" => Some("OEMFixedFont"),
                        "2" => Some("ANSIFixedFont"),
                        _ => None,
                    }),
                    Grammar::Spreadsheet => {
                        if context::has_mxl_object("sys_fonts") {
                            context::mxl("sys_fonts", key)
                        } else {
                            (key == "0").then_some("DefaultGUIFont")
                        }
                    }
                }
                .ok_or_else(|| match grammar {
                    Grammar::Graph => read.semantic("system font", || format!("system font {key}")),
                    Grammar::Spreadsheet => read.invalid("system font"),
                })?;
                out.push("ref", Value::QName("sys", name), read)?;
                "WindowsFont"
            }
            _ => match grammar {
                Grammar::Graph => {
                    return Err(read.semantic("font kind", || format!("font kind {kind}")));
                }
                Grammar::Spreadsheet => "",
            },
        };
        let mut fields: [Option<(&str, &Node)>; 6] = [None; 6];
        let mut at = 4;
        for (slot, (flag, key)) in [
            (1, "height"),
            (2, "bold"),
            (3, "italic"),
            (4, "underline"),
            (5, "strikeout"),
            (0, "faceName"),
        ]
        .into_iter()
        .enumerate()
        {
            if bit(flag) {
                let node = read.at(values, at)?;
                at += 1;
                if matches!(grammar, Grammar::Spreadsheet) {
                    scalar(node)?;
                }
                if let Some(field) = fields.get_mut(slot) {
                    *field = Some((key, node));
                }
            }
        }
        let field = |key| {
            fields
                .iter()
                .flatten()
                .find(|(name, _)| *name == key)
                .map(|(_, node)| *node)
        };
        if let Some(node) = field("faceName") {
            out.push("faceName", Value::Text(scalar(node)?), read)?;
        }
        if let Some(node) = field("height") {
            out.push("height", Self::height(node, read, grammar)?, read)?;
        }
        if let Some(node) = field("bold") {
            out.push(
                "bold",
                Value::Text(if int(node)? >= 700 { "true" } else { "false" }),
                read,
            )?;
        }
        for key in ["italic", "underline", "strikeout"] {
            if let Some(node) = field(key) {
                out.push(key, Self::boolean(node, read, grammar)?, read)?;
            }
        }
        if kind_name.is_empty() {
            return Err(read.invalid("font kind"));
        }
        out.push("kind", Value::Text(kind_name), read)?;
        if bit(9) {
            out.push(
                "scale",
                Value::Text(scalar(read.at(values, at + 1)?)?),
                read,
            )?;
        }
        if bit(8) {
            out.push(
                "lineHeight",
                Value::Text(scalar(read.at(values, at + 2)?)?),
                read,
            )?;
        }
        Ok(out)
    }
    fn iter(&self) -> impl Iterator<Item = (&str, [&str; 3])> {
        self.attrs
            .iter()
            .flatten()
            .map(|attr| (attr.key, attr.value.pieces()))
    }
    pub(crate) fn write(&self, writer: &mut (impl SemanticEmit + ?Sized), tag: &str) -> R<()> {
        writer.attribute_fragments(tag, &mut self.iter())?;
        Ok(())
    }
}
