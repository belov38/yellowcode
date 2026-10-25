use super::lstr::Grammar;
use super::read::{R, Read};
use super::scalar::Text;
use crate::xml_out::semantic::SemanticEmit;
use brace::Node;
use std::fmt::Write;

pub(crate) enum TypedValue<'a> {
    Undefined,
    String(&'a str),
    Number(&'a str),
    Bool(bool),
    Date(Text<96>),
}
fn chars(text: &str, start: usize, end: usize) -> &str {
    let begin = text
        .char_indices()
        .nth(start)
        .map(|(at, _)| at)
        .unwrap_or(text.len());
    let end = text
        .char_indices()
        .nth(end)
        .map(|(at, _)| at)
        .unwrap_or(text.len());
    text.get(begin..end).unwrap_or_default()
}
impl<'a> TypedValue<'a> {
    pub(crate) fn read(value: &'a Node, read: Read<'_>, grammar: Grammar) -> R<Self> {
        let values = read.list(value)?;
        let first = read.at(values, 0)?;
        let kind = first.text().unwrap_or_default();
        let parsed = match kind {
            "U" => Self::Undefined,
            "S" => Self::String(read.scalar(read.at(values, 1)?)?),
            "N" => Self::Number(read.scalar(read.at(values, 1)?)?),
            "B" => Self::Bool(read.at(values, 1)?.text() == Some("1")),
            "D" => {
                let text = read.scalar(read.at(values, 1)?)?;
                let mut date = Text::new();
                write!(
                    &mut date,
                    "{}-{}-{}T{}:{}:{}",
                    chars(text, 0, 4),
                    chars(text, 4, 6),
                    chars(text, 6, 8),
                    chars(text, 8, 10),
                    chars(text, 10, 12),
                    chars(text, 12, 14)
                )
                .map_err(|_| read.invalid("date format overflow"))?;
                Self::Date(date)
            }
            _ => {
                return Err(match grammar {
                    Grammar::Graph => {
                        read.semantic("typed value", || format!("typed value {kind}"))
                    }
                    Grammar::Spreadsheet => {
                        read.semantic("tvalue", || format!("tvalue {}", brace::to_compact(value)))
                    }
                });
            }
        };
        Ok(parsed)
    }
    fn parts(&self) -> (Option<&str>, bool, (&'static str, &'static str)) {
        match self {
            Self::Undefined => (None, false, ("xsi:nil", "true")),
            Self::String(text) => (Some(text), true, ("xsi:type", "xs:string")),
            Self::Number(text) => (Some(text), false, ("xsi:type", "xs:decimal")),
            Self::Bool(value) => (
                Some(if *value { "true" } else { "false" }),
                false,
                ("xsi:type", "xs:boolean"),
            ),
            Self::Date(text) => (Some(text.as_str()), false, ("xsi:type", "xs:dateTime")),
        }
    }
    pub(crate) fn write(&self, writer: &mut (impl SemanticEmit + ?Sized), tag: &str) -> R<()> {
        let (text, normalize, attr) = self.parts();
        writer.value(tag, text, normalize, &[attr])?;
        Ok(())
    }
}
