use std::collections::BTreeMap;

use brace::Node;
use mdschema::forms_upgrade::{UpgradeTables, WriterSet, upgrade_tables};
use regex::Regex;

use super::values::{CONTROLS_BORDERS_CLASS, NULL_UUID};
use super::{Refusal, Stats, translate};
use crate::forms::decode::{self, DecodedForm, Item, NESTED};

pub(crate) fn for_writer(d: &DecodedForm, stats: &mut Stats) -> Result<Node, Refusal> {
    if d.layout.serialization.is_4_59() {
        return Ok(d.root.clone());
    }
    let translated = translate(d, stats)?;
    let tables = upgrade_tables().map_err(|e| Refusal::Table(e.0))?;
    let t = decode::decode_form(&translated).ok_or(Refusal::Shape("translated entry"))?;
    apply(&t, tables, stats)
}

pub(crate) fn apply(
    d: &DecodedForm,
    tables: &UpgradeTables,
    stats: &mut Stats,
) -> Result<Node, Refusal> {
    let ws = &tables.writer_set;
    let kinds = FieldKinds::new(tables);
    let dl = tables
        .guids
        .get("DynamicList")
        .ok_or_else(|| Refusal::Table("DynamicList".into()))?;
    let mut dl_attrs: Vec<String> = Vec::new();
    let mut composer_attrs: Vec<String> = Vec::new();
    for a in &d.attrs {
        let Some(ty) = a.body.get(5) else {
            continue;
        };
        let id = a.id.map(|i| i.to_string()).unwrap_or_default();
        if contains_atom(ty, dl) {
            dl_attrs.push(id.clone());
        }
        if ws.table_bag_21_types.iter().any(|g| contains_atom(ty, g)) {
            composer_attrs.push(id);
        }
    }
    let cx = Rules {
        ws,
        kinds,
        dl_attrs,
        composer_attrs,
    };
    let mut form = d.form.clone();
    form_rules(&mut form, &cx, stats)?;
    for (_, acb) in form.nested.iter_mut() {
        item_rules(acb, &cx, stats);
    }
    for c in form.children.iter_mut() {
        item_rules(c, &cx, stats);
    }
    let t = d.root.as_list().ok_or(Refusal::Shape("entry"))?;
    let mut out = t.to_vec();
    let set = |out: &mut Vec<Node>, i: usize, n: Node| -> Result<(), Refusal> {
        *out.get_mut(i).ok_or(Refusal::Shape("entry"))? = n;
        Ok(())
    };
    set(&mut out, 1, form.to_raw())?;
    set(
        &mut out,
        3,
        attributes_rules(t.get(3).ok_or(Refusal::Shape("entry"))?, ws, stats),
    )?;
    for i in [6, 7] {
        set(
            &mut out,
            i,
            command_interface(t.get(i).ok_or(Refusal::Shape("entry"))?, stats),
        )?;
    }
    Ok(Node::list(out))
}

pub(crate) fn table_bag_21_hits(d: &DecodedForm, tables: &UpgradeTables) -> usize {
    let ws = &tables.writer_set;
    let composer: Vec<String> = d
        .attrs
        .iter()
        .filter(|a| {
            a.body
                .get(5)
                .is_some_and(|ty| ws.table_bag_21_types.iter().any(|g| contains_atom(ty, g)))
        })
        .filter_map(|a| a.id.map(|i| i.to_string()))
        .collect();
    if composer.is_empty() {
        return 0;
    }
    d.items()
        .iter()
        .filter(|it| it.class == Some("Table"))
        .filter(|it| !it.bag.iter().any(|(k, _)| k == "21" || k == "4"))
        .filter(|it| {
            let Some(dp) = it.body.get(11).and_then(Node::as_list) else {
                return false;
            };
            let first = dp
                .get(1)
                .and_then(Node::as_list)
                .filter(|seg| seg.len() == 1)
                .and_then(|seg| seg.first()?.text());
            let user_settings = dp
                .last()
                .and_then(Node::as_list)
                .is_some_and(|seg| seg.len() == 1 && seg.first().and_then(Node::text) == Some("1"));
            user_settings && first.is_some_and(|a| composer.iter().any(|c| c == a))
        })
        .count()
}

struct Rules<'a> {
    ws: &'a WriterSet,
    kinds: FieldKinds,
    dl_attrs: Vec<String>,
    composer_attrs: Vec<String>,
}

struct FieldKinds(BTreeMap<String, (Vec<usize>, Vec<usize>)>);

impl FieldKinds {
    fn new(tables: &UpgradeTables) -> FieldKinds {
        let mut m: BTreeMap<String, (Vec<usize>, Vec<usize>)> = BTreeMap::new();
        for (name, c) in &tables.sublists {
            let Some(kind) = name
                .strip_prefix("Field:")
                .and_then(|s| s.split('/').next())
            else {
                continue;
            };
            let e = m.entry(kind.to_string()).or_default();
            for op in &c.ops {
                match op.ty.as_deref() {
                    Some("Events") if !e.0.contains(&op.new) => e.0.push(op.new),
                    Some("Border") if !e.1.contains(&op.new) => e.1.push(op.new),
                    _ => {}
                }
            }
        }
        FieldKinds(m)
    }
}

fn contains_atom(n: &Node, text: &str) -> bool {
    match n {
        Node::List(l) => l.iter().any(|x| contains_atom(x, text)),
        Node::Atom(a) => a == text,
        Node::Str(_) => false,
    }
}

fn looks_like_events(l: &[Node]) -> Option<usize> {
    let n = usize::try_from(l.first()?.as_i64()?).ok()?;
    if n == 0 || l.len() < 2 * n + 3 {
        return None;
    }
    for i in 0..n {
        if !l.get(1 + 2 * i)?.is_guid() || l.get(2 + 2 * i)?.as_str().is_none() {
            return None;
        }
    }
    (l.get(2 * n + 1)?.text() == Some("1")).then_some(n)
}

pub(crate) fn drop_empty_events(n: &Node) -> Option<Node> {
    let l = n.as_list()?;
    let count = looks_like_events(l)?;
    let keep: Vec<usize> = (0..count)
        .filter(|i| l.get(2 + 2 * i).and_then(Node::as_str) != Some(""))
        .collect();
    if keep.len() == count {
        return None;
    }
    let rest = l.get(1 + 2 * count..)?;
    if rest.get(1)?.text() != Some("0") {
        return None;
    }
    let tail = rest.get(2..)?;
    let mut ents = Vec::with_capacity(count);
    let mut p = 0;
    for _ in 0..count {
        let cnt = usize::try_from(tail.get(p + 2)?.as_i64()?).ok()?;
        let len = 3 + 2 * cnt.saturating_sub(1);
        ents.push(tail.get(p..p + len)?);
        p += len;
    }
    let mut out = vec![Node::int(i64::try_from(keep.len()).ok()?)];
    for &i in &keep {
        out.push(l.get(1 + 2 * i)?.clone());
        out.push(l.get(2 + 2 * i)?.clone());
    }
    out.push(Node::int(1));
    out.push(Node::int(0));
    for &i in &keep {
        out.extend(ents.get(i)?.iter().cloned());
    }
    Some(Node::list(out))
}

pub(crate) fn fix_border(n: &Node) -> Option<Node> {
    let l = n.as_list()?;
    if l.len() != 7
        || l.first()?.text() != Some("3")
        || l.get(1)?.text() != Some("0")
        || !l.get(2)?.is_list()
        || l.get(6)?.text() != Some(NULL_UUID)
    {
        return None;
    }
    let mut b = l.to_vec();
    *b.get_mut(6)? = Node::atom(CONTROLS_BORDERS_CLASS);
    Some(Node::list(b))
}

fn set_at(body: &mut [Node], i: usize, n: Node) {
    if let Some(slot) = body.get_mut(i) {
        *slot = n;
    }
}

fn drop_events_at(body: &mut [Node], i: usize, stats: &mut Stats, key: &str) {
    if let Some(e) = body.get(i).and_then(drop_empty_events) {
        set_at(body, i, e);
        stats.hit(key);
    }
}

fn form_rules(form: &mut Item, cx: &Rules<'_>, stats: &mut Stats) -> Result<(), Refusal> {
    if let Some(c) = form.children_at {
        let t2 = c + 3;
        if form.body.get(t2).and_then(Node::text) == Some("1")
            && form.body.get(t2 + 1).is_some_and(Node::is_list)
        {
            form.body.remove(t2 + 1);
            set_at(&mut form.body, t2, Node::atom("0"));
            stats.hit("rt: form t2 Navigator dropped");
        }
    }
    let k = usize::try_from(
        form.body
            .get(18)
            .and_then(Node::as_i64)
            .ok_or(Refusal::FormBag)?,
    )
    .map_err(|_| Refusal::FormBag)?;
    drop_events_at(
        &mut form.body,
        19 + 2 * k,
        stats,
        "rt: form empty-handler events dropped",
    );
    let mut pairs: Vec<(Node, Node)> = Vec::with_capacity(k + 2);
    for i in 0..k {
        pairs.push((
            form.body.get(19 + 2 * i).ok_or(Refusal::FormBag)?.clone(),
            form.body.get(20 + 2 * i).ok_or(Refusal::FormBag)?.clone(),
        ));
    }
    let has = |pairs: &[(Node, Node)], key: &str| pairs.iter().any(|(k, _)| k.text() == Some(key));
    let mut added = 0;
    if has(&pairs, "7") {
        for (key, value) in &cx.ws.form_bag_with_key7 {
            if !has(&pairs, key) {
                pairs.push((
                    Node::atom(key.clone()),
                    brace::parse(value).map_err(|_| Refusal::Table("form_bag_with_key7".into()))?,
                ));
                stats.hit_owned(format!("rt: form bag key {key} added with 7"));
                added += 1;
            }
        }
    }
    if added > 0 {
        pairs.sort_by_key(|(k, _)| k.as_i64().unwrap_or(i64::MAX));
        let mut body: Vec<Node> = form.body.get(..18).ok_or(Refusal::FormBag)?.to_vec();
        body.push(Node::int(i64::try_from(pairs.len()).unwrap_or(0)));
        for (key, value) in pairs {
            body.push(key);
            body.push(value);
        }
        body.extend(
            form.body
                .get(19 + 2 * k..)
                .ok_or(Refusal::FormBag)?
                .iter()
                .cloned(),
        );
        let shift = 2 * added;
        for (p, _) in form.nested.iter_mut() {
            *p += shift;
        }
        if let Some(c) = form.children_at.as_mut() {
            *c += shift;
        }
        form.body = body;
    }
    Ok(())
}

fn item_rules(it: &mut Item, cx: &Rules<'_>, stats: &mut Stats) {
    match it.class {
        Some("Field") => {
            drop_events_at(
                &mut it.body,
                40,
                stats,
                "rt: field empty-handler events dropped",
            );
            let kind = it
                .body
                .get(5)
                .and_then(Node::text)
                .unwrap_or("")
                .to_string();
            if let Some((events, borders)) = cx.kinds.0.get(&kind)
                && let Some(sub) = it.body.get(39).and_then(Node::as_list)
            {
                let mut q = sub.to_vec();
                let mut changed = false;
                for &p in events {
                    if let Some(e) = q.get(p).and_then(drop_empty_events) {
                        set_at(&mut q, p, e);
                        changed = true;
                    }
                }
                for &p in borders {
                    if let Some(b) = q.get(p).and_then(fix_border) {
                        set_at(&mut q, p, b);
                        changed = true;
                    }
                }
                if changed {
                    set_at(&mut it.body, 39, Node::list(q));
                    stats.hit("rt: field kind sub-list events/border");
                }
            }
        }
        Some("Table") => {
            drop_events_at(
                &mut it.body,
                55,
                stats,
                "rt: table empty-handler events dropped",
            );
            let dp = it.body.get(11).and_then(Node::as_list);
            let first = dp.and_then(|dp| {
                dp.get(1)?
                    .as_list()
                    .filter(|seg| seg.len() == 1)?
                    .first()?
                    .text()
                    .map(str::to_string)
            });
            let one_segment = dp.is_some_and(|dp| dp.len() == 2);
            let has = |it: &Item, key: &str| it.bag.iter().any(|(k, _)| k == key);
            let mut added = false;
            if let Some(a) = &first {
                if one_segment
                    && !has(it, "20")
                    && cx.dl_attrs.contains(a)
                    && let Ok(v) = brace::parse(&cx.ws.table_bag_20_value)
                {
                    it.bag.push(("20".to_string(), v));
                    stats.hit("rt: table bag key 20 added");
                    added = true;
                }
                let user_settings = dp.is_some_and(|dp| {
                    dp.last().and_then(Node::as_list).is_some_and(|seg| {
                        seg.len() == 1 && seg.first().and_then(Node::text) == Some("1")
                    })
                });
                if !has(it, "21")
                    && !has(it, "4")
                    && user_settings
                    && cx.composer_attrs.contains(a)
                    && let Ok(v) = brace::parse(&cx.ws.table_bag_21_value)
                {
                    it.bag.push(("21".to_string(), v));
                    stats.hit("writer-set:table-bag-21");
                    added = true;
                }
            }
            if added {
                it.bag
                    .sort_by_key(|(k, _)| k.parse::<i64>().unwrap_or(i64::MAX));
                let n = i64::try_from(it.bag.len()).unwrap_or(0);
                set_at(&mut it.body, 54, Node::int(n));
            }
        }
        Some("Decoration") | None if it.class.is_some() || it.version == "12" => {
            if it.body.get(5).and_then(Node::text) == Some("0")
                && it.body.get(17).and_then(Node::text) == Some("1")
                && let Some(sub) = it.body.get(18).and_then(Node::as_list)
                && let Some(b) = sub.get(8).and_then(fix_border)
            {
                let mut q = sub.to_vec();
                set_at(&mut q, 8, b);
                set_at(&mut it.body, 18, Node::list(q));
                stats.hit("rt: LabelDecoration border class");
            }
        }
        _ => {}
    }
    let _ = NESTED;
    for (_, n) in it.nested.iter_mut() {
        item_rules(n, cx, stats);
    }
    for c in it.children.iter_mut() {
        item_rules(c, cx, stats);
    }
}

fn base64_xml_text(n: &Node) -> Option<String> {
    let bytes = n.base64()?;
    let s = String::from_utf8(bytes).ok()?;
    Some(s.trim_start_matches('\u{feff}').to_string())
}

fn raw_base64_text(s: &str) -> Option<String> {
    use base64::Engine as _;
    let clean: String = s.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(clean)
        .ok()?;
    let t = String::from_utf8(bytes).ok()?;
    Some(t.trim_start_matches('\u{feff}').to_string())
}

fn raw_base64(text: &str) -> String {
    use base64::Engine as _;
    let mut data = vec![0xEF, 0xBB, 0xBF];
    data.extend_from_slice(text.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(data)
}

fn base64_xml_atom(text: &str) -> Node {
    let mut data = vec![0xEF, 0xBB, 0xBF];
    data.extend_from_slice(text.as_bytes());
    brace::base64_atom(&data, brace::Newline::CrLf, false)
}

pub(crate) fn ca_upgrade(xml: &str, ws: &WriterSet) -> Option<String> {
    let root_re = Regex::new(r"<Settings[^>]*>").ok()?;
    let root = root_re.find(xml)?.as_str().to_string();
    let mut x = xml.to_string();
    if !root.contains("xmlns:pal=") {
        let pal = format!("xmlns:pal=\"{}\"", ws.pal_namespace);
        let decl_re = Regex::new(r"xmlns:(\w+)=").ok()?;
        let at = decl_re
            .captures_iter(&root)
            .find(|c| c.get(1).is_some_and(|m| m.as_str() > "pal"))
            .and_then(|c| c.get(0))
            .map(|m| m.start());
        let new_root = match at {
            Some(i) => format!("{}{pal} {}", root.get(..i)?, root.get(i..)?),
            None => {
                let (head, close) = if root.ends_with("/>") {
                    (root.get(..root.len() - 2)?, "/>")
                } else {
                    (root.get(..root.len() - 1)?, ">")
                };
                format!("{} {pal}{close}", head.trim_end())
            }
        };
        x = x.replacen(&root, &new_root, 1);
    }
    let out_re = Regex::new(r"\r?\n?\s*<outputParameters/>").ok()?;
    x = out_re.replace_all(&x, "").into_owned();
    let empty_re = Regex::new(r"(<Settings[^>]*?)>\s*</Settings>").ok()?;
    x = empty_re.replace_all(&x, "${1}/>").into_owned();
    Some(dcs_names(&x, ws))
}

fn dcs_names(x: &str, ws: &WriterSet) -> String {
    let Ok(re) = Regex::new(r"(<(?:\w+:)?parameter>)([^<]*)(</(?:\w+:)?parameter>)") else {
        return x.to_string();
    };
    re.replace_all(x, |c: &regex::Captures<'_>| {
        let name = c.get(2).map_or("", |m| m.as_str());
        format!(
            "{}{}{}",
            c.get(1).map_or("", |m| m.as_str()),
            ws.dcs_names.get(name).map_or(name, String::as_str),
            c.get(3).map_or("", |m| m.as_str())
        )
    })
    .into_owned()
}

fn attributes_rules(section: &Node, ws: &WriterSet, stats: &mut Stats) -> Node {
    let Some(l) = section.as_list() else {
        return section.clone();
    };
    let mut out = l.to_vec();
    if let Some(last) = out.last_mut()
        && let Some(ca) = last.as_list()
        && ca.len() == 1
        && let Some(atom) = ca.first()
    {
        match base64_xml_text(atom).and_then(|x| ca_upgrade(&x, ws)) {
            Some(x) => {
                let encoded = base64_xml_atom(&x);
                if encoded != *atom {
                    *last = Node::list(vec![encoded]);
                    stats.hit("rt: conditional appearance re-serialized");
                }
            }
            None => stats.hit("writer-set:ca"),
        }
    }
    let n = l
        .get(1)
        .and_then(Node::as_i64)
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(0);
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
        for v in settings.iter_mut().skip(2) {
            if rename_dcs_in_value(v, ws) {
                stats.hit("writer-set:dl-settings-names");
            }
        }
    }
    Node::list(out)
}

fn rename_dcs_in_value(v: &mut Node, ws: &WriterSet) -> bool {
    match v {
        Node::Atom(a) if a.starts_with("#base64:") => {
            let Some(x) = base64_xml_text(v).filter(|x| x.contains("parameter>")) else {
                return false;
            };
            let y = dcs_names(&x, ws);
            if y == x {
                return false;
            }
            *v = base64_xml_atom(&y);
            true
        }
        Node::List(l) => {
            let server_state = l.len() == 2
                && l.first().and_then(Node::as_str) == Some("S")
                && l.get(1).and_then(Node::as_str).is_some();
            if server_state {
                let Some(x) = l
                    .get(1)
                    .and_then(Node::as_str)
                    .and_then(raw_base64_text)
                    .filter(|x| x.contains("parameter>"))
                else {
                    return false;
                };
                let y = dcs_names(&x, ws);
                if y == x {
                    return false;
                }
                if let Some(slot) = l.get_mut(1) {
                    *slot = Node::str(raw_base64(&y));
                }
                return true;
            }
            let mut changed = false;
            for x in l.iter_mut() {
                changed |= rename_dcs_in_value(x, ws);
            }
            changed
        }
        _ => false,
    }
}

fn command_interface(section: &Node, stats: &mut Stats) -> Node {
    let Some(l) = section.as_list() else {
        return section.clone();
    };
    let all_false = Node::list(vec![
        Node::int(0),
        Node::list(vec![
            Node::int(0),
            Node::list(vec![Node::str("B"), Node::int(0)]),
            Node::int(0),
        ]),
    ]);
    let all_true = Node::list(vec![
        Node::int(0),
        Node::list(vec![
            Node::int(0),
            Node::list(vec![Node::str("B"), Node::int(1)]),
            Node::int(0),
        ]),
    ]);
    let mut out = l.to_vec();
    let mut changed = false;
    for (j, item) in out.iter_mut().skip(2).enumerate() {
        let Some(it) = item.as_list_mut() else {
            continue;
        };
        let idx = Node::int(i64::try_from(j).unwrap_or(0));
        if it.get(1) != Some(&idx) {
            set_at(it, 1, idx);
            changed = true;
        }
        if it.get(7).and_then(Node::text) == Some("1") && it.get(8) == Some(&all_false) {
            set_at(it, 8, all_true.clone());
            changed = true;
        }
    }
    if changed {
        stats.hit("rt: command interface renumbered");
    }
    Node::list(out)
}
