use super::{
    Fail, TemplateType,
    allocation::{Allocation, Owned},
    codec_base64,
    context::Lookup,
    entry_bytes,
    files::FileSet,
    links,
};
use crate::{Configuration, EntryData};
use brace::Node;
use memory_budget::BudgetError;
type Files = Vec<(String, Vec<u8>)>;
#[derive(Debug)]
pub(crate) enum SimpleError {
    Memory(BudgetError),
    Failed,
    Path,
}
impl From<BudgetError> for SimpleError {
    fn from(error: BudgetError) -> Self {
        Self::Memory(error)
    }
}
impl SimpleError {
    pub(crate) fn key(&self) -> Fail {
        match self {
            Self::Path => "path",
            Self::Failed | Self::Memory(_) => "failed",
        }
    }
}
const HELP_XML: &str = "\u{feff}<?xml version=\"1.0\" encoding=\"UTF-8\"?>\r\n<Help xmlns=\"http://v8.1c.ru/8.3/xcf/extrnprops\" xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" version=\"2.21\">\r\n";
fn required<T>(value: Option<T>) -> Result<T, SimpleError> {
    value.ok_or(SimpleError::Failed)
}
fn binary_with(allocation: Allocation<'_>, entry: &Node) -> Result<Owned<Files>, SimpleError> {
    let bytes = required(codec_base64::node_bytes(
        allocation,
        required(entry.get(&[1, 0]))?,
    )?)?;
    let mut files = FileSet::new(allocation)?;
    files.put(&["Template.bin"], bytes)?;
    Ok(files.finish()?)
}
fn plain_path(path: &str) -> bool {
    path.split('/').all(|part| {
        !part.is_empty() && part != "." && part != ".." && !part.contains(['\\', ':', '\0'])
    })
}
fn html_with(
    allocation: Allocation<'_>,
    lookup: &dyn Lookup,
    entry: &Node,
) -> Result<Owned<Files>, SimpleError> {
    let n = usize::try_from(required(entry.at(1).and_then(Node::as_i64))?).unwrap_or(0);
    let mut files = FileSet::new(allocation)?;
    let mut languages = allocation.vector(0)?;
    for i in 0..n {
        let at = i
            .checked_mul(2)
            .and_then(|n| n.checked_add(2))
            .ok_or(BudgetError::Overflow)?;
        let lang = required(entry.at(at).and_then(Node::text))?;
        let payload = required(entry.get(&[at.checked_add(1).ok_or(BudgetError::Overflow)?, 0]))?;
        let bytes = required(codec_base64::node_bytes(allocation, payload)?)?;
        languages.push(lang)?;
        let bytes = links::rewrite(allocation, bytes, lookup)?;
        files.put(&["Template/", lang, ".html"], bytes)?;
    }
    let mut at = n
        .checked_mul(2)
        .and_then(|n| n.checked_add(2))
        .ok_or(BudgetError::Overflow)?;
    let m = match entry.at(at) {
        Some(value) => usize::try_from(required(value.as_i64())?).unwrap_or(0),
        None => 0,
    };
    at = at.checked_add(1).ok_or(BudgetError::Overflow)?;
    for _ in 0..m {
        let name = required(entry.at(at).and_then(Node::text))?;
        let payload = required(entry.get(&[at.checked_add(2).ok_or(BudgetError::Overflow)?, 0]))?;
        let bytes = required(codec_base64::node_bytes(allocation, payload)?)?;
        files.put(&["Template/_files/", name], bytes)?;
        at = at.checked_add(3).ok_or(BudgetError::Overflow)?;
    }
    let mut xml = allocation.vector(0)?;
    xml.extend(HELP_XML.as_bytes())?;
    for lang in languages.iter() {
        xml.extend(b"\t<Page>")?;
        xml.extend(lang.as_bytes())?;
        xml.extend(b"</Page>\r\n")?;
    }
    xml.extend(b"</Help>")?;
    files.put(&["Template.xml"], xml.finish())?;
    let files = files.finish()?;
    if !files.iter().all(|(path, _)| plain_path(path)) {
        return Err(SimpleError::Path);
    }
    Ok(files)
}
pub(crate) fn convert_with(
    allocation: Allocation<'_>,
    lookup: &dyn Lookup,
    data: &EntryData,
    raw: &[u8],
    kind: TemplateType,
) -> Result<Owned<Files>, SimpleError> {
    use TemplateType::*;
    match kind {
        BinaryData | AddIn => match data {
            EntryData::Brace(doc) => binary_with(allocation, &doc.root),
            _ => Err(SimpleError::Failed),
        },
        HTMLDocument => match data {
            EntryData::Brace(doc) => html_with(allocation, lookup, &doc.root),
            _ => Err(SimpleError::Failed),
        },
        TextDocument | DataCompositionAppearanceTemplate => {
            let bytes = allocation.bytes(required(entry_bytes(data, raw))?)?;
            let mut files = FileSet::new(allocation)?;
            files.put(
                &[if kind == TextDocument {
                    "Template.txt"
                } else {
                    "Template.xml"
                }],
                bytes,
            )?;
            Ok(files.finish()?)
        }
        _ => Err(SimpleError::Failed),
    }
}
pub(crate) fn convert_simple(
    cfg: &Configuration,
    data: &EntryData,
    raw: &[u8],
    kind: TemplateType,
) -> Result<Files, Fail> {
    convert_with(Allocation::Native, cfg, data, raw, kind)
        .map_err(|e| e.key())?
        .native()
        .map_err(|_| "failed")
}
