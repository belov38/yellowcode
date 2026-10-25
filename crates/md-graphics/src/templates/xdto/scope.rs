use crate::graph::{
    read::{R, Read},
    scalar::Text,
};
use crate::templates::allocation::{Allocation, Vector};
use std::fmt::Write;

#[derive(Clone, Copy)]
pub(in crate::templates) enum Prefix<'a> {
    Borrowed(&'a str),
    Generated(usize, usize),
}
impl Prefix<'_> {
    pub(in crate::templates) fn with_text<T>(self, f: impl FnOnce(&str) -> T) -> T {
        match self {
            Self::Borrowed(text) => f(text),
            Self::Generated(depth, ordinal) => {
                let mut output = Text::<64>::new();
                let _ = write!(output, "d{depth}p{ordinal}");
                f(output.as_str())
            }
        }
    }
    fn equal(self, other: Self) -> bool {
        match (self, other) {
            (Self::Borrowed(a), Self::Borrowed(b)) => a == b,
            (Self::Generated(a, b), Self::Generated(c, d)) => a == c && b == d,
            (Self::Generated(..), Self::Borrowed(text))
            | (Self::Borrowed(text), Self::Generated(..)) => {
                let generated = if matches!(self, Self::Generated(..)) {
                    self
                } else {
                    other
                };
                generated.with_text(|value| value == text)
            }
        }
    }
}
#[derive(Clone, Copy)]
pub(in crate::templates) struct Binding<'a> {
    pub prefix: Option<Prefix<'a>>,
    pub uri: &'a str,
}
fn equal(a: Option<Prefix<'_>>, b: Option<Prefix<'_>>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => a.equal(b),
        _ => false,
    }
}
pub(in crate::templates) trait Scoped<'a> {
    fn uri(&self, prefix: Option<Prefix<'a>>) -> Option<&'a str>;
    fn find_prefix(&self, uri: &str, root: &dyn Scoped<'a>) -> Option<Prefix<'a>>;
    fn prefix(&self, uri: &str) -> Option<Prefix<'a>>;
}
pub(in crate::templates) struct Scope<'a, 'parent> {
    parent: Option<&'parent dyn Scoped<'a>>,
    pub bindings: Vector<Binding<'a>>,
}
impl<'a, 'parent> Scope<'a, 'parent> {
    pub(in crate::templates) fn new(
        allocation: Allocation<'_>,
        parent: Option<&'parent dyn Scoped<'a>>,
    ) -> R<Self> {
        Ok(Self {
            parent,
            bindings: allocation.vector(0)?,
        })
    }
    pub(in crate::templates) fn declare(
        &mut self,
        prefix: Option<Prefix<'a>>,
        uri: &'a str,
    ) -> R<()> {
        if let Some(binding) = self
            .bindings
            .iter_mut()
            .find(|binding| equal(binding.prefix, prefix))
        {
            binding.uri = uri;
        } else {
            self.bindings.push(Binding { prefix, uri })?;
        }
        Ok(())
    }
}
impl<'a> Scoped<'a> for Scope<'a, '_> {
    fn uri(&self, prefix: Option<Prefix<'a>>) -> Option<&'a str> {
        self.bindings
            .iter()
            .find(|binding| equal(binding.prefix, prefix))
            .map(|binding| binding.uri)
            .or_else(|| self.parent.and_then(|parent| parent.uri(prefix)))
    }
    fn find_prefix(&self, uri: &str, root: &dyn Scoped<'a>) -> Option<Prefix<'a>> {
        self.bindings
            .iter()
            .find_map(|binding| {
                binding
                    .prefix
                    .filter(|prefix| binding.uri == uri && root.uri(Some(*prefix)) == Some(uri))
            })
            .or_else(|| self.parent.and_then(|parent| parent.find_prefix(uri, root)))
    }
    fn prefix(&self, uri: &str) -> Option<Prefix<'a>> {
        self.find_prefix(uri, self)
    }
}
#[derive(Clone, Copy)]
pub(in crate::templates) struct QName<'a> {
    pub prefix: Option<Prefix<'a>>,
    pub local: &'a str,
}
pub(in crate::templates) struct Ctx<'a, 'parent> {
    pub scope: Scope<'a, 'parent>,
    depth: usize,
    ordinal: usize,
}
impl<'a, 'parent> Ctx<'a, 'parent> {
    pub(in crate::templates) fn new(
        allocation: Allocation<'_>,
        parent: &'parent dyn Scoped<'a>,
        depth: usize,
    ) -> R<Self> {
        Ok(Self {
            scope: Scope::new(allocation, Some(parent))?,
            depth,
            ordinal: 0,
        })
    }
    fn need(&mut self, uri: &'a str, read: Read<'_>) -> R<Option<Prefix<'a>>> {
        if uri.is_empty() || self.scope.uri(None) == Some(uri) {
            return Ok(None);
        }
        if let Some(prefix) = self.scope.prefix(uri) {
            return Ok(Some(prefix));
        }
        self.ordinal = self
            .ordinal
            .checked_add(1)
            .ok_or_else(|| read.invalid("namespace ordinal"))?;
        let prefix = Prefix::Generated(self.depth, self.ordinal);
        self.scope.declare(Some(prefix), uri)?;
        Ok(Some(prefix))
    }
    pub(in crate::templates) fn qname(
        &mut self,
        uri: &'a str,
        local: &'a str,
        read: Read<'_>,
    ) -> R<QName<'a>> {
        Ok(QName {
            prefix: self.need(uri, read)?,
            local,
        })
    }
    pub(in crate::templates) fn attribute(
        &mut self,
        uri: &'a str,
        local: &'a str,
        read: Read<'_>,
    ) -> R<QName<'a>> {
        let prefix = if uri.is_empty() {
            None
        } else {
            Some(
                self.need(uri, read)?
                    .ok_or_else(|| read.invalid("no prefix for attribute namespace"))?,
            )
        };
        Ok(QName { prefix, local })
    }
}
