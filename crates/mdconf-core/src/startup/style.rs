use super::{ManagedValueContext, index::text, leaf::decimal, xml::Xml};
use brace::Node;
use memory_budget::{BudgetError, BudgetedText, Category};
use std::fmt::Write;
macro_rules! present {
    ($value:expr) => {
        match $value {
            Some(value) => value,
            None => return Ok(None),
        }
    };
}
fn table(table: &str, key: &str) -> Option<&'static str> {
    mdschema::static_schema()
        .table("ui_tables.json")?
        .get(table)?
        .get(key)?
        .as_str()
}
fn int(node: &Node) -> Option<i64> {
    node.text()?.trim().parse().ok()
}
fn flag(node: &Node) -> Option<&'static str> {
    Some(if node.text()? == "1" { "true" } else { "false" })
}
fn bold(node: &Node) -> Option<&'static str> {
    Some(if int(node)? >= 700 { "true" } else { "false" })
}
impl ManagedValueContext<'_> {
    pub(super) fn validate_style_item(&self, kind: &str, node: &Node) -> Result<bool, BudgetError> {
        match kind {
            "0" => Ok(self.style_color(node)?.is_some()),
            "1" => Ok(self.style_font("Font", node)?.is_some()),
            "2" => Ok(self.style_border("Border", node)?.is_some()),
            _ => Ok(false),
        }
    }

    pub(super) fn style_value(&self, tag: &str, node: &Node) -> Result<Option<Xml>, BudgetError> {
        let payload = present!(node.at(3));
        match present!(node.at(1).and_then(Node::text)) {
            "9cd510c7-abfc-11d4-9434-004095e12fc7" => {
                let value = present!(self.style_color(payload)?);
                Ok(Some(
                    Xml::leaf(self.memory_budget(), tag, value.as_str(), 1)?.attr(
                        self.memory_budget(),
                        "xsi:type",
                        "v8ui:Color",
                    )?,
                ))
            }
            "9cd510c8-abfc-11d4-9434-004095e12fc7" => self.style_font(tag, payload),
            "4d10ca00-111a-4d43-9c96-92cd773716de" => self.style_border(tag, payload),
            _ => Ok(None),
        }
    }
    fn style_color(&self, node: &Node) -> Result<Option<BudgetedText>, BudgetError> {
        let kind = present!(node.at(1).and_then(Node::text));
        if matches!(kind, "0" | "4") {
            return self.color(node);
        }
        let value = match present!(node.at(2)) {
            Node::List(values) => values.first().and_then(Node::text).unwrap_or(""),
            _ => "",
        };
        let (prefix, table_name) = match kind {
            "1" => ("win:", "win_colors"),
            "2" => ("web:", "web_colors"),
            "3" => ("style:", "style_colors"),
            _ => return Ok(None),
        };
        let name = present!(if kind == "2" {
            mdschema::static_schema()
                .table("ui.json")
                .and_then(|s| s.get(table_name))
                .and_then(|s| s.get(value))
                .and_then(|s| s.as_str())
        } else {
            table(table_name, value)
        });
        Ok(Some(text(self.memory_budget(), &[prefix, name])?))
    }
    fn height(&self, node: &Node) -> Result<Option<BudgetedText>, BudgetError> {
        let value = present!(int(node));
        if value.rem_euclid(10) == 0 {
            let mut buffer = [0; 24];
            return Ok(Some(text(
                self.memory_budget(),
                &[decimal(value.div_euclid(10), &mut buffer)?],
            )?));
        }
        let mut output = BudgetedText::new(self.memory_budget(), Category::Metadata, u64::MAX)?;
        if write!(output, "{}", value as f64 / 10.0).is_err() {
            return Err(output.error().copied().unwrap_or(BudgetError::Overflow));
        }
        Ok(Some(output))
    }
    fn style_font(&self, tag: &str, node: &Node) -> Result<Option<Xml>, BudgetError> {
        let list = present!(node.as_list());
        if list.first().and_then(Node::text) != Some("8") {
            return Ok(None);
        }
        let kind = present!(list.get(1).and_then(Node::text));
        let mask = present!(list.get(2).and_then(int));
        if kind == "0" {
            let f = present!(list.get(3..));
            let height = present!(self.height(present!(f.first()))?);
            return Ok(Some(
                Xml::new(self.memory_budget(), tag, 9, 0)?
                    .attr(self.memory_budget(), "xsi:type", "v8ui:Font")?
                    .attr(
                        self.memory_budget(),
                        "faceName",
                        f.get(13).and_then(Node::text).unwrap_or(""),
                    )?
                    .attr(self.memory_budget(), "height", height.as_str())?
                    .attr(
                        self.memory_budget(),
                        "bold",
                        present!(f.get(4).and_then(bold)),
                    )?
                    .attr(
                        self.memory_budget(),
                        "italic",
                        present!(f.get(5).and_then(flag)),
                    )?
                    .attr(
                        self.memory_budget(),
                        "underline",
                        present!(f.get(6).and_then(flag)),
                    )?
                    .attr(
                        self.memory_budget(),
                        "strikeout",
                        present!(f.get(7).and_then(flag)),
                    )?
                    .attr(self.memory_budget(), "kind", "Absolute")?
                    .attr(
                        self.memory_budget(),
                        "scale",
                        present!(f.get(15).and_then(Node::text)),
                    )?,
            ));
        }
        if !matches!(kind, "1" | "2" | "3") {
            return Ok(None);
        }
        let mut at = 3;
        let reference = if kind != "3" {
            let id = present!(list.get(3).and_then(|n| n.at(0)).and_then(Node::text));
            let (prefix, table_name) = if kind == "2" {
                ("style:", "style_fonts")
            } else {
                ("sys:", "windows_fonts")
            };
            at = 4;
            Some(text(
                self.memory_budget(),
                &[prefix, present!(table(table_name, id))],
            )?)
        } else {
            None
        };
        let height = if mask & 2 != 0 {
            let value = present!(self.height(present!(list.get(at)))?);
            at += 1;
            Some(value)
        } else {
            None
        };
        let bold = if mask & 4 != 0 {
            let value = present!(list.get(at).and_then(bold));
            at += 1;
            Some(value)
        } else {
            None
        };
        let mut flags = [None; 3];
        for (slot, bit) in flags.iter_mut().zip([8, 16, 32]) {
            if mask & bit != 0 {
                *slot = Some(present!(list.get(at).and_then(flag)));
                at += 1;
            }
        }
        let face = if mask & 1 != 0 {
            let value = present!(list.get(at).and_then(Node::text));
            at += 1;
            Some(value)
        } else {
            None
        };
        let scale = if mask & 512 != 0 {
            Some(present!(list.get(at + 1).and_then(Node::text)))
        } else {
            None
        };
        let line_height = if mask & 256 != 0 {
            Some(present!(list.get(at + 2).and_then(Node::text)))
        } else {
            None
        };
        let capacity = 2
            + usize::from(reference.is_some())
            + usize::from(height.is_some())
            + usize::from(bold.is_some())
            + flags.iter().filter(|v| v.is_some()).count()
            + usize::from(face.is_some())
            + usize::from(scale.is_some())
            + usize::from(line_height.is_some());
        let mut root = Xml::new(self.memory_budget(), tag, capacity, 0)?.attr(
            self.memory_budget(),
            "xsi:type",
            "v8ui:Font",
        )?;
        if let Some(reference) = &reference {
            root = root.attr(self.memory_budget(), "ref", reference.as_str())?
        }
        if let Some(face) = face {
            root = root.attr(self.memory_budget(), "faceName", face)?
        }
        if let Some(height) = &height {
            root = root.attr(self.memory_budget(), "height", height.as_str())?
        }
        if let Some(bold) = bold {
            root = root.attr(self.memory_budget(), "bold", bold)?
        }
        for (name, flag) in ["italic", "underline", "strikeout"].into_iter().zip(flags) {
            if let Some(flag) = flag {
                root = root.attr(self.memory_budget(), name, flag)?
            }
        }
        root = root.attr(
            self.memory_budget(),
            "kind",
            match kind {
                "1" => "WindowsFont",
                "2" => "StyleItem",
                _ => "AutoFont",
            },
        )?;
        if let Some(scale) = scale {
            root = root.attr(self.memory_budget(), "scale", scale)?
        }
        if let Some(line_height) = line_height {
            root = root.attr(self.memory_budget(), "lineHeight", line_height)?
        }
        Ok(Some(root))
    }
    fn style_border(&self, tag: &str, node: &Node) -> Result<Option<Xml>, BudgetError> {
        if node.at(0).and_then(Node::text) != Some("3") {
            return Ok(None);
        }
        match present!(node.at(1).and_then(Node::text)) {
            "1" => {
                let id = present!(node.at(2).and_then(|n| n.at(0)).and_then(Node::text));
                let reference = text(
                    self.memory_budget(),
                    &["style:", present!(table("style_borders", id))],
                )?;
                Ok(Some(
                    Xml::new(self.memory_budget(), tag, 2, 0)?
                        .attr(self.memory_budget(), "xsi:type", "v8ui:Border")?
                        .attr(self.memory_budget(), "ref", reference.as_str())?,
                ))
            }
            "0" => {
                if node.at(6).and_then(Node::text) != Some("48312c09-257f-4b29-b280-284dd89efc1e") {
                    return Ok(None);
                }
                let style = present!(
                    node.at(3)
                        .and_then(Node::text)
                        .and_then(|id| table("border_styles", id))
                );
                let width = present!(node.at(4).and_then(Node::text));
                Ok(Some(
                    Xml::new(self.memory_budget(), tag, 2, 1)?
                        .attr(self.memory_budget(), "xsi:type", "v8ui:Border")?
                        .attr(self.memory_budget(), "width", width)?
                        .child(
                            Xml::leaf(self.memory_budget(), "v8ui:style", style, 1)?.attr(
                                self.memory_budget(),
                                "xsi:type",
                                "v8ui:ControlBorderType",
                            )?,
                        )?,
                ))
            }
            _ => Ok(None),
        }
    }
}
