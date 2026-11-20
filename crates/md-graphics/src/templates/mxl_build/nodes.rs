use super::{BuildError, R};
use crate::allocation::{Allocation, Owned};
use brace::Node;
use memory_budget::{BudgetedVec, Category, MemoryBudget};
use std::fmt::Write;
pub(super) type N = Owned<Node>;
#[derive(Clone, Copy)]
pub(super) struct Cx<'a> {
    pub budget: &'a MemoryBudget,
    pub cancelled: &'a dyn Fn() -> bool,
}
impl Cx<'_> {
    pub fn check(self) -> R<()> {
        if (self.cancelled)() {
            Err(BuildError::Cancelled)
        } else {
            Ok(())
        }
    }
    pub fn atom(self, value: &str) -> R<N> {
        Ok(Allocation::Managed(self.budget)
            .text(&[value])?
            .map(Node::Atom))
    }
    pub fn string(self, value: &str) -> R<N> {
        Ok(Allocation::Managed(self.budget)
            .text(&[value])?
            .map(Node::Str))
    }
    pub fn int(self, value: impl std::fmt::Display) -> R<N> {
        let mut text = crate::graph::scalar::Text::<64>::new();
        write!(&mut text, "{value}").map_err(|_| BuildError::Invalid("integer overflow"))?;
        self.atom(text.as_str())
    }
    pub fn list(self, values: impl IntoIterator<Item = N>) -> R<N> {
        let mut out = Nodes::new(self)?;
        for value in values {
            out.push(value)?;
        }
        out.finish(self)
    }
    pub fn ints(self, values: impl IntoIterator<Item = i64>) -> R<N> {
        let mut out = Nodes::new(self)?;
        for value in values {
            out.push(self.int(value)?)?;
        }
        out.finish(self)
    }
}
pub(super) struct Nodes(BudgetedVec<N>);
impl Nodes {
    pub fn new(cx: Cx<'_>) -> R<Self> {
        Ok(Self(BudgetedVec::new(cx.budget, Category::Metadata)?))
    }
    pub fn push(&mut self, node: N) -> R<()> {
        self.0.try_push(node)?;
        Ok(())
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn finish(mut self, cx: Cx<'_>) -> R<N> {
        let mut out = Allocation::Managed(cx.budget).vector(self.len())?.finish();
        while let Some(node) = self.0.pop() {
            out = out.push(node)?;
        }
        Ok(out.map(|mut values| {
            values.reverse();
            Node::List(values)
        }))
    }
}
pub(super) fn copy(node: &Node, cx: Cx<'_>) -> R<N> {
    cx.check()?;
    match node {
        Node::Atom(s) => cx.atom(s),
        Node::Str(s) => cx.string(s),
        Node::List(items) => {
            let mut out = Nodes::new(cx)?;
            for n in items {
                out.push(copy(n, cx)?)?;
            }
            out.finish(cx)
        }
    }
}
