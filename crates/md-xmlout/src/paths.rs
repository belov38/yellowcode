use md_model::{Configuration, MdKind, MdObject, Recalculation, SubKind, SubObject};

fn parent_chain<'a>(cfg: &'a Configuration, obj: &MdObject) -> Option<Vec<&'a MdObject>> {
    let mut stack: Vec<(&MdObject, Vec<&MdObject>)> =
        cfg.objects.iter().map(|o| (o, Vec::new())).collect();
    while let Some((o, chain)) = stack.pop() {
        if std::ptr::eq(o, obj) || (o.id == obj.id && o.kind == obj.kind && o.name == obj.name) {
            return Some(chain);
        }
        for s in &o.subsystems {
            let mut c = chain.clone();
            c.push(o);
            stack.push((s, c));
        }
    }
    None
}

fn own_dir(obj: &MdObject) -> String {
    match obj.kind.folder() {
        Some(f) => format!("{f}/{}", obj.name),
        None => String::new(),
    }
}

pub fn object_dir(cfg: &Configuration, obj: &MdObject) -> String {
    if obj.kind != MdKind::Subsystem {
        return own_dir(obj);
    }
    let mut s = String::new();
    for p in parent_chain(cfg, obj).unwrap_or_default() {
        s.push_str(&own_dir(p));
        s.push('/');
    }
    s.push_str(&own_dir(obj));
    s
}

pub fn sub_dir(cfg: &Configuration, owner: &MdObject, sub: &SubObject) -> String {
    let folder = match sub.kind {
        SubKind::Form => "Forms",
        SubKind::Template => "Templates",
    };
    format!("{}/{folder}/{}", object_dir(cfg, owner), sub.name)
}

pub fn recalc_dir(cfg: &Configuration, register: &MdObject, r: &Recalculation) -> String {
    format!("{}/Recalculations/{}", object_dir(cfg, register), r.name)
}

pub fn recalc_path(cfg: &Configuration, register: &MdObject, r: &Recalculation) -> String {
    format!("{}.xml", recalc_dir(cfg, register, r))
}
