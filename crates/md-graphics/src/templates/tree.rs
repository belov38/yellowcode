use super::allocation::Allocation;
use super::moxel_view::{MoxelView, ViewError};
use crate::entries::EntryData;
use brace::{Doc, Node};
use memory_budget::{BudgetError, Category, Charged};

#[derive(Debug)]
pub(crate) enum TreeError {
    Memory(BudgetError),
    Brace(brace::Error),
    Semantic(&'static str),
}
impl From<brace::ManagedParseError> for TreeError {
    fn from(error: brace::ManagedParseError) -> Self {
        match error {
            brace::ManagedParseError::Memory(error) => Self::Memory(error),
            brace::ManagedParseError::Parse(error) => Self::Brace(error),
        }
    }
}
impl From<ViewError> for TreeError {
    fn from(error: ViewError) -> Self {
        match error {
            ViewError::Memory(error) => Self::Memory(error),
            ViewError::Semantic(error) => Self::Semantic(error),
        }
    }
}
pub(crate) enum Tree<'a> {
    Borrowed(&'a Node),
    Native(Node),
    Managed(Charged<Node>),
}
impl Tree<'_> {
    pub(crate) fn root(&self) -> &Node {
        match self {
            Self::Borrowed(root) => root,
            Self::Native(root) => root,
            Self::Managed(root) => root,
        }
    }
}
pub(crate) fn entry<'a>(
    data: &'a EntryData,
    allocation: Allocation<'_>,
) -> Result<Tree<'a>, TreeError> {
    match data {
        EntryData::Brace(doc) => Ok(Tree::Borrowed(&doc.root)),
        EntryData::Bytes(bytes) | EntryData::Json(bytes) => match allocation {
            Allocation::Native => brace::parse_bytes(bytes)
                .map(Tree::Native)
                .map_err(TreeError::Brace),
            Allocation::Managed(budget) => Ok(Tree::Managed(brace::parse_bytes_budgeted(
                bytes,
                budget,
                Category::Metadata,
            )?)),
        },
        EntryData::Nested(_) => Err(TreeError::Semantic("nested container")),
    }
}
enum DocOwner {
    Native(Doc),
    Managed(Charged<Doc>),
}
pub(crate) struct MoxelSource {
    head: [u8; 6],
    doc: DocOwner,
}
impl MoxelSource {
    pub(crate) fn read(bytes: &[u8], allocation: Allocation<'_>) -> Result<Self, TreeError> {
        if !bytes.starts_with(b"MOXCEL\0") {
            return Err(TreeError::Semantic("no MOXCEL signature"));
        }
        let head = bytes
            .get(7..13)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or(TreeError::Semantic("truncated"))?;
        let body = bytes.get(13..).ok_or(TreeError::Semantic("truncated"))?;
        let doc = match allocation {
            Allocation::Native => {
                DocOwner::Native(brace::parse_doc(body).map_err(TreeError::Brace)?)
            }
            Allocation::Managed(budget) => {
                DocOwner::Managed(brace::parse_doc_budgeted(body, budget, Category::Metadata)?)
            }
        };
        Ok(Self { head, doc })
    }
    pub(crate) fn view(&self) -> Result<MoxelView<'_>, TreeError> {
        let doc = match &self.doc {
            DocOwner::Native(doc) => doc,
            DocOwner::Managed(doc) => doc,
        };
        let items = doc
            .root
            .as_list()
            .ok_or(TreeError::Semantic("not a list"))?;
        Ok(MoxelView::read(items, self.head)?)
    }
}
