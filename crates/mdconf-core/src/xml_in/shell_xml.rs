use super::walk::err;
use super::{ShellOptions, XmlFormat};
use crate::{
    Diagnostics, Result,
    xml::{
        read,
        view::{NodeRef, XmlArena},
    },
};
use memory_budget::MemoryBudget;
use quick_xml::{Reader, events::Event};
use std::path::Path;

pub(super) const MD: &str = "http://v8.1c.ru/8.3/MDClasses";
pub(super) const XR: &str = "http://v8.1c.ru/8.3/xcf/readable";
pub(super) type N<'a, 's> = NodeRef<'a, 's>;
pub(super) fn attr<'a>(n: N<'a, '_>, ns: Option<&str>, tag: &str) -> Option<&'a str> {
    n.attrs()
        .find(|a| a.ns() == ns && a.local() == tag)
        .map(|a| a.value())
}
pub(super) fn block<'a, 's>(
    n: N<'a, 's>,
    ns: &str,
    tag: &str,
    path: &Path,
) -> Result<Option<N<'a, 's>>> {
    let mut matches = n
        .children()
        .filter(|c| c.ns() == Some(ns) && c.local() == tag);
    let first = matches.next();
    if matches.next().is_some() {
        return Err(err(path, format!("duplicate {tag}")));
    }
    Ok(first)
}
pub(super) fn required<'a, 's>(
    n: N<'a, 's>,
    ns: &str,
    tag: &str,
    path: &Path,
) -> Result<N<'a, 's>> {
    block(n, ns, tag, path)?.ok_or_else(|| err(path, format!("missing {tag}")))
}
pub(super) fn leaf<'a>(n: N<'a, '_>, path: &Path) -> Result<&'a str> {
    if n.children().next().is_some() || n.attrs().next().is_some() {
        return Err(err(path, "expected text-only element"));
    }
    Ok(n.text().unwrap_or_default())
}
pub(super) fn attributes(n: N<'_, '_>, allowed: &[&str], path: &Path) -> Result<()> {
    if n.attrs()
        .any(|a| a.ns().is_some() || !allowed.contains(&a.local()))
    {
        return Err(err(path, "unknown structural attribute"));
    }
    Ok(())
}
pub(super) fn parse<'a>(
    bytes: &'a [u8],
    path: &Path,
    o: &ShellOptions,
    budget: &MemoryBudget,
    text_work: &mut u64,
) -> Result<XmlArena<'a>> {
    let mut r = Reader::from_reader(bytes);
    let mut nodes = 0usize;
    let mut depth = 0usize;
    let mut text_lengths = Vec::<u64>::new();
    loop {
        let event = r.read_event().map_err(|e| err(path, e.to_string()))?;
        match &event {
            Event::Start(_) => {
                depth += 1;
                if depth > o.max_xml_depth {
                    return Err(err(path, "XML depth limit"));
                }
                text_lengths.push(0);
            }
            Event::Empty(_) => {
                if depth + 1 > o.max_xml_depth {
                    return Err(err(path, "XML depth limit"));
                }
            }
            Event::End(_) => {
                depth = depth.saturating_sub(1);
                text_lengths.truncate(depth);
            }
            Event::Text(t) => charge_text(t.len(), &mut text_lengths, text_work, o, path)?,
            Event::CData(t) => charge_text(t.len(), &mut text_lengths, text_work, o, path)?,
            _ => {}
        }
        match event {
            Event::Start(e) | Event::Empty(e) => {
                nodes += 1;
                if nodes > o.max_xml_nodes {
                    return Err(err(path, "XML node limit"));
                }
                let mut attrs = 0usize;
                let mut ns = 0usize;
                for a in e.attributes().with_checks(false) {
                    let a = a.map_err(|e| err(path, e.to_string()))?;
                    attrs += 1;
                    if a.key.as_ref() == b"xmlns" || a.key.as_ref().starts_with(b"xmlns:") {
                        ns += 1;
                    }
                    if attrs > o.max_xml_attributes || ns > o.max_xml_namespaces {
                        return Err(err(path, "XML attribute/namespace limit"));
                    }
                }
            }
            Event::DocType(_) => return Err(err(path, "DTD is unsupported")),
            Event::Eof => break,
            _ => {}
        }
    }
    read::parse_arena(bytes, crate::allocation::Allocation::Managed(budget)).map_err(|e| match e {
        read::ReadError::Memory(e) => err(path, format!("XML allocation limit: {e}")),
        e => err(path, read::legacy_error(bytes, e).to_string()),
    })
}
fn charge_text(
    len: usize,
    lengths: &mut [u64],
    work: &mut u64,
    o: &ShellOptions,
    path: &Path,
) -> Result<()> {
    if let Some(total) = lengths.last_mut() {
        *total = total.saturating_add(len as u64);
        *work = work.saturating_add(*total);
        if *work > o.max_xml_text_work {
            return Err(err(path, "XML text work limit"));
        }
    }
    Ok(())
}
pub(super) fn description<'a, 's>(
    arena: &'a XmlArena<'s>,
    tag: &str,
    path: &Path,
    diagnostics: &mut Diagnostics,
) -> Result<(N<'a, 's>, XmlFormat)> {
    let root = arena.root().ok_or_else(|| err(path, "missing XML root"))?;
    if root.ns() != Some(MD) || root.local() != "MetaDataObject" {
        return Err(err(path, "wrong description root"));
    }
    attributes(root, &["version"], path)?;
    let format = match attr(root, None, "version") {
        Some("2.21") => XmlFormat::V2_21,
        Some("2.20") => XmlFormat::V2_20,
        _ => return Err(err(path, "unsupported dump version")),
    };
    let mut children = root.children();
    let n = children.next().ok_or_else(|| err(path, "missing object"))?;
    if children.next().is_some() || n.ns() != Some(MD) || n.local() != tag {
        return Err(err(path, format!("expected {tag} description")));
    }
    canonical(root, path, diagnostics)?;
    Ok((n, format))
}
fn canonical(n: N<'_, '_>, path: &Path, diagnostics: &mut Diagnostics) -> Result<()> {
    let canonical = crate::xml::parse_xml(format!("<a {}/>", crate::xml_out::NS_DECL).as_bytes())
        .map_err(|e| err(path, e.to_string()))?;
    let mut stack = vec![n];
    let mut extensions = 0usize;
    let mut first_extension = None;
    while let Some(n) = stack.pop() {
        for (prefix, uri) in n.ns_decls() {
            if prefix.is_some()
                && let Some(expected) = canonical.lookup_prefix(prefix).or_else(|| {
                    prefix
                        .and_then(mdschema::FixedNamespace::from_prefix)
                        .map(|ns| ns.uri())
                })
                && expected != uri
            {
                return Err(err(path, "noncanonical namespace binding"));
            }
        }
        let qname = if n.ns() == Some("http://v8.1c.ru/8.1/data/core")
            && matches!(n.local(), "Type" | "TypeSet")
        {
            n.text()
        } else {
            None
        };
        let xsi = attr(n, Some("http://www.w3.org/2001/XMLSchema-instance"), "type");
        for (q, type_member) in qname
            .into_iter()
            .map(|q| (q, true))
            .chain(xsi.map(|q| (q, false)))
        {
            let (binding, local) = super::namespaces::expanded(&n, q)
                .ok_or_else(|| err(path, "malformed or unbound QName"))?;
            let prefix = q.split_once(':').map(|(prefix, _)| prefix);
            let expected = prefix.and_then(|prefix| canonical.lookup_prefix(Some(prefix)));
            if expected.is_some_and(|expected| binding != expected) {
                return Err(err(path, "wrong QName namespace binding"));
            }
            let admitted = type_member
                && super::values::admitted_type(
                    mdschema::schema()?,
                    binding,
                    local,
                    n.local() == "TypeSet",
                );
            if (type_member && !admitted) || (!type_member && expected.is_none()) {
                extensions += 1;
                if first_extension.is_none() {
                    first_extension = Some(q.to_string());
                }
            }
        }
        stack.extend(n.children());
    }
    if let Some(first) = first_extension {
        diagnostics.unsupported.push(format!(
            "{}: {extensions} unsupported QName(s), first {first}",
            path.display()
        ));
    }
    Ok(())
}
