use crate::{DUMP_VERSION, ManagedXmlWriter, codec::parts::count};
use brace::Node;
use md_model::parts::Style;
use memory_budget::{BudgetError, BudgetedText, BudgetedVec, Category, Charged, MemoryBudget};
use std::fmt::Write;

enum Failure {
    Bad,
    Memory(BudgetError),
}
impl From<BudgetError> for Failure {
    fn from(e: BudgetError) -> Self {
        Self::Memory(e)
    }
}
impl From<&'static str> for Failure {
    fn from(_: &'static str) -> Self {
        Self::Bad
    }
}
type Result<T> = std::result::Result<T, Failure>;
fn need<T>(value: Option<T>) -> Result<T> {
    value.ok_or(Failure::Bad)
}
fn at(node: &Node, index: usize) -> Result<&Node> {
    need(node.at(index))
}
fn text(node: &Node) -> Result<&str> {
    need(node.text())
}
fn int(node: &Node) -> Result<i64> {
    need(text(node)?.trim().parse().ok())
}
fn flag(node: &Node) -> Result<&'static str> {
    Ok(if text(node)? == "1" { "true" } else { "false" })
}
fn bold(node: &Node) -> Result<&'static str> {
    Ok(if int(node)? >= 700 { "true" } else { "false" })
}
fn table(name: &str, key: &str) -> Result<&'static str> {
    need(
        mdschema::static_schema()
            .table("ui_tables.json")
            .and_then(|v| v.get(name))
            .and_then(|v| v.get(key))
            .and_then(|v| v.as_str()),
    )
}
fn buffer(budget: &MemoryBudget, parts: &[&str]) -> Result<BudgetedText> {
    let mut out = BudgetedText::new(budget, Category::Metadata, u64::MAX)?;
    for part in parts {
        out.try_push_str(part)?;
    }
    Ok(out)
}
fn formatted(budget: &MemoryBudget, args: std::fmt::Arguments<'_>) -> Result<BudgetedText> {
    let mut out = buffer(budget, &[])?;
    if out.write_fmt(args).is_err() {
        return Err(out.error().copied().unwrap_or(BudgetError::Overflow).into());
    }
    Ok(out)
}
fn height(budget: &MemoryBudget, node: &Node) -> Result<BudgetedText> {
    let n = int(node)?;
    if n.rem_euclid(10) == 0 {
        formatted(budget, format_args!("{}", n.div_euclid(10)))
    } else {
        formatted(budget, format_args!("{}", n as f64 / 10.0))
    }
}
fn color(budget: &MemoryBudget, node: &Node) -> Result<BudgetedText> {
    let kind = text(at(node, 1)?)?;
    let value = match at(node, 2)? {
        Node::List(v) => v.first().and_then(Node::text).unwrap_or(""),
        _ => "",
    };
    match kind {
        "0" => {
            let n = need(value.trim().parse::<i64>().ok())?;
            formatted(
                budget,
                format_args!(
                    "#{:02X}{:02X}{:02X}",
                    n & 255,
                    (n >> 8) & 255,
                    (n >> 16) & 255
                ),
            )
        }
        "4" => {
            if node.at(3).and_then(Node::text) == Some("5") {
                let (_, name) = need(
                    crate::codec::ui::PALETTE
                        .iter()
                        .find(|(key, _)| *key == value),
                )?;
                buffer(budget, &["pal:", name])
            } else {
                buffer(budget, &["auto"])
            }
        }
        "1" | "2" | "3" => {
            let (prefix, name) = match kind {
                "1" => ("win:", table("win_colors", value)?),
                "3" => ("style:", table("style_colors", value)?),
                _ => (
                    "web:",
                    need(
                        mdschema::static_schema()
                            .table("ui.json")
                            .and_then(|v| v.get("web_colors"))
                            .and_then(|v| v.get(value))
                            .and_then(|v| v.as_str()),
                    )?,
                ),
            };
            buffer(budget, &[prefix, name])
        }
        _ => Err(Failure::Bad),
    }
}
fn font(writer: &mut ManagedXmlWriter, budget: &MemoryBudget, node: &Node) -> Result<()> {
    let nodes = need(node.as_list())?;
    if nodes.first().and_then(Node::text) != Some("8") {
        return Err(Failure::Bad);
    }
    let kind = text(at(node, 1)?)?;
    let mask = int(at(node, 2)?)?;
    if kind == "0" {
        let values = need(nodes.get(3..))?;
        let h = height(budget, need(values.first())?)?;
        writer.leaf(
            "Font",
            None,
            &[
                (
                    "faceName",
                    values.get(13).and_then(Node::text).unwrap_or(""),
                ),
                ("height", h.as_str()),
                ("bold", bold(need(values.get(4))?)?),
                ("italic", flag(need(values.get(5))?)?),
                ("underline", flag(need(values.get(6))?)?),
                ("strikeout", flag(need(values.get(7))?)?),
                ("kind", "Absolute"),
                ("scale", text(need(values.get(15))?)?),
            ],
        )?;
        return Ok(());
    }
    if !matches!(kind, "1" | "2" | "3") {
        return Err(Failure::Bad);
    }
    let mut pos = 3;
    let reference = if kind != "3" {
        let id = text(at(at(node, 3)?, 0)?)?;
        pos = 4;
        let (prefix, lookup) = if kind == "2" {
            ("style:", "style_fonts")
        } else {
            ("sys:", "windows_fonts")
        };
        Some(buffer(budget, &[prefix, table(lookup, id)?])?)
    } else {
        None
    };
    let h = if mask & 2 != 0 {
        let value = height(budget, at(node, pos)?)?;
        pos += 1;
        Some(value)
    } else {
        None
    };
    let b = if mask & 4 != 0 {
        let value = bold(at(node, pos)?)?;
        pos += 1;
        Some(value)
    } else {
        None
    };
    let mut flags = [None; 3];
    for (slot, bit) in flags.iter_mut().zip([8, 16, 32]) {
        if mask & bit != 0 {
            *slot = Some(flag(at(node, pos)?)?);
            pos += 1;
        }
    }
    let face = if mask & 1 != 0 {
        let value = text(at(node, pos)?)?;
        pos += 1;
        Some(value)
    } else {
        None
    };
    let scale = if mask & 512 != 0 {
        Some(text(at(node, pos + 1)?)?)
    } else {
        None
    };
    let line = if mask & 256 != 0 {
        Some(text(at(node, pos + 2)?)?)
    } else {
        None
    };
    let mut attrs = [("", ""); 10];
    let mut len = 0;
    let mut push = |key, value| -> Result<()> {
        *attrs.get_mut(len).ok_or(BudgetError::Overflow)? = (key, value);
        len += 1;
        Ok(())
    };
    if let Some(v) = &reference {
        push("ref", v.as_str())?;
    }
    if let Some(v) = face {
        push("faceName", v)?;
    }
    if let Some(v) = &h {
        push("height", v.as_str())?;
    }
    if let Some(v) = b {
        push("bold", v)?;
    }
    for (name, value) in ["italic", "underline", "strikeout"].into_iter().zip(flags) {
        if let Some(v) = value {
            push(name, v)?;
        }
    }
    push(
        "kind",
        match kind {
            "1" => "WindowsFont",
            "2" => "StyleItem",
            _ => "AutoFont",
        },
    )?;
    if let Some(v) = scale {
        push("scale", v)?;
    }
    if let Some(v) = line {
        push("lineHeight", v)?;
    }
    writer.leaf("Font", None, attrs.get(..len).ok_or(BudgetError::Overflow)?)?;
    Ok(())
}
fn border(writer: &mut ManagedXmlWriter, budget: &MemoryBudget, node: &Node) -> Result<()> {
    if text(at(node, 0)?)? != "3" {
        return Err(Failure::Bad);
    }
    match text(at(node, 1)?)? {
        "1" => {
            let name = table("style_borders", text(at(at(node, 2)?, 0)?)?)?;
            let reference = buffer(budget, &["style:", name])?;
            writer.leaf("Border", None, &[("ref", reference.as_str())])?;
        }
        "0" => {
            if text(at(node, 6)?)? != "48312c09-257f-4b29-b280-284dd89efc1e" {
                return Err(Failure::Bad);
            }
            let style = table("border_styles", text(at(node, 3)?)?)?;
            let width = text(at(node, 4)?)?;
            writer.open("Border", &[("width", width)])?;
            writer.leaf(
                "v8ui:style",
                Some(style),
                &[("xsi:type", "v8ui:ControlBorderType")],
            )?;
            writer.close("Border")?;
        }
        _ => return Err(Failure::Bad),
    }
    Ok(())
}
struct Header<'a> {
    rank: usize,
    ordinal: usize,
    name: &'static str,
    kind: &'a str,
    value: &'a Node,
}
fn write(budget: &MemoryBudget, style: &Style, limit: u64) -> Result<Charged<String>> {
    let nodes = need(need(style.source.node())?.as_list())?;
    let n = count(need(nodes.get(1))?)?;
    let order = need(
        mdschema::static_schema()
            .table("ui_tables.json")
            .and_then(|v| v.get("style_order"))
            .and_then(|v| v.as_array()),
    )?;
    let mut headers = BudgetedVec::try_with_capacity(
        budget,
        Category::Metadata,
        n.min(nodes.len().saturating_sub(2)),
    )?;
    for (ordinal, item) in nodes.iter().skip(2).take(n).enumerate() {
        let id = text(at(at(item, 0)?, 0)?)?;
        let kind = text(at(item, 1)?)?;
        let value = at(item, 2)?;
        let (lookup, group) = match kind {
            "0" => ("style_colors", 0),
            "1" => ("style_fonts", 2),
            "2" => ("style_borders", 1),
            _ => return Err(Failure::Bad),
        };
        let name = table(lookup, id)?;
        let rank = order
            .iter()
            .position(|v| v.as_str() == Some(name))
            .unwrap_or(1000 + group * 100);
        headers.try_push(Header {
            rank,
            ordinal,
            name,
            kind,
            value,
        })?;
    }
    headers.sort_unstable_by_key(|h| (h.rank, h.ordinal));
    let mut writer = ManagedXmlWriter::part_document(
        budget,
        limit,
        "Style",
        &[
            ("xmlns", "http://v8.1c.ru/8.3/xcf/extrnprops"),
            ("xmlns:pal", "http://v8.1c.ru/8.1/data/ui/colors/palette"),
            ("xmlns:style", "http://v8.1c.ru/8.1/data/ui/style"),
            ("xmlns:sys", "http://v8.1c.ru/8.1/data/ui/fonts/system"),
            ("xmlns:v8ui", "http://v8.1c.ru/8.1/data/ui"),
            ("xmlns:web", "http://v8.1c.ru/8.1/data/ui/colors/web"),
            ("xmlns:win", "http://v8.1c.ru/8.1/data/ui/colors/windows"),
            ("xmlns:xr", "http://v8.1c.ru/8.3/xcf/readable"),
            ("xmlns:xs", "http://www.w3.org/2001/XMLSchema"),
            ("xmlns:xsi", "http://www.w3.org/2001/XMLSchema-instance"),
            ("version", DUMP_VERSION),
        ],
    )?;
    for h in headers.iter() {
        writer.open("Item", &[("name", h.name)])?;
        match h.kind {
            "0" => writer.leaf("Color", Some(color(budget, h.value)?.as_str()), &[])?,
            "1" => font(&mut writer, budget, h.value)?,
            _ => border(&mut writer, budget, h.value)?,
        }
        writer.close("Item")?;
    }
    Ok(writer.finish()?)
}
pub fn managed_style_xml(
    budget: &MemoryBudget,
    style: &Style,
    limit: u64,
) -> std::result::Result<Option<Charged<String>>, BudgetError> {
    match write(budget, style, limit) {
        Ok(body) => Ok(Some(body)),
        Err(Failure::Bad) => Ok(None),
        Err(Failure::Memory(e)) => Err(e),
    }
}
