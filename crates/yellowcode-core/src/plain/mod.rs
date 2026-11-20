pub mod bak;
pub mod convert;
pub mod export;
mod extension_info;
pub mod extension_intercepts;
mod extension_modules;
mod extension_objects;
pub mod grep;
pub mod metadata;
pub mod module;
pub mod modules;
pub mod scope;
pub mod template;

use mdconf::{LoadOptions, LoadProfile, MdKind, Stages};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reads {
    Tree {
        depth: Option<usize>,
        rooted: bool,
    },
    Show,
    Modules,
    Grep {
        code: bool,
        queries: bool,
        names: bool,
    },
    Templates,
}

pub(crate) fn load_options(reads: Reads) -> LoadOptions {
    let stages = match reads {
        Reads::Tree {
            depth: Some(0 | 1),
            rooted: false,
        } => Stages::NONE,
        Reads::Tree { .. } | Reads::Show | Reads::Modules => Stages::MODULES,
        Reads::Grep {
            code,
            queries,
            names: _,
        } => {
            let mut stages = Stages::NONE;
            if code {
                stages = stages.union(Stages::MODULES);
            }
            if queries {
                stages = stages.union(Stages::TEMPLATES);
            }
            stages
        }
        Reads::Templates => Stages::TEMPLATES,
    };
    LoadOptions {
        keep_source: false,
        profile: LoadProfile::Query,
        extra_stages: stages,
        ..LoadOptions::default()
    }
}

pub(crate) fn objects_of(paths: &[&str], every_kind: bool) -> Option<mdconf::ObjectFilter> {
    use crate::viewer::names::{fold_path_name, kind_from_name};
    let mut named: Vec<(MdKind, String)> = Vec::new();
    for path in paths {
        let mut segments = path.split('.').map(str::trim);
        let kind = kind_from_name(segments.next()?)?;
        if kind == MdKind::Configuration {
            continue;
        }
        named.push((
            kind,
            fold_path_name(segments.next().filter(|s| !s.is_empty())?),
        ));
    }
    let mut kinds: Vec<MdKind> = vec![MdKind::Language];
    for (kind, _) in &named {
        if !kinds.contains(kind) {
            kinds.push(*kind);
        }
    }
    Some(mdconf::ObjectFilter {
        index_kinds: (!every_kind).then_some(kinds),
        keep: std::sync::Arc::new(move |kind, name| {
            let name = fold_path_name(name);
            named.iter().any(|(k, n)| *k == kind && *n == name)
        }),
    })
}

pub(crate) fn load_objects(
    paths: &[&str],
    every_kind: bool,
    options: &LoadOptions,
    found: &dyn Fn(&mdconf::Configuration) -> bool,
    open: &mut dyn FnMut(&LoadOptions) -> crate::Result<mdconf::Configuration>,
) -> crate::Result<mdconf::Configuration> {
    if let Some(filter) = objects_of(paths, every_kind) {
        let cfg = open(&LoadOptions {
            objects: Some(filter),
            ..options.clone()
        })?;
        if found(&cfg) {
            return Ok(cfg);
        }
    }
    open(options)
}
