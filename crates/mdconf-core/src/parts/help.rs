use super::{Help, R, int, miss, plain_path};
use crate::entries::{Entries, EntryData};
use crate::load::LoadCtx;
use crate::{Configuration, MdKind, MdObject, Result};
use brace::{Guid, Node};

fn payload(n: &Node) -> R<Vec<u8>> {
    n.at(0).and_then(Node::base64).ok_or("malformed")
}

fn parse_help(t: &Node) -> R<Help> {
    let l = t.as_list().ok_or("malformed")?;
    let n = l
        .get(1)
        .and_then(int)
        .and_then(|n| usize::try_from(n).ok())
        .ok_or("malformed")?;
    if n > l.len() {
        return Err("malformed");
    }
    let mut h = Help::default();
    for i in 0..n {
        let lang = l.get(2 + 2 * i).and_then(Node::text).ok_or("malformed")?;
        let html = payload(l.get(3 + 2 * i).ok_or("malformed")?)?;
        h.pages.push((lang.to_string(), html));
    }
    let mut i = 2 + 2 * n;
    if let Some(k) = l.get(i) {
        let k = usize::try_from(int(k).ok_or("malformed")?).unwrap_or(0);
        if k > l.len() {
            return Err("malformed");
        }
        i += 1;
        for _ in 0..k {
            let name = l.get(i).and_then(Node::text).ok_or("malformed")?;
            let data = payload(l.get(i + 2).ok_or("malformed")?)?;
            h.files.push((name.to_string(), data));
            i += 3;
        }
    }
    let ok = h
        .pages
        .iter()
        .all(|(lang, _)| plain_path(&format!("{lang}.html")))
        && h.files.iter().all(|(n, _)| plain_path(n));
    if !ok {
        return Err("path");
    }
    Ok(h)
}

fn find_named(e: &Entries, id: Guid, forget_others: bool) -> Option<(String, Option<Node>)> {
    for suf in 0..20 {
        let name = format!("{id}.{suf}");
        if !e.contains(&name) {
            continue;
        }
        let Ok(Some(d)) = e.get(&name) else { continue };
        match &*d {
            EntryData::Brace(doc)
                if doc.bom
                    && doc.lead.is_empty()
                    && doc.root.as_list().is_some_and(|l| l.len() >= 2)
                    && doc.root.at(0) == Some(&Node::atom("5")) =>
            {
                return Some((name, Some(doc.root.clone())));
            }
            EntryData::Bytes(b) if b.starts_with(b"\xef\xbb\xbf{5,") => return Some((name, None)),
            _ => {}
        }
        if forget_others {
            e.forget(&name);
        }
    }
    None
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn find_help_entry(e: &Entries, id: Guid) -> Option<Node> {
    find_named(e, id, false).and_then(|(_, n)| n)
}

fn load_one(cx: &LoadCtx, id: Guid, misses: &mut Vec<&'static str>) -> Option<Help> {
    let keep = cx.opts.keep_source;
    let (name, node) = find_named(cx.entries, id, !keep)?;
    if !keep {
        cx.entries.forget(&name);
    }
    let Some(node) = node else {
        misses.push("not_brace");
        return None;
    };
    match parse_help(&node) {
        Ok(h) => Some(h),
        Err(why) => {
            misses.push(why);
            None
        }
    }
}

fn load_objects(cx: &LoadCtx, objs: &mut [MdObject], misses: &mut Vec<&'static str>) {
    for o in objs {
        if o.kind != MdKind::CommonTemplate && o.kind.folder().is_some() {
            o.parts.help = load_one(cx, o.id, misses);
        }
        for f in &o.forms {
            if let Some(h) = load_one(cx, f.id, misses) {
                o.parts.form_help.push((f.id, h));
            }
        }
        load_objects(cx, &mut o.subsystems, misses);
    }
}

pub(crate) fn load(cx: &LoadCtx, cfg: &mut Configuration) -> Result<()> {
    let mut misses = Vec::new();
    load_objects(cx, &mut cfg.objects, &mut misses);
    cfg.parts.help = load_one(cx, cfg.header_id, &mut misses);
    for why in misses {
        miss(cfg, &["help", why]);
    }
    Ok(())
}
