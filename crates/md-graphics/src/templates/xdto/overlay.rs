use super::{XR, XSI};
use crate::xml::view::NodeRef;

#[derive(Clone, Copy)]
pub(in crate::templates) enum Value<'a> {
    Plain(&'a str),
    Fragments([&'a str; 2]),
    QName(super::scope::QName<'a>),
}
impl<'a> Value<'a> {
    pub(in crate::templates) fn plain(self) -> Option<&'a str> {
        if let Self::Plain(value) = self {
            Some(value)
        } else {
            None
        }
    }
    pub(in crate::templates) fn empty(self) -> bool {
        match self {
            Self::Plain(value) => value.is_empty(),
            Self::Fragments(parts) => parts.iter().all(|part| part.is_empty()),
            Self::QName(name) => name.local.is_empty() && name.prefix.is_none(),
        }
    }
}
#[derive(Clone, Copy)]
pub(in crate::templates) enum View<'a, 'src> {
    Source(NodeRef<'a, 'src>),
    Rename {
        node: NodeRef<'a, 'src>,
        ns: &'a str,
        local: &'a str,
        drop_attrs: bool,
    },
    Picture {
        node: NodeRef<'a, 'src>,
        reference: [&'a str; 2],
    },
    PictureChild {
        reference: [&'a str; 2],
        transparent: bool,
    },
}
#[derive(Clone, Copy)]
pub(in crate::templates) struct Attribute<'a> {
    pub ns: &'a str,
    pub local: &'a str,
    pub value: &'a str,
}
impl<'a, 'src> View<'a, 'src> {
    pub(in crate::templates) fn ns(self) -> &'a str {
        match self {
            Self::Source(node) => node.ns().unwrap_or(""),
            Self::Rename { ns, .. } => ns,
            Self::Picture { node, .. } => node.ns().unwrap_or(""),
            Self::PictureChild { .. } => XR,
        }
    }
    pub(in crate::templates) fn local(self) -> &'a str {
        match self {
            Self::Source(node) | Self::Picture { node, .. } => node.local(),
            Self::Rename { local, .. } => local,
            Self::PictureChild {
                transparent: true, ..
            } => "LoadTransparent",
            Self::PictureChild {
                transparent: false, ..
            } => "Ref",
        }
    }
    pub(in crate::templates) fn text(self) -> Option<Value<'a>> {
        match self {
            Self::Source(node) | Self::Rename { node, .. } => node.text().map(Value::Plain),
            Self::Picture { .. } => None,
            Self::PictureChild {
                transparent: true, ..
            } => Some(Value::Plain("true")),
            Self::PictureChild {
                reference,
                transparent: false,
            } => Some(Value::Fragments(reference)),
        }
    }
    pub(in crate::templates) fn parent(self) -> Option<Self> {
        match self {
            Self::Source(node) | Self::Rename { node, .. } => node.parent().map(Self::Source),
            _ => None,
        }
    }
    pub(in crate::templates) fn attrs(self) -> impl Iterator<Item = Attribute<'a>> {
        let source = match self {
            Self::Source(node)
            | Self::Rename {
                node,
                drop_attrs: false,
                ..
            } => Some(node),
            _ => None,
        };
        source
            .into_iter()
            .flat_map(|node| node.attrs())
            .map(|attr| Attribute {
                ns: attr.ns().unwrap_or(""),
                local: attr.local(),
                value: attr.value(),
            })
            .chain(matches!(self, Self::Picture { .. }).then_some(Attribute {
                ns: XSI,
                local: "type",
                value: "d1p1:Picture",
            }))
    }
    pub(in crate::templates) fn decls(self) -> impl Iterator<Item = (Option<&'a str>, &'a str)> {
        let source = match self {
            Self::Source(node) | Self::Rename { node, .. } => Some(node),
            _ => None,
        };
        let picture = if let Self::Picture { node, .. } = self {
            [
                Some((Some("xsi"), XSI)),
                Some((Some("d1p1"), XR)),
                node.ns().map(|ns| (Some("ns0"), ns)),
            ]
        } else {
            [None; 3]
        };
        source
            .into_iter()
            .flat_map(|node| node.ns_decls())
            .chain(picture.into_iter().flatten())
    }
    pub(in crate::templates) fn children(self) -> impl Iterator<Item = Self> {
        let source = match self {
            Self::Source(node) | Self::Rename { node, .. } => Some(node),
            _ => None,
        };
        let picture = if let Self::Picture { reference, .. } = self {
            [
                Some(Self::PictureChild {
                    reference,
                    transparent: false,
                }),
                Some(Self::PictureChild {
                    reference,
                    transparent: true,
                }),
            ]
        } else {
            [None; 2]
        };
        source
            .into_iter()
            .flat_map(|node| node.children())
            .map(Self::Source)
            .chain(picture.into_iter().flatten())
    }
    pub(in crate::templates) fn attr(self, ns: &str, local: &str) -> Option<&'a str> {
        self.attrs()
            .find(|attr| attr.ns == ns && attr.local == local)
            .map(|attr| attr.value)
    }
    pub(in crate::templates) fn lookup(self, prefix: Option<&str>) -> Option<&'a str> {
        let mut current = Some(self);
        while let Some(node) = current {
            if let Some((_, uri)) = node.decls().find(|(p, _)| *p == prefix) {
                return Some(uri);
            }
            current = node.parent();
        }
        None
    }
    pub(in crate::templates) fn resolve(self, qname: &'a str) -> Option<(&'a str, &'a str)> {
        match qname.split_once(':') {
            Some((prefix, local)) => Some((self.lookup(Some(prefix))?, local)),
            None => Some((self.lookup(None).unwrap_or(""), qname)),
        }
    }
}
