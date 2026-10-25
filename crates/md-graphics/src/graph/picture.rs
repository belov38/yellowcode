use super::read::{R, Read};
use crate::templates::{
    allocation::Owned,
    context::{self, Lookup},
};
use crate::xml_out::semantic::SemanticEmit;
use brace::{Guid, Node};

pub(crate) struct GraphPicture {
    pub(crate) text: Owned<String>,
    pub(crate) transparent: bool,
}
impl GraphPicture {
    pub(crate) fn read(value: &Node, read: Read<'_>, lookup: &dyn Lookup) -> R<Option<Self>> {
        let values = read.list(value)?;
        let kind = read.at(values, 1)?;
        if kind.text() == Some("0") {
            return Ok(None);
        }
        if kind.text() != Some("1") {
            return Err(read.semantic("picture kind", || {
                format!("picture kind {}", brace::to_compact(kind))
            }));
        }
        let reference = read.at(values, 2)?;
        let first = read.item(reference, 0)?;
        let standard = context::std_picture;
        let text = if first.text() == Some("0") {
            let key = read.item(reference, 1)?.text();
            let name = key.and_then(Guid::parse).and_then(|id| lookup.name(&id));
            if let Some(name) = name {
                Some(read.allocation.text(&[name])?)
            } else {
                key.and_then(standard)
                    .map(|name| read.allocation.text(&["StdPicture.", name]))
                    .transpose()?
            }
        } else {
            match first.text().and_then(standard) {
                Some("") => Some(read.allocation.text(&[""])?),
                Some(name) => {
                    let name = read.allocation.replace_text(name, "StdPicture.", "")?;
                    Some(read.allocation.text(&["StdPicture.", name.as_str()])?)
                }
                None => None,
            }
        }
        .ok_or_else(|| {
            read.semantic("picture ref", || {
                format!("picture ref {}", brace::to_compact(reference))
            })
        })?;
        if read.at(values, 4)?.text() != Some("-1") {
            return Err(read.invalid("picture transparent pixel"));
        }
        let transparent = read.at(values, 6)?.text() == Some("1");
        Ok(Some(Self { text, transparent }))
    }
}

pub(crate) struct Picture<'a> {
    attrs: [Option<(&'static str, &'a str)>; 3],
    text: Option<Owned<String>>,
    reference: bool,
}
impl<'a> Picture<'a> {
    pub(crate) fn lookup(values: &'a [Node], read: Read<'_>, lookup: &dyn Lookup) -> R<Self> {
        let names = |key: &str| {
            Guid::parse(key)
                .and_then(|id| lookup.name(&id))
                .and_then(|name| name.split_once('.').map(|(_, name)| name))
        };
        Self::read(values, read, Some(&names))
    }
    pub(crate) fn read<'n>(
        values: &'a [Node],
        read: Read<'_>,
        names: Option<&crate::graph::read::NameResolver<'n, '_>>,
    ) -> R<Self> {
        let scalar = |node: &'a Node| node.text().ok_or_else(|| read.invalid("not a scalar"));
        let kind = scalar(read.at(values, 1)?)?;
        let mut out = Self {
            attrs: [None; 3],
            text: None,
            reference: false,
        };
        if kind == "0" {
            return Ok(out);
        }
        if scalar(read.at(values, 6)?)? == "0"
            && let Some(slot) = out.attrs.get_mut(0)
        {
            *slot = Some(("t", "false"));
        }
        let x = scalar(read.at(values, 4)?)?;
        if x != "-1" {
            let y = scalar(read.at(values, 5)?)?;
            if let Some(slot) = out.attrs.get_mut(1) {
                *slot = Some(("tx", x));
            }
            if let Some(slot) = out.attrs.get_mut(2) {
                *slot = Some(("ty", y));
            }
        }
        match kind {
            "1" => {
                let reference = read
                    .at(values, 2)?
                    .as_list()
                    .ok_or_else(|| read.invalid("not a list"))?;
                let key = scalar(read.at(reference, 0)?)?;
                let name = if key == "0" {
                    let key = reference.get(1).and_then(Node::text);
                    key.and_then(|key| names.and_then(|names| names(key)))
                        .or_else(|| key.and_then(|key| context::merged("std_pictures", key)))
                } else {
                    context::merged("std_pictures", key)
                }
                .ok_or_else(|| read.invalid("picture ref"))?;
                out.text = Some(read.allocation.text(&["v8ui:", name])?);
                out.reference = true;
            }
            "3" => {
                let value = read
                    .at(values, 7)?
                    .get(&[0, 0])
                    .ok_or_else(|| read.invalid("picture data"))?;
                let text = scalar(value)?;
                let text = text.strip_prefix("#base64:").unwrap_or(text);
                out.text = Some(read.allocation.replace_text(text, "\r\r\n", "\r\n")?);
            }
            _ => return Err(read.semantic("picture kind", || format!("picture kind {kind}"))),
        }
        Ok(out)
    }
    pub(crate) fn write(&self, writer: &mut (impl SemanticEmit + ?Sized), tag: &str) -> R<()> {
        let mut attrs = [("", ""); 4];
        let mut count = 0;
        for attr in self.attrs.iter().flatten() {
            let slot = attrs.get_mut(count).ok_or_else(|| {
                Read::new(crate::templates::allocation::Allocation::Native).invalid("picture attrs")
            })?;
            *slot = *attr;
            count += 1;
        }
        let text = match self.text.as_ref() {
            Some(text) if self.reference => {
                let slot = attrs.get_mut(count).ok_or_else(|| {
                    Read::new(crate::templates::allocation::Allocation::Native)
                        .invalid("picture attrs")
                })?;
                *slot = ("ref", text.as_str());
                count += 1;
                None
            }
            Some(text) => Some(text.as_str()),
            None => None,
        };
        writer.value(
            tag,
            text,
            false,
            attrs.get(..count).ok_or_else(|| {
                Read::new(crate::templates::allocation::Allocation::Native).invalid("picture attrs")
            })?,
        )?;
        Ok(())
    }
}
