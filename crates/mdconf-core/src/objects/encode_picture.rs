use crate::index::Index;
use crate::model::{Value, XmlElem};
use brace::{Guid, Node};
use mdschema::Ui;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PictureEncodeError {
    WrongValue,
    InvalidShape,
    UnknownPicture,
    AmbiguousPicture,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Candidate {
    Numeric(i64),
    Guid(Guid),
}

impl Candidate {
    fn node(self) -> Node {
        Node::list(match self {
            Self::Numeric(n) => vec![Node::int(n)],
            Self::Guid(id) => vec![Node::int(0), Node::guid(id)],
        })
    }
}

fn select(found: &mut Option<Candidate>, next: Candidate) -> Result<(), PictureEncodeError> {
    if found.is_some_and(|previous| previous != next) {
        return Err(PictureEncodeError::AmbiguousPicture);
    }
    *found = Some(next);
    Ok(())
}

fn resolve(ui: &Ui, index: &Index, name: &str) -> Result<Candidate, PictureEncodeError> {
    let mut found = None;
    if let Some(standard) = name.strip_prefix("StdPicture.").filter(|s| !s.is_empty()) {
        for (key, value) in &ui.std_pictures {
            if value != standard {
                continue;
            }
            if let Ok(number) = key.parse::<i64>()
                && number.to_string() == *key
            {
                select(&mut found, Candidate::Numeric(number))?;
            } else if let Some(id) = Guid::parse(key)
                && id.to_string() == *key
                && index.names.get(&id).is_none_or(|winner| winner == name)
            {
                select(&mut found, Candidate::Guid(id))?;
            }
        }
    }
    for (id, value) in &index.names {
        if value == name {
            select(&mut found, Candidate::Guid(*id))?;
        }
    }
    found.ok_or(PictureEncodeError::UnknownPicture)
}

fn leaf<'a>(element: &'a XmlElem, name: &str) -> Result<&'a str, PictureEncodeError> {
    if element.name != name
        || !element.attrs.is_empty()
        || !element.children.is_empty()
        || element.expanded
    {
        return Err(PictureEncodeError::InvalidShape);
    }
    element
        .text
        .as_deref()
        .ok_or(PictureEncodeError::InvalidShape)
}

pub(crate) fn encode_picture(
    ui: &Ui,
    index: &Index,
    tag: &str,
    value: &Value,
) -> Result<Node, PictureEncodeError> {
    let Value::Xml(root) = value else {
        return Err(PictureEncodeError::WrongValue);
    };
    if root.name != tag || !root.attrs.is_empty() || root.text.is_some() || root.expanded {
        return Err(PictureEncodeError::InvalidShape);
    }
    let (candidate, transparent) = match root.children.as_slice() {
        [] => (None, true),
        [reference, transparent] => {
            let reference = leaf(reference, "xr:Ref")?;
            if reference.is_empty() {
                return Err(PictureEncodeError::InvalidShape);
            }
            let transparent = match leaf(transparent, "xr:LoadTransparent")? {
                "true" => true,
                "false" => false,
                _ => return Err(PictureEncodeError::InvalidShape),
            };
            (Some(resolve(ui, index, reference)?), transparent)
        }
        _ => return Err(PictureEncodeError::InvalidShape),
    };
    Ok(Node::list(vec![
        Node::int(4),
        Node::int(i64::from(candidate.is_some())),
        candidate.map_or_else(|| Node::list(vec![Node::int(0)]), Candidate::node),
        Node::str(""),
        Node::int(-1),
        Node::int(-1),
        Node::int(i64::from(transparent)),
        Node::int(0),
        Node::str(""),
    ]))
}
