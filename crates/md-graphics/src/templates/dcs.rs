use super::{
    allocation::{Allocation, Owned},
    context::Lookup,
    xdto::{
        V8, V8UI, XS, XSI, normalize,
        overlay::View,
        visit::{Policy, Writer},
    },
};
use crate::{
    Configuration,
    graph::read::{R, Read},
};
use std::cell::Cell;
const NS_SCHEMA: &str = "http://v8.1c.ru/8.1/data-composition-system/schema";
const NS_SETTINGS: &str = "http://v8.1c.ru/8.1/data-composition-system/settings";

const ROOT_NS: [(Option<&str>, &str); 8] = [
    (None, NS_SCHEMA),
    (
        Some("dcscom"),
        "http://v8.1c.ru/8.1/data-composition-system/common",
    ),
    (
        Some("dcscor"),
        "http://v8.1c.ru/8.1/data-composition-system/core",
    ),
    (Some("dcsset"), NS_SETTINGS),
    (Some("v8"), V8),
    (Some("v8ui"), V8UI),
    (Some("xs"), XS),
    (Some("xsi"), XSI),
];

const ORDER: [&str; 14] = [
    "dataSource",
    "dataSet",
    "dataSetLink",
    "calculatedField",
    "totalField",
    "parameter",
    "nestedSchema",
    "template",
    "fieldTemplate",
    "groupTemplate",
    "groupHeaderTemplate",
    "totalFieldsTemplate",
    "defaultSettings",
    "settingsVariant",
];

struct Settings<'a> {
    data: &'a [u8],
    lengths: &'a [u8],
    start: usize,
    count: usize,
}
impl<'a> Settings<'a> {
    fn iter(&self) -> impl Iterator<Item = &'a [u8]> {
        let mut position = self.start;
        self.lengths
            .as_chunks::<8>()
            .0
            .iter()
            .take(self.count)
            .filter_map(move |length| {
                let length = usize::try_from(u64::from_le_bytes(*length)).ok()?;
                let end = position.checked_add(length)?;
                let output = self.data.get(position..end)?;
                position = end;
                Some(output)
            })
    }
}
struct PartsView<'a> {
    schema: &'a [u8],
    settings: Settings<'a>,
    tail: &'a [u8],
}
fn split_view(data: &[u8]) -> Option<PartsView<'_>> {
    let number = |at: usize| -> Option<u32> {
        Some(u32::from_le_bytes(
            data.get(at..at.checked_add(4)?)?.try_into().ok()?,
        ))
    };
    if number(0)? != 0 {
        return None;
    }
    let count = usize::try_from(number(4)?).ok()?;
    let table_end = count.checked_add(1)?.checked_mul(8)?.checked_add(8)?;
    let lengths = data.get(8..table_end)?;
    let mut cursor = table_end;
    let mut schema_end = cursor;
    for (ordinal, length) in lengths.as_chunks::<8>().0.iter().enumerate() {
        cursor = cursor.checked_add(usize::try_from(u64::from_le_bytes(*length)).ok()?)?;
        if cursor > data.len() {
            return None;
        }
        if ordinal == 0 {
            schema_end = cursor
        }
    }
    Some(PartsView {
        schema: data.get(table_end..schema_end)?,
        settings: Settings {
            data,
            lengths: lengths.get(8..)?,
            start: schema_end,
            count,
        },
        tail: data.get(cursor..)?,
    })
}
pub(super) fn app_index(text: Option<&str>, len: usize) -> Option<usize> {
    let i: i64 = text?.trim().parse().ok()?;
    let len_i = i64::try_from(len).ok()?;
    let i = if i < 0 { i.checked_add(len_i)? } else { i };
    usize::try_from(i).ok().filter(|&i| i < len)
}
pub(crate) fn convert_with(
    data: &[u8],
    lookup: &dyn Lookup,
    allocation: Allocation<'_>,
) -> R<Owned<Vec<u8>>> {
    let read = Read::new(allocation);
    let parts = split_view(data).ok_or_else(|| read.invalid("DCS wrapper"))?;
    let schema = normalize::parse(parts.schema, allocation)?;
    let dcs = schema
        .root()
        .and_then(|root| root.children().next())
        .filter(|node| node.local() == "dataCompositionSchema")
        .ok_or_else(|| read.invalid("DCS schema root"))?;
    let mut settings = allocation.vector(0)?;
    for data in parts.settings.iter() {
        let document = normalize::parse(data, allocation)?;
        settings.push(document)?;
    }
    let variants = dcs
        .children()
        .filter(|node| node.local() == "settingsVariant")
        .count();
    if variants != settings.len() {
        return Err(read.invalid("DCS settings count"));
    }
    let tail = if parts.tail.iter().all(u8::is_ascii_whitespace) {
        None
    } else {
        Some(normalize::parse(parts.tail, allocation)?)
    };
    let mut appearances = allocation.vector(0)?;
    let mut items = allocation.vector(0)?;
    for node in dcs.children() {
        items.push(node)?
    }
    if let Some(tail) = &tail {
        let root = tail
            .root()
            .ok_or_else(|| read.invalid("DCS trailing root"))?;
        let rd = root
            .children()
            .next()
            .ok_or_else(|| read.invalid("DCS trailing schema"))?;
        for node in rd.children() {
            items.push(node)?
        }
        for node in root.children().filter(|node| node.local() == "appearance") {
            appearances.push(node)?
        }
    }
    let mut keyed = allocation.vector(items.len())?;
    for (ordinal, node) in items.iter().enumerate() {
        let rank = ORDER
            .iter()
            .position(|name| *name == node.local())
            .ok_or_else(|| read.invalid("DCS schema child"))?;
        keyed.push((rank, ordinal, *node))?;
    }
    drop(items);
    keyed.sort_unstable_by_key(|(rank, ordinal, _)| (*rank, *ordinal));
    let failed = Cell::new(false);
    let mut writer = Writer::new(
        allocation,
        Policy {
            lookup: Some(lookup),
            appearances: &appearances,
            failed: &failed,
        },
    )?;
    writer
        .output
        .raw("\u{feff}<?xml version=\"1.0\" encoding=\"UTF-8\"?>")?;
    let scope = writer.root(&ROOT_NS, "DataCompositionSchema")?;
    let mut variant = 0usize;
    for (_, _, node) in keyed.iter() {
        let append = if node.local() == "settingsVariant" {
            let root = settings
                .get(variant)
                .and_then(|arena| arena.root())
                .ok_or_else(|| read.invalid("DCS settings variant"))?;
            variant = variant
                .checked_add(1)
                .ok_or_else(|| read.invalid("DCS settings index"))?;
            Some(View::Source(root))
        } else {
            None
        };
        writer.write(View::Source(*node), 2, &scope, None, append)?;
    }
    writer.output.raw("</DataCompositionSchema>")?;
    if failed.get() {
        return Err(read.invalid("DCS reference"));
    }
    Ok(writer.output.finish())
}
pub(crate) fn convert(cfg: &Configuration, data: &[u8]) -> Option<Vec<u8>> {
    convert_with(data, cfg, Allocation::Native)
        .ok()?
        .native()
        .ok()
}
