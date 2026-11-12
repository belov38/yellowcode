use std::collections::HashSet;

use mdconf::{Configuration, MdKind, MdObject, Value};

use crate::viewer::names::{fold_path_name, kind_from_name};
use crate::viewer::path::{self, Target};

#[derive(Debug, Clone, Default)]
pub struct ScopeArgs {
    pub kinds: Vec<String>,
    pub paths: Vec<String>,
    pub subsystems: Vec<String>,
}

#[derive(Debug, Clone)]
struct SubsystemScope {
    branch: Vec<String>,
    roots: HashSet<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Scope {
    kinds: Option<Vec<MdKind>>,
    paths: Option<Vec<Vec<String>>>,
    subsystems: Option<Vec<SubsystemScope>>,
}

fn fold_segments(path: &str) -> Vec<String> {
    path.split('.').map(fold_path_name).collect()
}

fn root_key(kind: &str, name: &str) -> String {
    let kind =
        kind_from_name(kind.trim()).map_or_else(|| kind.to_string(), |k| k.as_str().to_string());
    format!("{}.{}", fold_path_name(&kind), fold_path_name(name))
}

fn full_subsystem_path(arg: &str) -> String {
    let first = arg.split('.').next().unwrap_or_default();
    if kind_from_name(first.trim()).is_some() {
        return arg.to_string();
    }
    let parts: Vec<String> = arg
        .split('.')
        .map(|s| format!("Subsystem.{}", s.trim()))
        .collect();
    parts.join(".")
}

fn collect_content(o: &MdObject, roots: &mut HashSet<String>) {
    if let Some(Value::RefList(refs)) = o.prop("Content") {
        for r in refs {
            if let Some((kind, name)) = r.name.split_once('.')
                && !name.contains('.')
            {
                roots.insert(root_key(kind, name));
            }
        }
    }
    for s in &o.subsystems {
        collect_content(s, roots);
    }
}

fn resolve_subsystem(cfg: &Configuration, arg: &str) -> crate::Result<SubsystemScope> {
    let target = path::resolve(cfg, &full_subsystem_path(arg))?;
    let Target::Object(o) = &target else {
        return Err(format!("not a subsystem: {arg}").into());
    };
    if o.kind != MdKind::Subsystem {
        return Err(format!("not a subsystem: {arg}").into());
    }
    let mut roots = HashSet::new();
    collect_content(o, &mut roots);
    Ok(SubsystemScope {
        branch: fold_segments(&path::canonical(cfg, &target)),
        roots,
    })
}

impl Scope {
    pub fn all() -> Scope {
        Scope::default()
    }

    pub fn resolve(cfg: &Configuration, args: &ScopeArgs) -> crate::Result<Scope> {
        let mut kinds = Vec::new();
        for item in args.kinds.iter().flat_map(|a| a.split(',')) {
            let item = item.trim();
            if item.is_empty() {
                continue;
            }
            kinds.push(
                kind_from_name(item).ok_or_else(|| format!("unknown metadata kind: {item}"))?,
            );
        }
        let mut paths = Vec::new();
        for p in &args.paths {
            let target = path::resolve(cfg, p)?;
            paths.push(fold_segments(&path::canonical(cfg, &target)));
        }
        let mut subsystems = Vec::new();
        for s in &args.subsystems {
            subsystems.push(resolve_subsystem(cfg, s)?);
        }
        Ok(Scope {
            kinds: (!kinds.is_empty()).then_some(kinds),
            paths: (!paths.is_empty()).then_some(paths),
            subsystems: (!subsystems.is_empty()).then_some(subsystems),
        })
    }

    pub fn is_all(&self) -> bool {
        self.kinds.is_none() && self.paths.is_none() && self.subsystems.is_none()
    }

    pub fn contains(&self, path: &str) -> bool {
        let segs = fold_segments(path);
        if let Some(kinds) = &self.kinds {
            let kind = path
                .split('.')
                .next()
                .and_then(|k| kind_from_name(k.trim()));
            if !kind.is_some_and(|k| kinds.contains(&k)) {
                return false;
            }
        }
        if let Some(paths) = &self.paths
            && !paths.iter().any(|p| segs.starts_with(p))
        {
            return false;
        }
        if let Some(subsystems) = &self.subsystems {
            let root = match segs.as_slice() {
                [kind, name, ..] => Some(root_key(kind, name)),
                _ => None,
            };
            let hit = |s: &SubsystemScope| {
                segs.starts_with(&s.branch) || root.as_ref().is_some_and(|r| s.roots.contains(r))
            };
            if !subsystems.iter().any(hit) {
                return false;
            }
        }
        true
    }
}
