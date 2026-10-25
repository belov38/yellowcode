use brace::Node;

use super::values::{self, CONTROLS_BORDERS_CLASS, NULL_UUID};
use super::writer_set::drop_empty_events;
use crate::forms::decode::{self, Item};

const MASK: &str = "*";

fn masked() -> Node {
    Node::atom(MASK)
}

fn is_default_user_visible(v: &Node) -> bool {
    let Some(l) = v.as_list() else {
        return false;
    };
    l.len() == 2
        && l.first().and_then(Node::text) == Some("0")
        && l.get(1).and_then(Node::as_list).is_some_and(|m| {
            m.len() == 3
                && m.first().and_then(Node::text) == Some("0")
                && m.get(2).and_then(Node::text) == Some("0")
                && m.get(1).and_then(Node::as_list).is_some_and(|b| {
                    b.len() == 2
                        && b.first().and_then(Node::as_str) == Some("B")
                        && b.get(1).and_then(Node::text) == Some("1")
                })
        })
}

fn canon_value(n: &Node) -> Node {
    let Some(l) = n.as_list() else {
        return n.clone();
    };
    let items: Vec<Node> = l.iter().map(canon_value).collect();
    if let Some(e) = drop_empty_events(&Node::list(items.clone())) {
        return e;
    }
    let head = items.first().and_then(Node::text);
    if head == Some("8")
        && items.len() == 20
        && items.get(1).and_then(Node::text) == Some("0")
        && let Ok(f) = values::font_xml_roundtrip(&Node::list(items.clone()))
    {
        return f;
    }
    if head == Some("3")
        && items.len() == 7
        && items.get(1).and_then(Node::text) == Some("0")
        && items.get(2).is_some_and(Node::is_list)
        && items.get(6).and_then(Node::text) == Some(NULL_UUID)
    {
        let mut b = items;
        if let Some(c) = b.get_mut(6) {
            *c = Node::atom(CONTROLS_BORDERS_CLASS);
        }
        return Node::list(b);
    }
    Node::list(items)
}

fn set(body: &mut [Node], i: usize, n: Node) {
    if let Some(slot) = body.get_mut(i) {
        *slot = n;
    }
}

fn canon_item(it: &mut Item, owner: Option<&str>) {
    for n in it.body.iter_mut() {
        if n.text() != Some(decode::NESTED) {
            *n = canon_value(n);
        }
    }
    let uv_at = if it.class == Some("Button") { 3 } else { 4 };
    if let Some(pos) = it
        .opt
        .iter()
        .position(|(k, v)| *k == "UserVisible" && is_default_user_visible(v))
    {
        it.opt.remove(pos);
        set(&mut it.body, uv_at, Node::atom("0"));
    }
    match it.class {
        Some("Field") => {
            set(&mut it.body, 2, Node::atom("0"));
            set(&mut it.body, 11, masked());
        }
        Some("Button") => set(&mut it.body, 9, masked()),
        Some("Table") => {
            it.bag
                .retain(|(k, _)| !matches!(k.as_str(), "15" | "16" | "19" | "20"));
            let n = i64::try_from(it.bag.len()).unwrap_or(0);
            set(&mut it.body, 54, Node::int(n));
        }
        None if owner == Some("Field") && matches!(it.version.as_str(), "11" | "12") => {
            set(&mut it.body, 2, Node::atom("0"));
        }
        _ => {}
    }
    let class = it.class.unwrap_or("nested");
    for (_, n) in it.nested.iter_mut() {
        canon_item(n, Some(class));
    }
    for c in it.children.iter_mut() {
        canon_item(c, Some(class));
    }
}

fn settings_key_dropped(k: &str) -> bool {
    k == "KeyType"
        || k == "KeyFields"
        || k.starts_with("FieldsMap")
        || k.starts_with("FiledsMap")
        || k.starts_with("ReqMap")
}

fn canon_attributes(section: &Node) -> Node {
    let Some(l) = section.as_list() else {
        return section.clone();
    };
    let n = l
        .get(1)
        .and_then(Node::as_i64)
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(0);
    let mut out: Vec<Node> = l.iter().map(canon_value).collect();
    for row in out.iter_mut().skip(2).take(n) {
        let Some(a) = row.as_list_mut() else {
            continue;
        };
        let ncols = a
            .get(13)
            .and_then(Node::as_i64)
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(0);
        let Some(settings) = a.get_mut(14 + ncols).and_then(Node::as_list_mut) else {
            continue;
        };
        let Some(cnt) = settings
            .get(1)
            .and_then(Node::as_i64)
            .and_then(|n| usize::try_from(n).ok())
        else {
            continue;
        };
        let pairs: Vec<(Node, Node)> = settings
            .get(2..)
            .unwrap_or_default()
            .as_chunks::<2>()
            .0
            .iter()
            .take(cnt)
            .map(|[k, v]| (k.clone(), v.clone()))
            .filter(|(k, _)| !k.as_str().is_some_and(settings_key_dropped))
            .collect();
        let mut s = vec![
            settings.first().cloned().unwrap_or_else(Node::empty),
            Node::int(i64::try_from(pairs.len()).unwrap_or(0)),
        ];
        for (k, v) in pairs {
            s.push(k);
            s.push(v);
        }
        *settings = s;
    }
    let tail_start = 2 + n;
    if let Some(k) = out
        .get(tail_start)
        .and_then(Node::as_i64)
        .and_then(|n| usize::try_from(n).ok())
    {
        let map_start = tail_start + 1 + k;
        let last = out.len().saturating_sub(1);
        for i in map_start..last {
            set(&mut out, i, masked());
        }
    }
    if let Some(last) = out.last_mut() {
        *last = masked();
    }
    Node::list(out)
}

fn canon_command_interface(section: &Node) -> Node {
    let Some(l) = section.as_list() else {
        return section.clone();
    };
    let mut out = l.to_vec();
    for item in out.iter_mut().skip(2) {
        if let Some(it) = item.as_list_mut() {
            set(it, 1, masked());
            set(it, 8, masked());
        }
    }
    Node::list(out)
}

pub(crate) fn canonical(root: &Node) -> Option<Node> {
    let d = decode::decode_form(root)?;
    if !d.layout.serialization.is_4_59() {
        return None;
    }
    let mut form = d.form.clone();
    if let Some(c) = form.children_at {
        let t2 = c + 3;
        if form.body.get(t2).and_then(Node::text) == Some("1")
            && form.body.get(t2 + 1).is_some_and(Node::is_list)
        {
            form.body.remove(t2 + 1);
            set(&mut form.body, t2, Node::atom("0"));
        }
    }
    let k = usize::try_from(form.body.get(18)?.as_i64()?).ok()?;
    let mut pairs: Vec<(Node, Node)> = Vec::with_capacity(k);
    for i in 0..k {
        let key = form.body.get(19 + 2 * i)?.clone();
        let val = form.body.get(20 + 2 * i)?.clone();
        let kn = key.as_i64()?;
        if matches!(kn, 1 | 23 | 24 | 25 | 26 | 27 | 29) {
            continue;
        }
        let val = if matches!(kn, 8..=13 | 17 | 18) {
            masked()
        } else {
            val
        };
        pairs.push((key, val));
    }
    let mut body: Vec<Node> = form.body.get(..18)?.to_vec();
    body.push(Node::int(i64::try_from(pairs.len()).ok()?));
    for (key, val) in &pairs {
        body.push(key.clone());
        body.push(val.clone());
    }
    let removed = 2 * (k - pairs.len());
    body.extend(form.body.get(19 + 2 * k..)?.iter().cloned());
    for (p, _) in form.nested.iter_mut() {
        *p -= removed;
    }
    if let Some(c) = form.children_at.as_mut() {
        *c -= removed;
    }
    form.body = body;
    for n in form.body.iter_mut() {
        if n.text() != Some(decode::NESTED) {
            *n = canon_value(n);
        }
    }
    for (_, acb) in form.nested.iter_mut() {
        canon_item(acb, None);
    }
    for c in form.children.iter_mut() {
        canon_item(c, None);
    }
    let t = root.as_list()?;
    let mut out = t.to_vec();
    set(&mut out, 1, form.to_raw());
    set(&mut out, 2, masked());
    set(&mut out, 3, canon_attributes(t.get(3)?));
    set(&mut out, 6, canon_command_interface(t.get(6)?));
    set(&mut out, 7, canon_command_interface(t.get(7)?));
    Some(Node::list(out))
}
