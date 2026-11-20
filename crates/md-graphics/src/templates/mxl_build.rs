use brace::Guid;
use memory_budget::{BudgetError, Charged, MemoryBudget};
use std::sync::atomic::AtomicBool;
mod cells;
mod document;
mod formats;
mod fragment;
pub use fragment::{build_form_value, build_form_value_checked};
mod layout;
mod nodes;
mod paths;
mod tables;
mod values;
mod xml;
use mdconf_core::xml::view::NodeRef;
use memory_budget::{BudgetedVec, Category};
const HEADERS: [&str; 6] = [
    "leftHeader",
    "leftFooter",
    "centerHeader",
    "centerFooter",
    "rightHeader",
    "rightFooter",
];
type R<T> = Result<T, BuildError>;
const ZERO: &str = "00000000-0000-0000-0000-000000000000";

#[derive(Clone, Copy, Debug)]
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
    #[error("spreadsheet build cancelled")]
    Cancelled,
    #[error(transparent)]
    Memory(#[from] BudgetError),
    #[error("invalid spreadsheet: {0}")]
    Invalid(&'static str),
    #[error("unsupported spreadsheet shape: {0}")]
    Unsupported(&'static str),
}
pub fn build(
    xml: &[u8],
    assets: &[Asset<'_>],
    lookup: &dyn InverseLookup,
    budget: &MemoryBudget,
    cancel: &AtomicBool,
) -> R<Charged<Vec<u8>>> {
    let cancelled = || cancel.load(std::sync::atomic::Ordering::Relaxed);
    let cx = nodes::Cx {
        budget,
        cancelled: &cancelled,
    };
    cx.check()?;
    let arena = xml::parse(xml, cx, &cancelled)?;
    let root = arena
        .root()
        .ok_or(BuildError::Invalid("missing document"))?;
    if root.local() != "document" || root.ns() != Some(xml::NS) {
        return Err(BuildError::Invalid("document namespace"));
    }
    let tree = encode_tree(root, assets, lookup, cx, Frame::Standalone)?;
    let body = brace::write_node_budgeted(&tree, budget, memory_budget::Category::Metadata)?;
    let mut out = memory_budget::BudgetedVec::try_with_capacity(
        budget,
        memory_budget::Category::Metadata,
        16usize
            .checked_add(body.len())
            .ok_or(BudgetError::Overflow)?,
    )?;
    out.try_extend_from_slice(b"MOXCEL\0\x08\0\x01\0\x0c\0\xef\xbb\xbf")?;
    out.try_extend_from_slice(&body)?;
    cx.check()?;
    Ok(out.into_charged_vec())
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Frame {
    Standalone,
    FormSettings,
}
fn encode_tree(
    root: NodeRef<'_, '_>,
    assets: &[Asset<'_>],
    lookup: &dyn InverseLookup,
    cx: nodes::Cx<'_>,
    frame: Frame,
) -> R<nodes::N> {
    match frame {
        Frame::Standalone => validate(root, assets, cx)?,
        Frame::FormSettings => validate_for(root, assets, cx, &[(Some(xml::XSI), "type")])?,
    }
    for e in root.children() {
        if e.local() == "font" {
            drop(values::font(e, cx, lookup)?);
        }
        if e.local() == "line" {
            drop(values::line(e, cx)?);
        }
        if e.local() == "format" {
            if e.attrs().next().is_some() || e.text().is_some_and(|s| !s.trim().is_empty()) {
                return Err(BuildError::Invalid("format shape"));
            }
            for c in e.children() {
                if c.ns() != Some(xml::NS)
                    || !(super::mxltables::FORMAT_BITS
                        .iter()
                        .any(|(_, name, _)| *name == c.local())
                        || [
                            "border",
                            "leftBorder",
                            "topBorder",
                            "rightBorder",
                            "bottomBorder",
                            "drawingBorder",
                            "drawingHaveLeftBorder",
                            "drawingHaveTopBorder",
                            "drawingHaveRightBorder",
                            "drawingHaveBottomBorder",
                            "print",
                        ]
                        .contains(&c.local()))
                {
                    return Err(BuildError::Unsupported("format element"));
                }
            }
        }
    }
    let tree = Encoder::new(root, assets, lookup, cx, frame)?.run()?;
    cx.check()?;
    Ok(tree)
}
struct Encoder<'a, 's, 'c> {
    root: NodeRef<'a, 's>,
    assets: &'c [Asset<'c>],
    lookup: &'c dyn InverseLookup,
    cx: nodes::Cx<'c>,
    frame: Frame,
    formats: BudgetedVec<NodeRef<'a, 's>>,
    fonts: BudgetedVec<NodeRef<'a, 's>>,
    lines: BudgetedVec<NodeRef<'a, 's>>,
    order: BudgetedVec<(usize, bool)>,
    font_order: BudgetedVec<usize>,
    line_order: BudgetedVec<usize>,
    group_colors: BudgetedVec<usize>,
    records: BudgetedVec<nodes::N>,
    colors: tables::Intern,
    strings: tables::Intern,
    masks: tables::Intern,
    types: tables::Intern,
    uuids: tables::Intern,
}
impl<'a, 's, 'c> Encoder<'a, 's, 'c> {
    fn new(
        root: NodeRef<'a, 's>,
        assets: &'c [Asset<'c>],
        lookup: &'c dyn InverseLookup,
        cx: nodes::Cx<'c>,
        frame: Frame,
    ) -> R<Self> {
        let collect = |tag: &str| -> R<BudgetedVec<NodeRef<'a, 's>>> {
            let mut out = BudgetedVec::new(cx.budget, Category::Metadata)?;
            for c in root.children().filter(|c| c.local() == tag) {
                if out.len() >= 4096 {
                    return Err(BuildError::Invalid("XML shared table limit"));
                }
                out.try_push(c)?;
            }
            Ok(out)
        };
        Ok(Self {
            root,
            assets,
            lookup,
            cx,
            frame,
            formats: collect("format")?,
            fonts: collect("font")?,
            lines: collect("line")?,
            order: BudgetedVec::new(cx.budget, Category::Metadata)?,
            font_order: BudgetedVec::new(cx.budget, Category::Metadata)?,
            line_order: BudgetedVec::new(cx.budget, Category::Metadata)?,
            group_colors: BudgetedVec::new(cx.budget, Category::Metadata)?,
            records: BudgetedVec::new(cx.budget, Category::Metadata)?,
            colors: tables::Intern::new(cx)?,
            strings: tables::Intern::new(cx)?,
            masks: tables::Intern::new(cx)?,
            types: tables::Intern::new(cx)?,
            uuids: tables::Intern::new(cx)?,
        })
    }
}
fn validate(root: NodeRef<'_, '_>, assets: &[Asset<'_>], cx: nodes::Cx<'_>) -> R<()> {
    validate_for(root, assets, cx, &[])
}
fn validate_for(
    root: NodeRef<'_, '_>,
    assets: &[Asset<'_>],
    cx: nodes::Cx<'_>,
    attrs: &[(Option<&str>, &str)],
) -> R<()> {
    xml::shape(
        root,
        &[
            "leftHeader",
            "leftFooter",
            "centerHeader",
            "centerFooter",
            "rightHeader",
            "rightFooter",
            "languageSettings",
            "columns",
            "rowsItem",
            "height",
            "templateMode",
            "stepDirection",
            "defaultFormatIndex",
            "drawing",
            "vg",
            "hg",
            "vgRows",
            "vgLevels",
            "verticalPageBreak",
            "horizontalPageBreak",
            "merge",
            "verticalUnmerge",
            "horizontalUnmerge",
            "namedItem",
            "printSettingsName",
            "printSettings",
            "printArea",
            "repeatRows",
            "repeatColumns",
            "font",
            "format",
            "line",
            "picture",
            "groupsBackColor",
            "groupsColor",
            "headersBackColor",
            "headersColor",
            "saveViewSettings",
            "readOnly",
            "viewSettings",
            "usedFileName",
        ],
        &[
            "columns",
            "rowsItem",
            "drawing",
            "vg",
            "hg",
            "verticalPageBreak",
            "horizontalPageBreak",
            "merge",
            "verticalUnmerge",
            "horizontalUnmerge",
            "namedItem",
            "font",
            "format",
            "line",
            "picture",
        ],
        attrs,
    )?;
    if root.children().any(|c| c.ns() != Some(xml::NS)) {
        return Err(BuildError::Invalid("document child namespace"));
    }
    for tag in ["columns", "drawing", "vg", "hg", "namedItem", "picture"] {
        if root.children().filter(|c| c.local() == tag).count() > 4096 {
            return Err(BuildError::Invalid("layout table limit"));
        }
    }
    let mut ids = BudgetedVec::new(cx.budget, Category::Metadata)?;
    for (i, c) in root
        .children()
        .filter(|c| c.local() == "columns")
        .enumerate()
    {
        let id = brace::Guid::parse(xml::text(c, "id", ZERO)?)
            .ok_or(BuildError::Invalid("column set GUID"))?;
        if i > 0 && id == Guid::NIL || ids.contains(&id) {
            return Err(BuildError::Invalid("duplicate or empty column set id"));
        }
        ids.try_push(id)?;
    }
    if assets.len() > 4096 {
        return Err(BuildError::Invalid("asset count limit"));
    }
    let mut total = 0usize;
    for (i, asset) in assets.iter().enumerate() {
        cx.check()?;
        cells::validate_path(asset.path)?;
        total = total
            .checked_add(asset.bytes.len())
            .ok_or(BuildError::Invalid("asset byte limit"))?;
        if asset.bytes.len() > 64 << 20 || total > 128 << 20 {
            return Err(BuildError::Invalid("asset byte limit"));
        }
        for a in assets.get(..i).ok_or(BuildError::Invalid("asset index"))? {
            if paths::equal(a.path, asset.path, true, cx)? {
                return Err(BuildError::Invalid("asset path collision"));
            }
        }
        let mut referenced = false;
        for path in root
            .children()
            .filter(|c| c.local() == "picture")
            .filter_map(|c| xml::child(c, "picture"))
            .filter_map(|c| xml::attr(c, None, "src"))
        {
            if paths::equal(path, asset.path, false, cx)? {
                referenced = true;
                break;
            }
        }
        if !referenced {
            return Err(BuildError::Unsupported("unreferenced asset"));
        }
    }
    Ok(())
}
