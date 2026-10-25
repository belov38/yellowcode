use crate::{
    Result,
    viewer::{
        self, Lang,
        path::{self, Target},
        tree::{self, TreeNode},
    },
};
use mdconf::{Configuration, LoadOptions};
use serde::Serialize;
use serde_json::json;
use std::{io::Write, path::PathBuf};

#[derive(Clone, Copy)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
pub enum Format {
    Json,
    Text,
}

pub enum Command {
    Tree {
        source: PathBuf,
        depth: Option<usize>,
        path: Option<String>,
        kind: Option<String>,
        flat: bool,
        format: Format,
    },
    Show {
        source: PathBuf,
        object: String,
        format: Format,
    },
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Node {
    id: String,
    kind: String,
    node_type: String,
    path: Option<String>,
    uuid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    module: Option<String>,
    label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    type_text: Option<String>,
    has_children: bool,
    children_truncated: bool,
    children: Vec<Node>,
}

fn uuid(target: &Target<'_>) -> Option<String> {
    Some(
        match target {
            Target::Configuration(c) => c.id,
            Target::Object(o) => o.id,
            Target::Child { chain, .. } => chain.last()?.id,
            Target::Sub { sub, .. } => sub.id,
            Target::Recalc { recalc, .. } => recalc.id,
        }
        .to_string(),
    )
}

fn project(cfg: &Configuration, node: &TreeNode, parent: &str) -> Node {
    let icon = node.icon.as_deref().unwrap_or("group:Unknown");
    let (node_type, kind) = match icon.split_once(':') {
        Some((typ, kind)) => (typ, kind),
        None if icon == "root" => ("configuration", "Configuration"),
        None => ("object", "Recalculation"),
    };
    let id = match (&node.path, &node.module) {
        (Some(path), None) => path.clone(),
        (Some(path), Some(module)) => format!("{path}::module:{module}"),
        _ => format!("{parent}::{icon}"),
    };
    let uuid = if node.module.is_none() {
        node.path
            .as_deref()
            .and_then(|p| path::resolve(cfg, p).ok())
            .and_then(|t| uuid(&t))
    } else {
        None
    };
    Node {
        children: node.children.iter().map(|c| project(cfg, c, &id)).collect(),
        id,
        kind: kind.into(),
        node_type: node_type.into(),
        path: node.path.clone(),
        uuid,
        module: node.module.clone(),
        label: node.label.clone(),
        type_text: node.type_text.clone(),
        has_children: !node.children.is_empty(),
        children_truncated: false,
    }
}

fn find<'a>(root: &'a Node, uuid: &str) -> Option<&'a Node> {
    if root.uuid.as_deref() == Some(uuid) {
        return Some(root);
    }
    root.children.iter().find_map(|c| find(c, uuid))
}

fn prune(node: &mut Node, depth: usize) {
    if depth == 0 {
        node.children_truncated = node.has_children;
        node.children.clear();
    } else {
        for child in &mut node.children {
            prune(child, depth - 1);
        }
        node.children_truncated = node.children.iter().any(|c| c.children_truncated);
    }
}

fn flatten(
    node: &Node,
    parent: Option<&str>,
    depth: usize,
    rows: &mut Vec<serde_json::Value>,
) -> Result<()> {
    let mut value = json!({
        "id": node.id, "kind": node.kind, "nodeType": node.node_type,
        "path": node.path, "uuid": node.uuid, "label": node.label,
        "hasChildren": node.has_children, "childrenTruncated": node.children_truncated,
    });
    let object = value
        .as_object_mut()
        .ok_or("metadata node is not an object")?;
    if let Some(module) = &node.module {
        object.insert("module".into(), json!(module));
    }
    if let Some(ty) = &node.type_text {
        object.insert("typeText".into(), json!(ty));
    }
    object.insert("parentId".into(), json!(parent));
    object.insert("depth".into(), json!(depth));
    rows.push(value);
    for child in &node.children {
        flatten(child, Some(&node.id), depth + 1, rows)?;
    }
    Ok(())
}

fn text(node: &Node, depth: usize, out: &mut dyn Write) -> Result<()> {
    write!(out, "{}{}", "  ".repeat(depth), node.label)?;
    if let Some(ty) = &node.type_text {
        write!(out, ": {ty}")?;
    }
    if node.children_truncated {
        write!(out, " [children truncated]")?;
    }
    writeln!(out)?;
    for child in &node.children {
        text(child, depth + 1, out)?;
    }
    Ok(())
}

pub fn run(command: Command, out: &mut dyn Write) -> Result<()> {
    match command {
        Command::Tree {
            source,
            depth,
            path,
            kind,
            flat,
            format,
        } => {
            let cfg = viewer::source::open(&source, &LoadOptions::default())?;
            let kinds = kind
                .as_deref()
                .map(|kinds| {
                    kinds
                        .split(',')
                        .map(|kind| {
                            viewer::names::kind_from_name(kind.trim())
                                .ok_or_else(|| format!("unknown metadata kind: {kind}"))
                        })
                        .collect::<std::result::Result<Vec<_>, _>>()
                })
                .transpose()?
                .unwrap_or_default();
            let mut root = project(&cfg, &tree::tree(&cfg, None, &kinds, Lang::Ru), "");
            if let Some(path) = path {
                let target = path::resolve(&cfg, &path)?;
                let id = uuid(&target).ok_or("metadata target has no UUID")?;
                let selected = find(&root, &id).ok_or("metadata path excluded by kind filter")?;
                root = selected.clone();
            }
            if let Some(depth) = depth {
                prune(&mut root, depth);
            }
            match format {
                Format::Text => text(&root, 0, out)?,
                Format::Json => {
                    let value = if flat {
                        let mut rows = Vec::new();
                        flatten(&root, None, 0, &mut rows)?;
                        json!({"schemaVersion": 1, "nodes": rows})
                    } else {
                        json!({"schemaVersion": 1, "root": root})
                    };
                    serde_json::to_writer_pretty(&mut *out, &value)?;
                    writeln!(out)?;
                }
            }
        }
        Command::Show {
            source,
            object,
            format,
        } => {
            let cfg = viewer::source::open(&source, &LoadOptions::default())?;
            let target = path::resolve(&cfg, &object)?;
            let root = project(&cfg, &tree::tree(&cfg, None, &[], Lang::Ru), "");
            let id = uuid(&target).ok_or("metadata target has no UUID")?;
            let selected = find(&root, &id).ok_or("metadata path absent from viewer tree")?;
            let props = viewer::show::show(&cfg, &target, Lang::Ru);
            match format {
                Format::Json => {
                    serde_json::to_writer_pretty(
                        &mut *out,
                        &json!({"schemaVersion": 1, "object": selected, "properties": props}),
                    )?;
                    writeln!(out)?;
                }
                Format::Text => {
                    writeln!(
                        out,
                        "{}",
                        selected.path.as_deref().unwrap_or(&selected.label)
                    )?;
                    for prop in props {
                        writeln!(out, "{}: {}", prop.title, prop.text)?;
                    }
                }
            }
        }
    }
    Ok(())
}
