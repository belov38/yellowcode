use super::{XmlAttr, XmlNode};
use crate::allocation::{Owned, Vector};
use memory_budget::BudgetError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NodeId(pub(crate) usize);
#[derive(Clone, Copy, Debug)]
#[doc(hidden)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}
impl Span {
    pub(super) fn get(self, source: &str) -> Option<&str> {
        source.get(self.start..self.end)
    }
}
#[derive(Clone, Copy, Debug)]
pub(super) enum Text {
    Source(Span),
    Pool(usize),
    Static(&'static str),
}
pub(super) struct Node {
    pub name: Span,
    pub local: Span,
    pub prefix: Option<Span>,
    pub ns: Option<Text>,
    pub parent: Option<NodeId>,
    pub first: Option<NodeId>,
    pub last: Option<NodeId>,
    pub next: Option<NodeId>,
    pub attrs: std::ops::Range<usize>,
    pub decls: std::ops::Range<usize>,
    pub text: Option<Text>,
}
pub(super) struct Attr {
    pub local: Span,
    pub prefix: Option<Span>,
    pub ns: Option<Text>,
    pub value: Text,
}
pub(super) struct Declaration {
    pub prefix: Option<Span>,
    pub raw: Span,
    pub value: Text,
}
pub(super) enum Source<'src> {
    Borrowed(&'src str),
    Owned {
        value: Owned<String>,
        logical_start: usize,
    },
}
#[doc(hidden)]
pub struct XmlArena<'src> {
    pub(super) source: Source<'src>,
    pub(super) nodes: Vector<Node>,
    pub(super) attrs: Vector<Attr>,
    pub(super) decls: Vector<Declaration>,
    pub(super) pool: Vector<Option<Owned<String>>>,
    pub(super) root: NodeId,
    pub(super) native: bool,
}
impl<'src> XmlArena<'src> {
    pub(super) fn source(&self) -> Result<&str, BudgetError> {
        match &self.source {
            Source::Borrowed(value) => Ok(value),
            Source::Owned {
                value,
                logical_start,
            } => value.get(*logical_start..).ok_or(BudgetError::Overflow),
        }
    }
    #[doc(hidden)]
    pub fn root(&self) -> Option<NodeRef<'_, 'src>> {
        self.node(self.root)
    }
    pub(crate) fn node(&self, id: NodeId) -> Option<NodeRef<'_, 'src>> {
        let data = self.nodes.get(id.0)?;
        Some(NodeRef {
            arena: self,
            id,
            data,
            local: data.local.get(self.source().ok()?)?,
            prefix: match data.prefix {
                Some(p) => Some(p.get(self.source().ok()?)?),
                None => None,
            },
            ns: data.ns.map(|t| self.text(t)).transpose().ok()?,
            text: data.text.map(|t| self.text(t)).transpose().ok()?,
        })
    }
    pub(super) fn text(&self, text: Text) -> Result<&str, BudgetError> {
        match text {
            Text::Source(span) => span.get(self.source()?).ok_or(BudgetError::Overflow),
            Text::Pool(id) => self
                .pool
                .get(id)
                .and_then(Option::as_deref)
                .map(|s| s.as_str())
                .ok_or(BudgetError::Overflow),
            Text::Static(text) => Ok(text),
        }
    }
    #[doc(hidden)]
    pub fn into_native(self) -> Result<XmlNode, BudgetError> {
        use crate::allocation::Allocation;
        if !self.native {
            return Err(BudgetError::Overflow);
        }
        let a = Allocation::Native;
        let mut built = a.vector(self.nodes.len())?;
        for _ in self.nodes.iter() {
            built.push(None)?;
        }
        let string = |s: &str| a.text(&[s])?.native();
        for at in (0..self.nodes.len()).rev() {
            let n = self.node(NodeId(at)).ok_or(BudgetError::Overflow)?;
            let mut attrs = a.vector(n.attrs().count())?;
            for attr in n.attrs() {
                attrs.push(XmlAttr {
                    ns: attr.ns().map(string).transpose()?,
                    local: string(attr.local())?,
                    prefix: attr.prefix().map(string).transpose()?,
                    value: string(attr.value())?,
                })?;
            }
            let mut decls = a.vector(n.ns_decls().count())?;
            for (prefix, uri) in n.ns_decls() {
                decls.push((prefix.map(string).transpose()?, string(uri)?))?;
            }
            let mut children = a.vector(n.children().count())?;
            for child in n.children() {
                children.push(
                    built
                        .get_mut(child.id.0)
                        .ok_or(BudgetError::Overflow)?
                        .take()
                        .ok_or(BudgetError::Overflow)?,
                )?;
            }
            *built.get_mut(at).ok_or(BudgetError::Overflow)? = Some(XmlNode {
                ns: n.ns().map(string).transpose()?,
                local: string(n.local())?,
                prefix: n.prefix().map(string).transpose()?,
                attrs: attrs.finish().native()?,
                ns_decls: decls.finish().native()?,
                children: children.finish().native()?,
                text: n.text().map(string).transpose()?,
            });
        }
        built
            .get_mut(self.root.0)
            .ok_or(BudgetError::Overflow)?
            .take()
            .ok_or(BudgetError::Overflow)
    }
}
#[derive(Clone, Copy)]
#[doc(hidden)]
pub struct NodeRef<'a, 'src> {
    arena: &'a XmlArena<'src>,
    id: NodeId,
    data: &'a Node,
    local: &'a str,
    prefix: Option<&'a str>,
    ns: Option<&'a str>,
    text: Option<&'a str>,
}
impl<'a, 'src> NodeRef<'a, 'src> {
    pub(crate) fn same_node(self, other: Self) -> bool {
        std::ptr::eq(self.arena, other.arena) && self.id == other.id
    }
    #[doc(hidden)]
    pub fn ns(self) -> Option<&'a str> {
        self.ns
    }
    #[doc(hidden)]
    pub fn local(self) -> &'a str {
        self.local
    }
    pub(crate) fn prefix(self) -> Option<&'a str> {
        self.prefix
    }
    #[doc(hidden)]
    pub fn text(self) -> Option<&'a str> {
        self.text
    }
    #[doc(hidden)]
    pub fn parent(self) -> Option<Self> {
        self.data.parent.and_then(|id| self.arena.node(id))
    }
    #[doc(hidden)]
    pub fn children(self) -> impl Iterator<Item = Self> {
        std::iter::successors(self.data.first.and_then(|id| self.arena.node(id)), |n| {
            n.data.next.and_then(|id| n.arena.node(id))
        })
    }
    #[doc(hidden)]
    pub fn attrs(self) -> impl Iterator<Item = AttrRef<'a>> {
        self.arena
            .attrs
            .get(self.data.attrs.clone())
            .into_iter()
            .flatten()
            .filter_map(move |a| {
                Some(AttrRef {
                    local: a.local.get(self.arena.source().ok()?)?,
                    prefix: match a.prefix {
                        Some(p) => Some(p.get(self.arena.source().ok()?)?),
                        None => None,
                    },
                    ns: a.ns.map(|t| self.arena.text(t)).transpose().ok()?,
                    value: self.arena.text(a.value).ok()?,
                })
            })
    }
    #[doc(hidden)]
    pub fn ns_decls(self) -> impl Iterator<Item = (Option<&'a str>, &'a str)> {
        self.arena
            .decls
            .get(self.data.decls.clone())
            .into_iter()
            .flatten()
            .filter_map(move |d| {
                Some((
                    match d.prefix {
                        Some(p) => Some(p.get(self.arena.source().ok()?)?),
                        None => None,
                    },
                    self.arena.text(d.value).ok()?,
                ))
            })
    }
    #[cfg(any(test, feature = "testing"))]
    #[doc(hidden)]
    pub fn attr(self, ns: Option<&str>, local: &str) -> Option<&'a str> {
        self.attrs()
            .find(|a| a.ns() == ns && a.local() == local)
            .map(|a| a.value())
    }
}
#[derive(Clone, Copy)]
#[doc(hidden)]
pub struct AttrRef<'a> {
    local: &'a str,
    prefix: Option<&'a str>,
    ns: Option<&'a str>,
    value: &'a str,
}
impl<'a> AttrRef<'a> {
    #[doc(hidden)]
    pub fn ns(self) -> Option<&'a str> {
        self.ns
    }
    #[doc(hidden)]
    pub fn local(self) -> &'a str {
        self.local
    }
    pub(crate) fn prefix(self) -> Option<&'a str> {
        self.prefix
    }
    #[doc(hidden)]
    pub fn value(self) -> &'a str {
        self.value
    }
}
