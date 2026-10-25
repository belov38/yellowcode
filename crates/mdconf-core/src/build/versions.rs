use crate::{EntryData, Error, Result};
use brace::{Doc, Guid, Node};

fn bad(what: impl Into<String>) -> Error {
    Error::Format {
        entry: "versions".into(),
        what: what.into(),
    }
}

fn refused(what: &str) -> Error {
    Error::Build {
        object: "Configuration".into(),
        path: "versions".into(),
        what: what.into(),
    }
}

fn doc(stored: &[u8]) -> Result<Doc> {
    match crate::entries::decode_stored(stored)? {
        EntryData::Brace(doc) => Ok(doc),
        _ => Err(bad("not a brace document")),
    }
}

pub(crate) fn stored_of(doc: &Doc) -> Result<Vec<u8>> {
    zraw::deflate(&brace::write_doc(doc), zraw::LEVEL_1C).map_err(|source| Error::Deflate {
        entry: "versions".into(),
        source,
    })
}

pub(crate) fn reencoded(stored: &[u8]) -> Result<Vec<u8>> {
    stored_of(&doc(stored)?)
}

pub(crate) fn patched(stored: &[u8], model: &[(String, Guid)]) -> Result<(Vec<u8>, usize)> {
    let mut doc = doc(stored)?;
    let list = doc.root.as_list_mut().ok_or_else(|| bad("not a list"))?;
    let n: usize = list
        .get(1)
        .and_then(Node::text)
        .and_then(|s| s.trim().parse::<i64>().ok())
        .map_or(0, |n| usize::try_from(n).unwrap_or(0));
    let mut kept = Vec::new();
    for i in 0..n {
        let (at_name, at_ver) = (2 + 2 * i, 3 + 2 * i);
        let name = list
            .get(at_name)
            .and_then(Node::text)
            .ok_or_else(|| bad("short pair list"))?;
        let ver = list
            .get(at_ver)
            .and_then(Node::text)
            .ok_or_else(|| bad("short pair list"))?;
        match Guid::parse(ver) {
            Some(g) => kept.push((name.to_string(), g, at_ver)),
            None if name.is_empty() => {}
            None => return Err(bad(format!("version of {name} is not a GUID"))),
        }
    }
    if kept.len() != model.len() || kept.iter().zip(model).any(|(k, m)| k.0 != m.0) {
        return Err(refused("element list differs from the base (C9/M8)"));
    }
    let mut changed = 0;
    for ((_, old, at), (_, new)) in kept.iter().zip(model) {
        if old == new {
            continue;
        }
        let slot = list.get_mut(*at).ok_or_else(|| bad("short pair list"))?;
        *slot = match slot {
            Node::Str(_) => Node::str(new.to_string()),
            _ => Node::guid(*new),
        };
        changed += 1;
    }
    Ok((stored_of(&doc)?, changed))
}
