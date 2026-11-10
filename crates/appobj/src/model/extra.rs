use super::types::TypeSet;
use super::{ObjId, Uuid16};
use bsl_compiler::CommonFlags;
use md_model::{Configuration, LocalString, MdKind, MdObject};

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

#[derive(Clone, Debug)]
pub struct SubsystemDef {
    pub top: usize,
    pub route: Vec<usize>,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
}

pub(super) fn subsystem_tree(cfg: &Configuration) -> Vec<SubsystemDef> {
    let mut defs: Vec<SubsystemDef> = Vec::new();
    let mut stack: Vec<(&MdObject, usize, Vec<usize>, Option<usize>)> = cfg
        .objects
        .iter()
        .enumerate()
        .filter(|(_, o)| o.kind == MdKind::Subsystem)
        .map(|(top, o)| (o, top, Vec::new(), None))
        .collect();
    stack.reverse();
    while let Some((object, top, route, parent)) = stack.pop() {
        let me = defs.len();
        if let Some(p) = parent.and_then(|p| defs.get_mut(p)) {
            p.children.push(me);
        }
        for (i, nested) in object.subsystems.iter().enumerate().rev() {
            let mut path = route.clone();
            path.push(i);
            stack.push((nested, top, path, Some(me)));
        }
        defs.push(SubsystemDef {
            top,
            route,
            parent,
            children: Vec::new(),
        });
    }
    defs
}

pub(super) fn subsystem_object<'a>(
    cfg: &'a Configuration,
    def: &SubsystemDef,
) -> Option<&'a MdObject> {
    let mut object = cfg.objects.get(def.top)?;
    for &i in &def.route {
        object = object.subsystems.get(i)?;
    }
    Some(object)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MdPath {
    Object(ObjId),
    Field(ObjId, Option<usize>, usize),
    Tabular(ObjId, usize),
    EnumValue(ObjId, usize),
    Form(ObjId, usize),
    Command(ObjId, usize),
    Template(ObjId, usize),
    HttpUrlTemplate(usize, usize),
    HttpMethod(usize, usize, usize),
    WebOperation(usize, usize),
    WebParameter(usize, usize, usize),
    CommonModule(usize),
    SessionParam(usize),
    Subscription(usize),
    Subsystem(usize),
    Other(usize),
    Configuration,
}
