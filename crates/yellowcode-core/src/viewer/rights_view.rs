use std::collections::HashMap;

use mdconf::parts::{Right, RightTarget, RightValue};
use mdconf::{Configuration, MdKind, MdObject};
use serde::Serialize;

use super::configurator::{right_hint, right_title};
use super::names::kind_name;
use super::out::Lang;
use super::path::{self, Target};

#[derive(Serialize, Debug)]
pub struct RightsTable {
    pub defined: bool,
    pub flags: serde_json::Value,
    pub columns: Vec<String>,
    pub titles: Vec<String>,
    pub hints: Vec<String>,
    pub rows: Vec<RightsRow>,
    pub total: usize,
    pub from: usize,
    pub templates: Vec<(String, String)>,
}

#[derive(Serialize, Debug)]
pub struct RightsRow {
    pub object: String,
    pub kind: String,
    pub values: Vec<Option<bool>>,
    pub restricted: Vec<RightRestriction>,
}

#[derive(Serialize, Debug)]
pub struct RightRestriction {
    pub right: String,
    pub conditions: Vec<String>,
}

pub const MAX_CONDITION: usize = 16 * 1024;

fn cut_condition(s: &str) -> String {
    match s.char_indices().nth(MAX_CONDITION) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

fn all_objects(cfg: &Configuration) -> Vec<(String, &MdObject)> {
    let mut out = Vec::new();
    let mut stack: Vec<(String, &MdObject)> = cfg
        .objects
        .iter()
        .rev()
        .map(|o| (format!("{}.{}", kind_name(o.kind, Lang::En), o.name), o))
        .collect();
    while let Some((p, o)) = stack.pop() {
        for s in o.subsystems.iter().rev() {
            stack.push((format!("{p}.Subsystem.{}", s.name), s));
        }
        out.push((p, o));
    }
    out
}

pub const DEFAULT_LIMIT: usize = 300;
pub const MAX_LIMIT: usize = 2000;

#[derive(Debug, Clone)]
pub struct RightsQuery {
    pub q: String,
    pub only: bool,
    pub from: usize,
    pub limit: usize,
    pub templates: bool,
}

impl Default for RightsQuery {
    fn default() -> Self {
        RightsQuery {
            q: String::new(),
            only: false,
            from: 0,
            limit: DEFAULT_LIMIT,
            templates: false,
        }
    }
}

pub fn rights_table(
    cfg: &Configuration,
    role: &str,
    lang: Lang,
    query: &RightsQuery,
) -> Option<RightsTable> {
    let Target::Object(o) = path::resolve(cfg, role).ok()? else {
        return None;
    };
    if o.kind != MdKind::Role {
        return None;
    }
    let rights = o.rights();
    let mut stored: HashMap<(&RightTarget, Right), &RightValue> = HashMap::new();
    for or in rights.iter().flat_map(|r| &r.objects) {
        for rv in &or.rights {
            stored.entry((&or.target, rv.right)).or_insert(rv);
        }
    }
    let mut targets: Vec<(String, MdKind, RightTarget)> = vec![(
        "Configuration".to_string(),
        MdKind::Configuration,
        RightTarget::Configuration,
    )];
    targets.extend(
        all_objects(cfg)
            .into_iter()
            .map(|(p, o)| (p, o.kind, RightTarget::Object(o.id))),
    );
    let mut by_kind: HashMap<MdKind, Vec<Right>> = HashMap::new();
    let used: Vec<(String, MdKind, RightTarget, Vec<Right>)> = targets
        .into_iter()
        .map(|(p, k, t)| {
            let a = by_kind
                .entry(k)
                .or_insert_with(|| Right::applicable(k))
                .clone();
            (p, k, t, a)
        })
        .filter(|(_, _, _, a)| !a.is_empty())
        .collect();
    let columns: Vec<Right> = Right::all()
        .filter(|r| by_kind.values().any(|a| a.contains(r)))
        .collect();
    let q = query.q.to_lowercase();
    let limit = query.limit.min(MAX_LIMIT);
    let make = |(p, k, t, a): &(String, MdKind, RightTarget, Vec<Right>)| {
        let mut restricted = Vec::new();
        let values = columns
            .iter()
            .map(|r| {
                if !a.contains(r) {
                    return None;
                }
                let rv = stored.get(&(t, *r));
                let conditions: Vec<String> = rv
                    .into_iter()
                    .flat_map(|v| &v.restrictions)
                    .filter(|x| !x.condition.is_empty())
                    .map(|x| cut_condition(&x.condition))
                    .collect();
                if !conditions.is_empty() {
                    restricted.push(RightRestriction {
                        right: r.name().to_string(),
                        conditions,
                    });
                }
                Some(rv.map_or_else(|| rights.is_some_and(|r| r.default_for(t)), |v| v.value))
            })
            .collect();
        RightsRow {
            object: p.clone(),
            kind: kind_name(*k, lang),
            values,
            restricted,
        }
    };
    let wanted: Vec<&(String, MdKind, RightTarget, Vec<Right>)> = if rights.is_none() {
        Vec::new()
    } else {
        used.iter()
            .filter(|u| q.is_empty() || u.0.to_lowercase().contains(&q))
            .collect()
    };
    let (rows, total) = if query.only {
        let granted: Vec<RightsRow> = wanted
            .into_iter()
            .map(make)
            .filter(|r| r.values.contains(&Some(true)))
            .collect();
        let total = granted.len();
        (
            granted.into_iter().skip(query.from).take(limit).collect(),
            total,
        )
    } else {
        let total = wanted.len();
        (
            wanted
                .into_iter()
                .skip(query.from)
                .take(limit)
                .map(make)
                .collect(),
            total,
        )
    };
    Some(RightsTable {
        defined: rights.is_some(),
        flags: serde_json::json!({
            "set_for_new_objects": rights.is_some_and(|r| r.set_for_new_objects),
            "set_for_attributes_by_default": rights.is_some_and(|r| r.set_for_attributes_by_default),
            "independent_rights_of_child_objects":
                rights.is_some_and(|r| r.independent_rights_of_child_objects),
        }),
        columns: columns.iter().map(|r| r.name().to_string()).collect(),
        titles: columns
            .iter()
            .map(|r| right_title(r.name(), lang).unwrap_or(r.name()).to_string())
            .collect(),
        hints: columns
            .iter()
            .map(|r| right_hint(r.name(), lang).unwrap_or_default().to_string())
            .collect(),
        rows,
        total,
        from: query.from,
        templates: rights
            .filter(|_| query.templates)
            .map(|r| r.templates.clone())
            .unwrap_or_default(),
    })
}

mod managed;
pub use managed::{RightsQueryRef, rights_table_borrowed, rights_table_managed};
