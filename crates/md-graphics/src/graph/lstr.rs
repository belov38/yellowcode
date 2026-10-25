use super::read::{R, Read};
use crate::xml_out::semantic::SemanticEmit;
use brace::Node;

#[derive(Clone, Copy)]
pub(crate) enum Grammar {
    Graph,
    Spreadsheet,
}
pub(crate) struct LocalStrings<'a> {
    items: &'a [Node],
}
impl<'a> LocalStrings<'a> {
    pub(crate) fn read(value: &'a Node, read: Read<'_>, grammar: Grammar) -> R<Self> {
        let values = read.list(value)?;
        let count_node = read.at(values, 1)?;
        let count = match grammar {
            Grammar::Graph => read.int(count_node)?,
            Grammar::Spreadsheet => read.plain_int(count_node)?,
        };
        let count = usize::try_from(count).unwrap_or(0);
        for i in 0..count {
            let at = i
                .checked_add(2)
                .and_then(|at| i64::try_from(at).ok())
                .ok_or_else(|| read.invalid("index overflow"))?;
            let item = read.list(read.at(values, at)?)?;
            read.scalar(read.at(item, 0)?)?;
            read.scalar(read.at(item, 1)?)?;
        }
        let items = values.get(2..count.saturating_add(2)).unwrap_or_default();
        Ok(Self { items })
    }
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&'a str, &'a str)> {
        self.items.iter().filter_map(|item| {
            let item = item.as_list()?;
            Some((item.first()?.text()?, item.get(1)?.text()?))
        })
    }
    pub(crate) fn write(
        &self,
        writer: &mut (impl SemanticEmit + ?Sized),
        tag: &str,
        prefix: &str,
    ) -> R<()> {
        if self.items.is_empty() {
            writer.leaf_parts(&[tag], "", false)?;
            return Ok(());
        }
        writer.open_parts(&[tag])?;
        for (lang, text) in self.iter() {
            writer.open_parts(&[prefix, ":item"])?;
            writer.leaf_parts(&[prefix, ":lang"], lang, false)?;
            writer.leaf_parts(&[prefix, ":content"], text, true)?;
            writer.close_parts(&[prefix, ":item"])?;
        }
        writer.close_parts(&[tag])?;
        Ok(())
    }
}
