use super::{
    allocation::{Allocation, Owned, Vector},
    xdto::{CFG, STYLE, V8, V8UI, XS, XSI, normalize},
};
use brace::Guid;
use memory_budget::{BudgetError, Charged, MemoryBudget};
use std::sync::atomic::{AtomicBool, Ordering};

#[path = "dcs_build_tree.rs"]
mod tree;
#[path = "dcs_build_write.rs"]
mod write;
use tree::Node;

const SCHEMA: &str = "http://v8.1c.ru/8.1/data-composition-system/schema";
const SETTINGS: &str = "http://v8.1c.ru/8.1/data-composition-system/settings";
const AREA: &str = "http://v8.1c.ru/8.1/data-composition-system/area-template";
const CORE: &str = "http://v8.1c.ru/8.1/data-composition-system/core";
const XR: &str = "http://v8.1c.ru/8.3/xcf/readable";
const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";
const MAX_DEPTH: usize = 128;
const TEMPLATE_TAGS: [&str; 5] = [
    "template",
    "fieldTemplate",
    "groupTemplate",
    "groupHeaderTemplate",
    "totalFieldsTemplate",
];
const SCHEMA_TAGS: [&str; 9] = [
    "dataSource",
    "dataSet",
    "dataSetLink",
    "calculatedField",
    "totalField",
    "parameter",
    "nestedSchema",
    "defaultSettings",
    "settingsVariant",
];
type Result<T> = std::result::Result<T, BuildError>;

#[derive(Clone, Copy)]
struct Context<'a> {
    allocation: Allocation<'a>,
    cancelled: &'a dyn Fn() -> bool,
}
impl Context<'_> {
    fn check(self) -> Result<()> {
        if (self.cancelled)() {
            Err(BuildError::Cancelled)
        } else {
            Ok(())
        }
    }
    fn xml_name(self, name: &str) -> Result<()> {
        if name.is_empty() {
            return Err(BuildError::Invalid("XML name"));
        }
        let mut next = 0;
        for (at, character) in name.char_indices() {
            if at >= next {
                self.check()?;
                next = at.saturating_add(4096);
            }
            let mut scalar = [b'_', 0, 0, 0, 0];
            let prefix = usize::from(at != 0);
            let size = character
                .encode_utf8(scalar.get_mut(prefix..).ok_or(BudgetError::Overflow)?)
                .len();
            let text =
                std::str::from_utf8(scalar.get(..prefix + size).ok_or(BudgetError::Overflow)?)
                    .map_err(|_| BuildError::Invalid("XML name"))?;
            if !mdschema::is_ncname(text) {
                return Err(BuildError::Invalid("XML name"));
            }
        }
        Ok(())
    }
    fn equal(self, first: &str, second: &str) -> Result<bool> {
        self.check()?;
        if first.len() != second.len() {
            return Ok(false);
        }
        for (a, b) in first
            .as_bytes()
            .chunks(4096)
            .zip(second.as_bytes().chunks(4096))
        {
            self.check()?;
            if a != b {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

pub struct Asset<'a> {
    pub path: &'a str,
    pub bytes: &'a [u8],
}

pub trait InverseLookup {
    fn type_id(&self, name: &str) -> Option<Guid>;
    fn object_id(&self, name: &str) -> Option<Guid>;
}

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("DCS memory: {0}")]
    Memory(#[from] BudgetError),
    #[error("DCS build cancelled")]
    Cancelled,
    #[error("invalid DCS: {0}")]
    Invalid(&'static str),
}

pub fn build(
    xml: &[u8],
    assets: &[Asset<'_>],
    lookup: &dyn InverseLookup,
    budget: &MemoryBudget,
    cancelled: &AtomicBool,
) -> Result<Charged<Vec<u8>>> {
    let poll = || cancelled.load(Ordering::Relaxed);
    let cx = Context {
        allocation: Allocation::Managed(budget),
        cancelled: &poll,
    };
    cx.check()?;
    let document =
        normalize::parse_checked(xml, cx.allocation, cx.cancelled).map_err(
            |error| match error {
                normalize::CheckedError::Cancelled => BuildError::Cancelled,
                normalize::CheckedError::Read(crate::graph::read::ReadError::Memory(error)) => {
                    BuildError::Memory(error)
                }
                normalize::CheckedError::Read(crate::graph::read::ReadError::Semantic {
                    ..
                }) => BuildError::Invalid("XML"),
            },
        )?;
    cx.check()?;
    let root = document
        .root()
        .filter(|n| n.ns() == Some(SCHEMA) && n.local() == "DataCompositionSchema")
        .ok_or(BuildError::Invalid("schema root"))?;
    if root.attrs().next().is_some() || root.text().is_some() {
        return Err(BuildError::Invalid("schema root content"));
    }
    let mut schema = cx.allocation.vector(0)?;
    let mut templates = cx.allocation.vector(0)?;
    let mut settings = cx.allocation.vector(0)?;
    let converter = tree::Converter {
        cx,
        lookup,
        assets,
        embedded: false,
    };
    converter.identity(root)?;
    for child in root.children() {
        cx.check()?;
        if child.ns() != Some(SCHEMA)
            || (!SCHEMA_TAGS.contains(&child.local()) && !TEMPLATE_TAGS.contains(&child.local()))
        {
            return Err(BuildError::Invalid("schema child"));
        }
        let mut node = converter.convert(child, 3)?;
        if TEMPLATE_TAGS.contains(&node.local) {
            templates.push(node)?;
        } else {
            if node.local == "settingsVariant" {
                let mut positions = node
                    .children
                    .iter()
                    .enumerate()
                    .filter(|(_, n)| n.ns == SETTINGS && n.local == "settings")
                    .map(|(i, _)| i);
                let at = positions
                    .next()
                    .ok_or(BuildError::Invalid("settingsVariant settings"))?;
                if positions.next().is_some() {
                    return Err(BuildError::Invalid("settingsVariant settings"));
                }
                let mut value = tree::remove(&mut node.children, at)?;
                value.ns = SETTINGS;
                value.local = "Settings";
                if !value.attrs.is_empty() {
                    return Err(BuildError::Invalid("settings attributes"));
                }
                settings.push(value)?;
            }
            schema.push(node)?;
        }
    }
    let mut appearances = cx.allocation.vector(0)?;
    let mut keys = cx.allocation.vector(0)?;
    for template in templates.iter_mut() {
        tree::extract_appearances(template, &mut appearances, &mut keys, cx)?;
    }
    let count = u32::try_from(settings.len()).map_err(|_| BuildError::Invalid("settings count"))?;
    let mut parts = cx.allocation.vector(0)?;
    parts.push(write::schema_file(&schema, &[], cx)?)?;
    for setting in settings.iter() {
        parts.push(write::settings_file(setting, cx)?)?;
    }
    let tail = write::schema_file(&templates, &appearances, cx)?;
    let header_len = parts
        .len()
        .checked_mul(8)
        .and_then(|n| n.checked_add(8))
        .ok_or(BudgetError::Overflow)?;
    let size = parts
        .iter()
        .try_fold(header_len, |size, part| {
            size.checked_add(part.len()).ok_or(BudgetError::Overflow)
        })?
        .checked_add(tail.len())
        .ok_or(BudgetError::Overflow)?;
    let mut output = cx.allocation.vector(size)?;
    output.extend(&0u32.to_le_bytes())?;
    output.extend(&count.to_le_bytes())?;
    for part in parts.iter() {
        output.extend(
            &u64::try_from(part.len())
                .map_err(|_| BudgetError::Overflow)?
                .to_le_bytes(),
        )?;
    }
    for part in parts.iter() {
        cx.check()?;
        output.extend(part)?;
    }
    output.extend(&tail)?;
    cx.check()?;
    Ok(output.finish().managed()?)
}

#[path = "dcs_build_embedded.rs"]
mod embedded;
#[doc(hidden)]
pub use embedded::{
    EmbeddedChild, EmbeddedNamespaces, EmbeddedRoot, write_embedded, write_embedded_checked,
};
