use crate::graph::{
    read::{R, Read},
    scalar::Text,
};
use crate::templates::context::Lookup;
use crate::xml_out::semantic::SemanticEmit;
use brace::{Guid, Node};
use std::fmt::Write;
#[derive(Clone, Copy)]
enum Qualifier {
    None,
    Number(u32, u32, bool),
    String(u32, bool),
    Date(&'static str),
    Binary(u32, bool),
}
struct Item<'a> {
    name: [&'a str; 2],
    namespace: Option<&'a str>,
    set: bool,
    qualifier: Qualifier,
}
fn unsigned(node: &Node) -> Option<u32> {
    node.text()?.trim().parse().ok()
}
fn length(values: &[Node]) -> Option<(u32, bool)> {
    let length = match values.get(1) {
        Some(node) => unsigned(node)?,
        None => 0,
    };
    let variable = match values.get(2) {
        Some(node) => node.text()? == "1",
        None => true,
    };
    Some((length, !variable))
}
fn item<'a>(node: &'a Node, lookup: &'a dyn Lookup) -> Option<Item<'a>> {
    let values = node.as_list()?;
    let plain = |name| Item {
        name: [name, ""],
        namespace: None,
        set: false,
        qualifier: Qualifier::None,
    };
    match values.first()?.text()? {
        "B" => Some(plain("xs:boolean")),
        "S" => {
            let (len, fixed) = length(values)?;
            Some(Item {
                qualifier: Qualifier::String(len, fixed),
                ..plain("xs:string")
            })
        }
        "R" => {
            let (len, fixed) = length(values)?;
            Some(Item {
                qualifier: Qualifier::Binary(len, fixed),
                ..plain("xs:base64Binary")
            })
        }
        "N" => {
            let digits = match values.get(1) {
                Some(node) => unsigned(node)?,
                None => 0,
            };
            let fraction = match values.get(2) {
                Some(node) => unsigned(node)?,
                None => 0,
            };
            let nonnegative = match values.get(3) {
                Some(node) => node.text()? == "1",
                None => false,
            };
            Some(Item {
                qualifier: Qualifier::Number(digits, fraction, nonnegative),
                ..plain("xs:decimal")
            })
        }
        "D" => Some(Item {
            qualifier: Qualifier::Date(match values.get(1).and_then(Node::text) {
                Some("D") => "Date",
                Some("T") => "Time",
                _ => "DateTime",
            }),
            ..plain("xs:dateTime")
        }),
        "#" => {
            let text = values.get(1)?.text()?;
            let id = Guid::parse(text)?;
            if let Some(name) = lookup.type_name(&id) {
                if !mdschema::is_ncname(name) {
                    return None;
                }
                Some(Item {
                    name: ["cfg:", name],
                    namespace: None,
                    set: matches!(
                        name.split('.').next(),
                        Some("DefinedType" | "Characteristic")
                    ),
                    qualifier: Qualifier::None,
                })
            } else if text == "f5c65050-3bbb-11d5-b988-0050bae0a95d" {
                Some(plain("v8:TypeDescription"))
            } else {
                let platform = mdschema::static_schema().platform_type(text)?;
                let fixed = mdschema::FixedNamespace::from_uri(platform.name.namespace_uri);
                let prefix = match fixed {
                    Some(mdschema::FixedNamespace::Cfg) => "cfg:",
                    Some(mdschema::FixedNamespace::V8) => "v8:",
                    Some(mdschema::FixedNamespace::Xs) => "xs:",
                    Some(mdschema::FixedNamespace::Dcsset) => "dcsset:",
                    None => "",
                };
                Some(Item {
                    name: [prefix, platform.name.local],
                    namespace: fixed.is_none().then_some(platform.name.namespace_uri),
                    set: platform.set,
                    qualifier: Qualifier::None,
                })
            }
        }
        _ => None,
    }
}
pub(crate) struct TypePattern<'a> {
    items: &'a [Node],
    lookup: &'a dyn Lookup,
    number: Option<(u32, u32, bool)>,
    string: Option<(u32, bool)>,
    date: Option<&'static str>,
    binary: Option<(u32, bool)>,
}
impl<'a> TypePattern<'a> {
    pub(crate) fn read(node: &'a Node, read: Read<'_>, lookup: &'a dyn Lookup) -> R<Self> {
        let values = read.list(node)?;
        let parse = || -> Option<Self> {
            if values.first()?.text()? != "Pattern" {
                return None;
            }
            let mut out = Self {
                items: values.get(1..)?,
                lookup,
                number: None,
                string: None,
                date: None,
                binary: None,
            };
            for node in out.items {
                match item(node, lookup)?.qualifier {
                    Qualifier::None => {}
                    Qualifier::Number(a, b, c) => out.number = Some((a, b, c)),
                    Qualifier::String(a, b) => out.string = Some((a, b)),
                    Qualifier::Date(date) => out.date = Some(date),
                    Qualifier::Binary(a, b) => out.binary = Some((a, b)),
                }
            }
            Some(out)
        };
        parse().ok_or_else(|| {
            read.semantic("value type", || {
                format!("value type {}", brace::to_compact(node))
            })
        })
    }
    pub(crate) fn write(
        &self,
        writer: &mut (impl SemanticEmit + ?Sized),
        tag: &str,
        read: Read<'_>,
    ) -> R<()> {
        if self.items.is_empty() {
            writer.value(tag, None, false, &[])?;
            return Ok(());
        }
        writer.open_parts(&[tag])?;
        for set in [false, true] {
            for node in self.items {
                let item = item(node, self.lookup).ok_or_else(|| read.invalid("value type"))?;
                if item.set == set {
                    let tag = if set { "v8:TypeSet" } else { "v8:Type" };
                    if let Some(uri) = item.namespace {
                        let depth = writer
                            .depth()
                            .checked_add(1)
                            .ok_or_else(|| read.invalid("type namespace depth"))?;
                        let mut prefix = Text::<64>::new();
                        write!(&mut prefix, "d{depth}p1")
                            .map_err(|_| read.invalid("type namespace prefix"))?;
                        let mut key = Text::<72>::new();
                        write!(&mut key, "xmlns:{}", prefix.as_str())
                            .map_err(|_| read.invalid("type namespace declaration"))?;
                        writer.text_fragments(
                            tag,
                            &[
                                prefix.as_str(),
                                ":",
                                item.name
                                    .get(1)
                                    .copied()
                                    .ok_or_else(|| read.invalid("type name"))?,
                            ],
                            &[(key.as_str(), uri)],
                        )?;
                    } else {
                        let attrs: &[(&str, &str)] = if item.name.first() == Some(&"dcsset:") {
                            &[("xmlns:dcsset", mdschema::FixedNamespace::Dcsset.uri())]
                        } else {
                            &[]
                        };
                        writer.text_fragments(tag, &item.name, attrs)?;
                    }
                }
            }
        }
        if let Some((digits, fraction, nonnegative)) = self.number {
            writer.open_parts(&["v8:NumberQualifiers"])?;
            integer(writer, "v8:Digits", digits, read)?;
            integer(writer, "v8:FractionDigits", fraction, read)?;
            writer.value(
                "v8:AllowedSign",
                Some(if nonnegative { "Nonnegative" } else { "Any" }),
                false,
                &[],
            )?;
            writer.close_parts(&["v8:NumberQualifiers"])?;
        }
        if let Some((len, fixed)) = self.string {
            write_length(writer, "v8:StringQualifiers", len, fixed, read)?;
        }
        if let Some(date) = self.date {
            writer.open_parts(&["v8:DateQualifiers"])?;
            writer.value("v8:DateFractions", Some(date), false, &[])?;
            writer.close_parts(&["v8:DateQualifiers"])?;
        }
        if let Some((len, fixed)) = self.binary {
            write_length(writer, "v8:BinaryDataQualifiers", len, fixed, read)?;
        }
        writer.close_parts(&[tag])?;
        Ok(())
    }
}
fn integer(
    writer: &mut (impl SemanticEmit + ?Sized),
    tag: &str,
    value: u32,
    read: Read<'_>,
) -> R<()> {
    let mut text = Text::<16>::new();
    write!(&mut text, "{value}").map_err(|_| read.invalid("type qualifier format"))?;
    writer.value(tag, Some(text.as_str()), false, &[])?;
    Ok(())
}
fn write_length(
    writer: &mut (impl SemanticEmit + ?Sized),
    tag: &str,
    len: u32,
    fixed: bool,
    read: Read<'_>,
) -> R<()> {
    writer.open_parts(&[tag])?;
    integer(writer, "v8:Length", len, read)?;
    writer.value(
        "v8:AllowedLength",
        Some(if fixed { "Fixed" } else { "Variable" }),
        false,
        &[],
    )?;
    writer.close_parts(&[tag])?;
    Ok(())
}
