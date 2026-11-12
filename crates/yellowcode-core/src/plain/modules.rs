use mdconf::{Child, Configuration, MdObject, Module};

use super::scope::Scope;
use crate::viewer::module::{module_of, modules_of};
use crate::viewer::names::{child_tag_en, module_name, sub_kind_en};
use crate::viewer::out::Lang;
use crate::viewer::path::{self, RECALC_EN, Target};

pub struct ModuleRef<'a> {
    pub path: String,
    pub owner: String,
    pub kind: mdconf::ModuleKind,
    pub module: &'a Module,
}

fn refs_of<'a>(owner: &str, mods: impl IntoIterator<Item = &'a Module>) -> Vec<ModuleRef<'a>> {
    mods.into_iter()
        .map(|m| ModuleRef {
            path: format!("{owner}.{}", module_name(m.kind, Lang::En)),
            owner: owner.to_string(),
            kind: m.kind,
            module: m,
        })
        .collect()
}

struct Lister<'a, 's> {
    scope: &'s Scope,
    out: Vec<ModuleRef<'a>>,
}

impl<'a> Lister<'a, '_> {
    fn add(&mut self, owner: &str, mods: impl IntoIterator<Item = &'a Module>) {
        let refs = refs_of(owner, mods);
        if !refs.is_empty() && self.scope.contains(owner) {
            self.out.extend(refs);
        }
    }

    fn child(&mut self, owner: &str, c: &'a Child) {
        let path = format!("{owner}.{}.{}", child_tag_en(c.tag), c.name);
        self.add(&path, &c.module);
        for c2 in &c.children {
            self.child(&path, c2);
        }
    }

    fn object(&mut self, path: &str, o: &'a MdObject) {
        self.add(path, &o.modules);
        for c in &o.children {
            self.child(path, c);
        }
        for s in o.forms.iter().chain(&o.templates) {
            self.add(
                &format!("{path}.{}.{}", sub_kind_en(s.kind), s.name),
                &s.module,
            );
        }
        for r in &o.recalculations {
            self.add(&format!("{path}.{RECALC_EN}.{}", r.name), &r.module);
        }
        for s in &o.subsystems {
            self.object(&format!("{path}.Subsystem.{}", s.name), s);
        }
    }
}

pub fn modules<'a>(cfg: &'a Configuration, scope: &Scope) -> Vec<ModuleRef<'a>> {
    let mut l = Lister {
        scope,
        out: Vec::new(),
    };
    l.add("Configuration", &cfg.modules);
    for o in &cfg.objects {
        l.object(&format!("{}.{}", o.kind.as_str(), o.name), o);
    }
    l.out
}

fn list_paths(refs: &[ModuleRef<'_>]) -> String {
    refs.iter()
        .map(|r| r.path.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn only_module<'a>(cfg: &Configuration, t: &Target<'a>) -> crate::Result<ModuleRef<'a>> {
    let owner = path::canonical(cfg, t);
    let mut refs = refs_of(&owner, modules_of(t));
    match refs.len() {
        1 => Ok(refs.remove(0)),
        0 => Err(format!("{owner} has no modules").into()),
        _ => Err(format!(
            "several modules in {owner}, name one: {}",
            list_paths(&refs)
        )
        .into()),
    }
}

pub fn resolve_module<'a>(cfg: &'a Configuration, path: &str) -> crate::Result<ModuleRef<'a>> {
    let mut prefix_failure = None;
    if let Some((prefix, last)) = path.rsplit_once('.')
        && let Ok(t) = path::resolve(cfg, prefix)
    {
        match module_of(cfg, &t, Some(last)) {
            Ok(module) => {
                let owner = path::canonical(cfg, &t);
                return Ok(ModuleRef {
                    path: format!("{owner}.{}", module_name(module.kind, Lang::En)),
                    owner,
                    kind: module.kind,
                    module,
                });
            }
            Err(_) => {
                let owner = path::canonical(cfg, &t);
                let available = list_paths(&refs_of(&owner, modules_of(&t)));
                prefix_failure = Some(format!(
                    "no module {last} in {owner} (available: {})",
                    if available.is_empty() {
                        "none"
                    } else {
                        &available
                    }
                ));
            }
        }
    }
    match path::resolve(cfg, path) {
        Ok(t) => only_module(cfg, &t),
        Err(e) => match prefix_failure {
            Some(msg) if e.suggestions.is_empty() => Err(msg.into()),
            _ => Err(e.into()),
        },
    }
}
