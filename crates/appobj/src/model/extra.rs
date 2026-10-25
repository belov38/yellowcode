use super::types::TypeSet;
use super::{ObjId, Uuid16};
use bsl_compiler::CommonFlags;
use md_model::LocalString;

#[derive(Clone, Debug)]
pub struct CommonModuleDef {
    pub md: Uuid16,
    pub name: String,
    pub flags: CommonFlags,
    pub has_text: bool,
}

#[derive(Clone, Debug)]
pub struct SessionParamDef {
    pub md: Uuid16,
    pub name: String,
    pub synonym: LocalString,
    pub types: TypeSet,
}

#[derive(Clone, Debug)]
pub struct SubscriptionDef {
    pub md: Uuid16,
    pub name: String,
    pub source: TypeSet,
    pub event: String,
    pub handler: (String, String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MdPath {
    Object(ObjId),
    Field(ObjId, Option<usize>, usize),
    Tabular(ObjId, usize),
    EnumValue(ObjId, usize),
    Form(ObjId, usize),
    CommonModule(usize),
    SessionParam(usize),
    Subscription(usize),
    Other(usize),
    Configuration,
}
