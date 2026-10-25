use crate::{Configuration, MdKind, MdObject, Recalculation, SubKind, SubObject};
use brace::Guid;

pub fn guid(n: u32) -> Guid {
    Guid::parse(&format!("{n:08x}-0000-0000-0000-000000000000")).unwrap_or_default()
}

pub fn obj(kind: MdKind, name: &str) -> MdObject {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(1000);
    MdObject {
        kind,
        id: guid(NEXT.fetch_add(1, Ordering::Relaxed)),
        name: name.into(),
        synonym: Default::default(),
        comment: String::new(),
        generated_types: vec![],
        this_node: None,
        props: vec![],
        children: vec![],
        forms: vec![],
        templates: vec![],
        recalculations: vec![],
        subsystems: vec![],
        modules: vec![],
        parts: Default::default(),
        form: None,
        template: None,
        flowchart: None,
    }
}

pub fn sub(kind: SubKind, name: &str) -> SubObject {
    SubObject {
        kind,
        id: guid(1),
        name: name.into(),
        props: vec![],
        module: None,
        form: None,
        template: None,
    }
}

pub fn recalc(id: Guid, name: &str) -> Recalculation {
    Recalculation {
        id,
        name: name.into(),
        synonym: Default::default(),
        comment: String::new(),
        generated_types: vec![],
        props: vec![],
        dimensions: vec![],
        module: None,
    }
}

pub fn cfg_with(objects: Vec<MdObject>) -> Configuration {
    Configuration {
        objects,
        ..Default::default()
    }
}
