use crate::{MdKind, MdObject};
use brace::{Guid, Node};
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Right(#[doc(hidden)] pub u16);

fn table() -> &'static [(Guid, &'static str)] {
    static T: OnceLock<Vec<(Guid, &'static str)>> = OnceLock::new();
    T.get_or_init(|| {
        mdschema::rights::rights_tables()
            .map(|t| {
                t.rights
                    .iter()
                    .map(|(g, n)| (Guid::parse(g).unwrap_or(Guid::NIL), n.as_str()))
                    .collect()
            })
            .unwrap_or_default()
    })
}

impl Right {
    #[doc(hidden)]
    pub fn from_static_guid(guid: Guid) -> Option<Right> {
        mdschema::static_rights_tables()
            .rights()
            .enumerate()
            .find(|(_, (raw, _))| Guid::parse(raw) == Some(guid))
            .and_then(|(ordinal, _)| Self::at(ordinal))
    }

    pub fn all_static() -> impl Iterator<Item = Right> {
        (0..mdschema::static_rights_tables().rights().count()).filter_map(Self::at)
    }

    pub fn static_name(self) -> &'static str {
        mdschema::static_rights_tables()
            .rights()
            .nth(usize::from(self.0))
            .map_or("", |(_, name)| name)
    }

    pub fn is_applicable(self, kind: MdKind) -> bool {
        let wanted = self.static_name();
        mdschema::static_rights_tables()
            .rights_by_kind()
            .find(|(name, _)| *name == kind.as_str())
            .is_some_and(|(_, rights)| rights.iter().any(|name| name == wanted))
    }

    pub fn all() -> impl Iterator<Item = Right> {
        (0..u16::try_from(table().len()).unwrap_or(u16::MAX)).map(Right)
    }

    #[doc(hidden)]
    pub fn at(i: usize) -> Option<Right> {
        u16::try_from(i).ok().map(Right)
    }

    pub fn from_name(name: &str) -> Option<Right> {
        Right::at(table().iter().position(|e| e.1 == name)?)
    }

    pub fn from_guid(g: Guid) -> Option<Right> {
        Right::at(table().iter().position(|e| e.0 == g)?)
    }

    pub fn name(self) -> &'static str {
        table().get(usize::from(self.0)).map_or("", |e| e.1)
    }

    pub fn guid(self) -> Guid {
        table().get(usize::from(self.0)).map_or(Guid::NIL, |e| e.0)
    }

    pub fn applicable(kind: MdKind) -> Vec<Right> {
        let Ok(t) = mdschema::rights::rights_tables() else {
            return Vec::new();
        };
        t.rights_by_kind
            .iter()
            .find(|(k, _)| k == kind.as_str())
            .map(|(_, names)| names.iter().filter_map(|n| Right::from_name(n)).collect())
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RightTarget {
    Configuration,
    Object(Guid),
    Child { id: Guid, field: bool },
    StandardAttribute { owner: Guid, id: i32 },
}

impl RightTarget {
    pub fn is_field(&self) -> bool {
        matches!(
            self,
            RightTarget::Child { field: true, .. } | RightTarget::StandardAttribute { .. }
        )
    }

    #[doc(hidden)]
    pub fn sort_key(&self, root: Guid) -> (Guid, u8, i32) {
        match *self {
            RightTarget::Configuration => (root, 0, 0),
            RightTarget::Object(id) | RightTarget::Child { id, .. } => (id, 0, 0),
            RightTarget::StandardAttribute { owner, id } => (owner, 1, id),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Restriction {
    pub condition: String,
    pub fields: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RightValue {
    pub right: Right,
    pub value: bool,
    pub restrictions: Vec<Restriction>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ObjectRights {
    pub target: RightTarget,
    pub full_name: String,
    pub rights: Vec<RightValue>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Rights {
    pub set_for_new_objects: bool,
    pub set_for_attributes_by_default: bool,
    pub independent_rights_of_child_objects: bool,
    pub objects: Vec<ObjectRights>,
    pub templates: Vec<(String, String)>,
    #[doc(hidden)]
    pub source: Node,
}

impl Rights {
    pub fn default_for(&self, target: &RightTarget) -> bool {
        if target.is_field() {
            self.set_for_attributes_by_default
        } else {
            self.set_for_new_objects
        }
    }

    #[doc(hidden)]
    pub fn stored(&self, target: &RightTarget, right: Right) -> Option<&RightValue> {
        self.objects
            .iter()
            .filter(|o| o.target == *target)
            .flat_map(|o| &o.rights)
            .find(|rv| rv.right == right)
    }

    pub fn value(&self, target: &RightTarget, right: Right) -> bool {
        self.stored(target, right)
            .map_or_else(|| self.default_for(target), |rv| rv.value)
    }

    pub fn restrictions(&self, target: &RightTarget, right: Right) -> &[Restriction] {
        self.stored(target, right)
            .map_or(&[], |rv| rv.restrictions.as_slice())
    }
}

impl MdObject {
    pub fn rights(&self) -> Option<&Rights> {
        self.parts.rights.as_ref()
    }
}
