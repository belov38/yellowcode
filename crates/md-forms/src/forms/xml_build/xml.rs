use super::{FormBuildError, Result, invalid};
use crate::forms::fe::{E, prefix_of};
use mdconf_core::xml::view::{NodeRef, XmlArena};
use memory_budget::MemoryBudget;
use std::path::Path;

pub(super) const NS: &str = "http://v8.1c.ru/8.3/xcf/logform";
pub(super) fn child<'a>(e: &'a E, tag: &str) -> Option<&'a E> {
    e.kids.iter().find(|c| c.tag == tag)
}
pub(super) fn text(e: &E) -> &str {
    e.text.as_deref().unwrap_or_default()
}
pub(super) fn attr<'a>(e: &'a E, key: &str) -> Option<&'a str> {
    e.attrs
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}
pub(super) fn ctext<'a>(e: &'a E, key: &str, default: &'a str) -> &'a str {
    child(e, key).map_or(default, text)
}
pub(super) fn container(e: &E) -> Result<()> {
    if !e.attrs.is_empty() || !text(e).trim().is_empty() {
        return Err(invalid(&e.tag, "unexpected container attribute or text"));
    }
    Ok(())
}
pub(super) fn unique(e: &E) -> Result<()> {
    let mut seen = std::collections::HashSet::new();
    for c in &e.kids {
        if !seen.insert(c.tag.as_str()) {
            return Err(invalid(&e.tag, format!("duplicate {}", c.tag)));
        }
    }
    Ok(())
}
fn name(ns: Option<&str>, local: &str) -> Result<String> {
    match ns {
        None => Ok(local.to_owned()),
        Some(ns) => match prefix_of(ns) {
            Some("") => Ok(local.to_owned()),
            Some(p) => Ok(format!("{p}:{local}")),
            None => {
                let prefix = mdschema::static_schema()
                    .table("forms/types.json")
                    .and_then(|t| t.as_object())
                    .and_then(|rows| {
                        rows.iter().find_map(|(_, row)| {
                            if row.at(2).and_then(|v| v.as_str()) != Some(ns) {
                                return None;
                            }
                            row.at(1)?
                                .as_str()?
                                .split_once(':')
                                .filter(|(prefix, name)| *prefix != "@" || *name == local)
                                .map(|(prefix, _)| prefix)
                        })
                    });
                match prefix {
                    Some(prefix) => Ok(format!("{prefix}:{local}")),
                    _ => Err(invalid(local, "unknown namespace")),
                }
            }
        },
    }
}
fn qname(n: NodeRef<'_, '_>, value: &str) -> Result<String> {
    let (p, local) = value
        .split_once(':')
        .map_or((None, value), |(p, l)| (Some(p), l));
    let mut at = Some(n);
    while let Some(n) = at {
        if let Some((_, ns)) = n.ns_decls().find(|(k, _)| *k == p) {
            return name(Some(ns), local);
        }
        at = n.parent();
    }
    Err(invalid(n.local(), "unbound QName"))
}
pub(super) fn convert(n: NodeRef<'_, '_>, cancel: &dyn Fn() -> bool) -> Result<E> {
    if cancel() {
        return Err(FormBuildError::Cancelled);
    }
    let tag = name(n.ns(), n.local())?;
    let mut attrs = Vec::new();
    for a in n.attrs() {
        if a.ns() == Some(NS) {
            return Err(invalid(a.local(), "qualified logform attribute"));
        }
        let key = name(a.ns(), a.local())?;
        let value = if key == "xsi:type" || key == "ref" && a.value().contains(':') {
            qname(n, a.value())?
        } else {
            a.value().to_owned()
        };
        attrs.push((key, value));
    }
    let kids: Vec<_> = n
        .children()
        .map(|c| convert(c, cancel))
        .collect::<Result<_>>()?;
    let value = n.text().unwrap_or_default();
    if !kids.is_empty() && !value.trim().is_empty() {
        return Err(invalid(&tag, "mixed XML content"));
    }
    let text = if !kids.is_empty() || value.is_empty() {
        None
    } else if matches!(tag.as_str(), "v8:Type" | "v8:TypeSet")
        || tag.ends_with("Color") && value.contains(':')
    {
        Some(qname(n, value.trim())?)
    } else {
        Some(value.to_owned())
    };
    Ok(E::new(tag, text, attrs, kids))
}
pub(super) fn parse<'src>(
    bytes: &'src [u8],
    budget: &MemoryBudget,
    cancel: &dyn Fn() -> bool,
) -> Result<XmlArena<'src>> {
    if cancel() {
        return Err(FormBuildError::Cancelled);
    }
    budget.check_admission(memory_budget::Category::Execution)?;
    let options = mdconf_core::xml_in::ShellOptions {
        max_xml_depth: 128,
        max_xml_nodes: 32768,
        ..Default::default()
    };
    let mut work = 0;
    let arena = mdconf_core::xml_in::parse_part_checked(
        bytes,
        Path::new("Form.xml"),
        &options,
        budget,
        &mut work,
        cancel,
    )
    .map_err(|e| match e {
        mdconf_core::xml_in::CheckedPartError::Cancelled => FormBuildError::Cancelled,
        mdconf_core::xml_in::CheckedPartError::Memory(e) => FormBuildError::Budget(e),
        mdconf_core::xml_in::CheckedPartError::Part(e) => FormBuildError::Xml(e.to_string()),
    })?;
    let root = arena
        .root()
        .ok_or_else(|| invalid("Form", "missing root"))?;
    if root.ns() != Some(NS) || root.local() != "Form" {
        return Err(invalid("Form", "wrong root or namespace"));
    }
    Ok(arena)
}

pub(super) fn source_map<'a>(
    root: &'a E,
    source: NodeRef<'a, 'a>,
    cancel: &dyn Fn() -> bool,
) -> Result<std::collections::HashMap<usize, NodeRef<'a, 'a>>> {
    fn walk<'a>(
        e: &'a E,
        n: NodeRef<'a, 'a>,
        out: &mut std::collections::HashMap<usize, NodeRef<'a, 'a>>,
        cancel: &dyn Fn() -> bool,
    ) -> Result<()> {
        if cancel() {
            return Err(FormBuildError::Cancelled);
        }
        out.insert(std::ptr::from_ref(e) as usize, n);
        for (e, n) in e.kids.iter().zip(n.children()) {
            walk(e, n, out, cancel)?;
        }
        Ok(())
    }
    let mut out = std::collections::HashMap::new();
    walk(root, source, &mut out, cancel)?;
    Ok(out)
}
