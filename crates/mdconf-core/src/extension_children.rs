use crate::extension_header::{ExtensionHeader, HeaderError, WireInteger};
use brace::{Guid, Node};
use mdschema::{StaticValue, static_schema};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildKind {
    Attribute,
    TabularSection,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ChildLocation {
    pub parent: Option<(usize, usize)>,
    pub collection: Option<usize>,
    pub child: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildErrorKind {
    UnsupportedKind,
    UnsupportedCollection,
    NotTabularSection,
    Shape,
    UnsupportedVersion,
    Count,
    Class,
    HeaderPath,
    Header(HeaderError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("extension children {location:?}: {kind:?}")]
pub struct ChildError {
    pub location: ChildLocation,
    pub kind: ChildErrorKind,
}
impl ChildLocation {
    fn error(self, kind: ChildErrorKind) -> ChildError {
        ChildError {
            location: self,
            kind,
        }
    }
}

type Schema = &'static StaticValue;

pub struct ExtensionChildCollections<'a> {
    groups: std::iter::Enumerate<std::slice::Iter<'a, Node>>,
    schema: Schema,
    location: ChildLocation,
    terminal: bool,
}
impl<'a> ExtensionChildCollections<'a> {
    pub(crate) fn from_object(node: &'a Node, kind: Option<&str>) -> Result<Self, ChildError> {
        let location = ChildLocation::default();
        let version = match kind {
            Some("Catalog") => "57",
            Some("Document") => "40",
            _ => return Err(location.error(ChildErrorKind::UnsupportedKind)),
        };
        let schema = kind
            .and_then(|k| static_schema().kind(k))
            .ok_or(location.error(ChildErrorKind::UnsupportedKind))?
            .raw();
        let Some([one, body, count, groups @ ..]) = node.as_list() else {
            return Err(location.error(ChildErrorKind::Shape));
        };
        check_version(one, "1", location)?;
        check_version(at(body, 0, location)?, version, location)?;
        check_count(count, groups.len(), location)?;
        Ok(Self {
            groups: groups.iter().enumerate(),
            schema,
            location,
            terminal: false,
        })
    }
}
impl<'a> Iterator for ExtensionChildCollections<'a> {
    type Item = Result<ExtensionChildCollection<'a>, ChildError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.terminal {
            return None;
        }
        let Some((index, node)) = self.groups.next() else {
            self.terminal = true;
            return None;
        };
        let location = ChildLocation {
            collection: Some(index),
            ..self.location
        };
        let result = group(node, self.schema, location, index);
        if result.is_err() {
            self.terminal = true;
        }
        Some(result)
    }
}
impl std::iter::FusedIterator for ExtensionChildCollections<'_> {}

pub struct ExtensionChildCollection<'a> {
    pub index: usize,
    pub class_id: Guid,
    pub raw_class: &'a str,
    pub tag: Option<&'static str>,
    source: &'a Node,
    items: &'a [Node],
    schema: Option<Schema>,
    location: ChildLocation,
}
impl<'a> ExtensionChildCollection<'a> {
    pub fn source(&self) -> &'a Node {
        self.source
    }
    pub fn len(&self) -> usize {
        self.items.len()
    }
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn children(&self) -> Result<ExtensionChildren<'a>, ChildError> {
        let kind = match self
            .schema
            .and_then(|s| s.get("tag"))
            .and_then(StaticValue::as_str)
        {
            Some("Attribute") => ChildKind::Attribute,
            Some("TabularSection") if self.location.parent.is_none() => ChildKind::TabularSection,
            _ => return Err(self.location.error(ChildErrorKind::UnsupportedCollection)),
        };
        let schema = self
            .schema
            .ok_or(self.location.error(ChildErrorKind::UnsupportedCollection))?;
        Ok(ExtensionChildren {
            items: self.items.iter().enumerate(),
            schema,
            kind,
            location: self.location,
        })
    }
}

fn group(
    node: &Node,
    schema: Schema,
    location: ChildLocation,
    index: usize,
) -> Result<ExtensionChildCollection<'_>, ChildError> {
    let Some([class, count, items @ ..]) = node.as_list() else {
        return Err(location.error(ChildErrorKind::Shape));
    };
    let class_id = class
        .as_guid()
        .ok_or(location.error(ChildErrorKind::Class))?;
    let raw_class = class
        .as_atom()
        .ok_or(location.error(ChildErrorKind::Class))?;
    check_count(count, items.len(), location)?;
    let child_schema = schema
        .get("children")
        .and_then(StaticValue::as_object)
        .and_then(|pairs| {
            pairs
                .iter()
                .find(|(id, _)| Guid::parse(id) == Some(class_id))
        })
        .map(|(_, spec)| spec);
    let tag = child_schema
        .and_then(|s| s.get("tag"))
        .and_then(StaticValue::as_str)
        .or_else(|| {
            schema
                .get("subobjects")
                .and_then(StaticValue::as_object)
                .and_then(|pairs| {
                    pairs
                        .iter()
                        .find(|(_, id)| id.as_str().and_then(Guid::parse) == Some(class_id))
                })
                .map(|(tag, _)| *tag)
        });
    Ok(ExtensionChildCollection {
        index,
        class_id,
        raw_class,
        tag,
        source: node,
        items,
        schema: child_schema,
        location,
    })
}

pub struct ExtensionChildren<'a> {
    items: std::iter::Enumerate<std::slice::Iter<'a, Node>>,
    schema: Schema,
    kind: ChildKind,
    location: ChildLocation,
}
impl<'a> Iterator for ExtensionChildren<'a> {
    type Item = Result<ExtensionChild<'a>, ChildError>;
    fn next(&mut self) -> Option<Self::Item> {
        self.items.next().map(|(index, source)| {
            let location = ChildLocation {
                child: Some(index),
                ..self.location
            };
            let child = ExtensionChild {
                index,
                kind: self.kind,
                source,
                schema: self.schema,
                location,
            };
            child.validate()?;
            child.header()?;
            Ok(child)
        })
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.items.size_hint()
    }
}
impl ExactSizeIterator for ExtensionChildren<'_> {}
impl std::iter::FusedIterator for ExtensionChildren<'_> {}

pub struct ExtensionChild<'a> {
    pub index: usize,
    kind: ChildKind,
    source: &'a Node,
    schema: Schema,
    location: ChildLocation,
}
impl<'a> ExtensionChild<'a> {
    pub fn kind(&self) -> ChildKind {
        self.kind
    }
    pub fn source(&self) -> &'a Node {
        self.source
    }
    pub fn header(&self) -> Result<ExtensionHeader<'a>, ChildError> {
        let node = self
            .schema
            .get("header")
            .and_then(StaticValue::as_array)
            .and_then(|path| {
                path.iter()
                    .try_fold(self.source, |n, i| n.at(usize::try_from(i.as_u64()?).ok()?))
            })
            .ok_or(self.location.error(ChildErrorKind::HeaderPath))?;
        ExtensionHeader::from_node(node).map_err(|e| self.location.error(ChildErrorKind::Header(e)))
    }
    pub fn child_collections(&self) -> Result<ExtensionChildCollections<'a>, ChildError> {
        if self.kind != ChildKind::TabularSection {
            return Err(self.location.error(ChildErrorKind::NotTabularSection));
        }
        let Some([_body, count, groups @ ..]) = self.source.as_list() else {
            return Err(self.location.error(ChildErrorKind::Shape));
        };
        check_count(count, groups.len(), self.location)?;
        Ok(ExtensionChildCollections {
            groups: groups.iter().enumerate(),
            schema: self.schema,
            location: ChildLocation {
                parent: self.location.collection.zip(self.location.child),
                collection: None,
                child: None,
            },
            terminal: false,
        })
    }
    fn validate(&self) -> Result<(), ChildError> {
        let loc = self.location;
        match self.kind {
            ChildKind::Attribute => {
                let Some([body, trailer]) = self.source.as_list() else {
                    return Err(loc.error(ChildErrorKind::Shape));
                };
                if trailer.as_atom() != Some("0") {
                    return Err(loc.error(ChildErrorKind::Shape));
                }
                check_version(
                    at(body, 0, loc)?,
                    if loc.parent.is_some() { "8" } else { "6" },
                    loc,
                )?;
                let field = at(body, 1, loc)?;
                check_version(at(field, 0, loc)?, "27", loc)?;
                let Some([version, _header, _opaque_type]) = at(field, 1, loc)?.as_list() else {
                    return Err(loc.error(ChildErrorKind::Shape));
                };
                check_version(version, "2", loc)?;
            }
            ChildKind::TabularSection => {
                let Some([body, _count, _groups @ ..]) = self.source.as_list() else {
                    return Err(loc.error(ChildErrorKind::Shape));
                };
                let Some([version, inner, _opaque, _tail]) = body.as_list() else {
                    return Err(loc.error(ChildErrorKind::Shape));
                };
                check_version(version, "2", loc)?;
                check_version(at(inner, 0, loc)?, "11", loc)?;
                let Some([zero, _header]) = at(inner, 5, loc)?.as_list() else {
                    return Err(loc.error(ChildErrorKind::Shape));
                };
                if zero.as_atom() != Some("0") {
                    return Err(loc.error(ChildErrorKind::Shape));
                }
            }
        }
        Ok(())
    }
}
fn at(node: &Node, index: usize, location: ChildLocation) -> Result<&Node, ChildError> {
    node.at(index).ok_or(location.error(ChildErrorKind::Shape))
}
fn check_version(node: &Node, expected: &str, location: ChildLocation) -> Result<(), ChildError> {
    let value = WireInteger::from_node(node).ok_or(location.error(ChildErrorKind::Shape))?;
    if value.raw() != expected {
        return Err(location.error(ChildErrorKind::UnsupportedVersion));
    }
    Ok(())
}
fn check_count(node: &Node, len: usize, location: ChildLocation) -> Result<(), ChildError> {
    let count = node
        .as_atom()
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|s| s.parse::<usize>().ok());
    if count != Some(len) {
        return Err(location.error(ChildErrorKind::Count));
    }
    Ok(())
}

pub fn decode_attribute(
    main: &crate::Configuration,
    owner: &crate::model::MdObject,
    class: Guid,
    nested: bool,
    item: &Node,
) -> Option<crate::model::Child> {
    crate::objects::decode::decode_extension_child(main, owner, class, nested, item)
        .filter(|child| child.tag == crate::model::ChildTag::Attribute)
}
