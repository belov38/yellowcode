use crate::model::SubKind;
use brace::Guid;
use mdschema::MdKind;
use std::collections::HashMap;

#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeOwner {
    pub object: Guid,
    pub child: Option<Guid>,
    pub category: String,
    pub recalc: Option<Guid>,
}

#[doc(hidden)]
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Index {
    pub cfg_id: Guid,
    pub header_id: Guid,
    pub order: Vec<Guid>,
    pub objs: HashMap<Guid, (MdKind, Option<String>)>,
    pub names: HashMap<Guid, String>,
    pub types: HashMap<Guid, String>,
    pub type_owners: HashMap<Guid, TypeOwner>,
    pub sub_hdr: HashMap<Guid, Vec<usize>>,
    pub subs: HashMap<Guid, Vec<(SubKind, Guid)>>,
    pub recalcs: HashMap<Guid, Vec<Guid>>,
    pub nested: Vec<(Guid, Guid)>,
    pub missing: Vec<Guid>,
    pub too_deep: Vec<Guid>,
    pub skipped: Vec<String>,
}

impl Index {
    #[doc(hidden)]
    pub fn for_tests(names: &[(Guid, &str)], types: &[(Guid, &str)]) -> Index {
        Index {
            names: names.iter().map(|(g, n)| (*g, n.to_string())).collect(),
            types: types.iter().map(|(g, n)| (*g, n.to_string())).collect(),
            ..Default::default()
        }
    }
}
