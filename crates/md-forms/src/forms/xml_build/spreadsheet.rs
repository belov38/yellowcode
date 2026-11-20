use super::builder::Builder;
use super::{FormBuildError, FormBuildResolution, Result, invalid};
use crate::forms::fe::E;
use brace::{Guid, Node};
use md_graphics::templates::mxl_build::{self, Asset, InverseLookup};
use memory_budget::Category;
use unicode_normalization::UnicodeNormalization;

const NS: &str = "http://v8.1c.ru/8.2/data/spreadsheet";
struct Lookup<'a>(&'a dyn FormBuildResolution);
impl InverseLookup for Lookup<'_> {
    fn type_id(&self, name: &str) -> Option<Guid> {
        self.0.type_id(name)
    }
    fn object_id(&self, name: &str) -> Option<Guid> {
        self.0.metadata_id(name)
    }
}
fn error(error: mxl_build::BuildError) -> FormBuildError {
    match error {
        mxl_build::BuildError::Cancelled => FormBuildError::Cancelled,
        mxl_build::BuildError::Memory(error) => FormBuildError::Budget(error),
        mxl_build::BuildError::Invalid(what) | mxl_build::BuildError::Unsupported(what) => {
            invalid("Attribute.Settings", what)
        }
    }
}
impl Builder<'_> {
    pub fn spreadsheet(&self, settings: &E) -> Result<Node> {
        self.check()?;
        let key = std::ptr::from_ref(settings) as usize;
        if let Some(value) = self.native_values.borrow().get(&key) {
            return self.copy_spreadsheet(value);
        }
        let source = self.source_of(settings)?;
        let mut assets = Vec::new();
        let mut used = std::collections::HashSet::new();
        for picture in source
            .children()
            .filter(|n| n.ns() == Some(NS) && n.local() == "picture")
        {
            self.check()?;
            for value in picture
                .children()
                .filter(|n| n.ns() == Some(NS) && n.local() == "picture")
            {
                self.check()?;
                let Some(path) = value
                    .attrs()
                    .find(|a| a.ns().is_none() && a.local() == "src")
                    .map(|a| a.value())
                else {
                    continue;
                };
                if path.len() > 4096 {
                    return Err(FormBuildError::Limit("spreadsheet asset path"));
                }
                let index = self
                    .asset_index
                    .get(&path.nfc().collect::<String>())
                    .copied()
                    .ok_or_else(|| invalid("Attribute.Settings", "missing picture asset"))?;
                if used.insert(index) {
                    let (path, bytes) = self
                        .input
                        .files
                        .get(index)
                        .ok_or_else(|| invalid("Attribute.Settings", "asset index"))?;
                    assets.push(Asset { path, bytes });
                    self.asset_uses.borrow_mut().insert(index);
                }
            }
        }
        let value = mxl_build::build_form_value_checked(
            source,
            &assets,
            &Lookup(self.resolution),
            self.memory,
            self.cancelled,
        )
        .map_err(error)?;
        let copy = self.copy_spreadsheet(&value)?;
        let mut cache = self.native_values.borrow_mut();
        cache
            .try_reserve(1)
            .map_err(|_| memory_budget::BudgetError::AllocationFailed)?;
        cache.insert(key, value);
        Ok(copy)
    }
    fn spreadsheet_bytes(&self, node: &Node, depth: usize) -> Result<u64> {
        self.check()?;
        if depth > 256 {
            return Err(FormBuildError::Limit("spreadsheet value depth"));
        }
        match node {
            Node::Atom(text) | Node::Str(text) => Ok(text.len() as u64),
            Node::List(values) => values.iter().try_fold(
                (values.len() as u64)
                    .checked_mul(std::mem::size_of::<Node>() as u64)
                    .ok_or(FormBuildError::Limit("spreadsheet value bytes"))?,
                |bytes, value| {
                    bytes
                        .checked_add(self.spreadsheet_bytes(value, depth + 1)?)
                        .ok_or(FormBuildError::Limit("spreadsheet value bytes"))
                },
            ),
        }
    }
    fn copy_spreadsheet(&self, node: &Node) -> Result<Node> {
        let bytes = self
            .spreadsheet_bytes(node, 0)?
            .checked_mul(2)
            .and_then(|n| n.checked_add(512))
            .ok_or(FormBuildError::Limit("spreadsheet copy bytes"))?;
        let charge = self.memory.try_reserve(Category::Execution, bytes)?;
        self.asset_charges.borrow_mut().push(charge);
        self.copy_spreadsheet_node(node, 0)
    }
    fn copy_spreadsheet_node(&self, node: &Node, depth: usize) -> Result<Node> {
        self.check()?;
        if depth > 256 {
            return Err(FormBuildError::Limit("spreadsheet value depth"));
        }
        match node {
            Node::Atom(text) | Node::Str(text) => {
                let mut bytes = Vec::new();
                bytes
                    .try_reserve_exact(text.len())
                    .map_err(|_| memory_budget::BudgetError::AllocationFailed)?;
                for chunk in text.as_bytes().chunks(4096) {
                    self.check()?;
                    bytes.extend_from_slice(chunk);
                }
                let text = String::from_utf8(bytes)
                    .map_err(|_| invalid("Attribute.Settings", "native text encoding"))?;
                Ok(if matches!(node, Node::Atom(_)) {
                    Node::Atom(text)
                } else {
                    Node::Str(text)
                })
            }
            Node::List(values) => {
                let mut copy = Vec::new();
                copy.try_reserve_exact(values.len())
                    .map_err(|_| memory_budget::BudgetError::AllocationFailed)?;
                for value in values {
                    copy.push(self.copy_spreadsheet_node(value, depth + 1)?);
                }
                Ok(Node::List(copy))
            }
        }
    }
}
