use super::read::{R, Read};
use crate::xml_out::semantic::SemanticEmit;
use brace::Node;
const CONNECTOR: &str = "e45c0cd8-a878-4bcb-8e1a-af934481e1cc";
const CELL: &str = "f527dc88-1d39-40b3-bcbb-d98b690ead68";
const DRAWING: &str = "b7438842-27cc-42a3-846f-2250cd9c1bc3";
const CONNECTOR_NAMES: &[&str] = &[
    "None",
    "Solid",
    "Dashed",
    "Dotted",
    "DashDotted",
    "DashDottedDotted",
];
const CELL_NAMES: &[&str] = &[
    "None",
    "Solid",
    "Dotted",
    "Double",
    "ThinDashed",
    "ThickDashed",
    "LargeDashed",
];
pub(crate) struct Line<'a> {
    width: &'a str,
    gap: bool,
    pub(crate) style: Option<(&'static str, &'static str)>,
}
impl<'a> Line<'a> {
    pub(crate) fn graph(node: &'a Node, read: Read<'_>) -> R<Self> {
        let values = read.list(node)?;
        let style = read.int(read.at(values, 3)?)?;
        let width = read.scalar(read.at(values, 4)?)?;
        let gap = read.at(values, 7)?.text() == Some("1");
        let guid = read.scalar(read.at(values, 6)?)?;
        if guid != CONNECTOR {
            return Err(read.semantic("line type", || format!("line type {guid}")));
        }
        let name = *read.at(CONNECTOR_NAMES, style)?;
        Ok(Self {
            width,
            gap,
            style: Some(("ConnectorLineType", name)),
        })
    }
    pub(crate) fn spreadsheet(values: &'a [Node], read: Read<'_>) -> R<Self> {
        let scalar = |node: &'a Node| node.text().ok_or_else(|| read.invalid("not a scalar"));
        let style = scalar(read.at(values, 3)?)?
            .trim()
            .parse::<i64>()
            .map_err(|_| read.invalid("not an integer"))?;
        let width = scalar(read.at(values, 4)?)?;
        let gap = scalar(read.at(values, 7)?)? == "1";
        let guid = scalar(read.at(values, 6)?)?;
        let kind = match guid {
            CELL => Some(("v8ui:SpreadsheetDocumentCellLineType", CELL_NAMES)),
            DRAWING => Some(("v8ui:SpreadsheetDocumentDrawingLineType", CONNECTOR_NAMES)),
            _ => None,
        };
        let style = kind
            .map(|(kind, names)| -> R<(&'static str, &'static str)> {
                let name = usize::try_from(style)
                    .ok()
                    .and_then(|index| names.get(index))
                    .ok_or_else(|| read.invalid("line style"))?;
                Ok((kind, *name))
            })
            .transpose()?;
        Ok(Self { width, gap, style })
    }
    pub(crate) fn attrs(&self) -> [(&str, &str); 2] {
        [
            ("width", self.width),
            ("gap", if self.gap { "true" } else { "false" }),
        ]
    }
    pub(crate) fn write(&self, writer: &mut (impl SemanticEmit + ?Sized), tag: &str) -> R<()> {
        let attrs = self.attrs();
        match self.style {
            Some((kind, name)) => {
                writer.open_attributes(tag, &attrs)?;
                writer.value("v8ui:style", Some(name), false, &[("xsi:type", kind)])?;
                writer.close_parts(&[tag])?;
            }
            None => writer.value(tag, None, false, &attrs)?,
        }
        Ok(())
    }
}
