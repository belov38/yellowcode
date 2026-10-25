use std::sync::LazyLock;

use super::tables::{ArrayRef, JsonRef, PropRef, TableView};
use brace::Node;

use super::ctx::{DL_SPECIAL, NodeKind, md_name};
use super::decode::{FORMID, Item};
use super::fe::E;
use super::py::{
    Py, Raised, is_digits, py_index, py_int, py_item, py_len, py_seq, py_slice, py_str, str_at,
};
use super::write::{ABSENT, Emit, FormCtx, Src};

static NONE_MARK: LazyLock<Node> = LazyLock::new(|| Node::atom("<none>"));

pub(crate) fn prepare(fc: &mut FormCtx<'_>) {
    fc.form_cat = "none".to_string();
    for ai in 0..fc.info.attrs.len() {
        let main = fc
            .info
            .attrs
            .get(ai)
            .and_then(|a| a.rec.body.get(10))
            .map(|v| py_str(v) == "1");
        match main {
            None => {
                fc.raise();
                return;
            }
            Some(true) => {
                let t = fc.info.attr_tname(ai);
                fc.form_cat = category(t.as_deref());
                return;
            }
            Some(false) => {}
        }
    }
}

pub(crate) fn category(tname: Option<&str>) -> String {
    match tname {
        None | Some("") => "none".to_string(),
        Some(t) => {
            let t = t.split_once(':').map_or(t, |(_, r)| r);
            t.split('.').next().unwrap_or(t).to_string()
        }
    }
}

pub(crate) fn table_cat(fc: &mut FormCtx<'_>, item: &Item) -> String {
    match fc.info.item_node(item) {
        None => "none".to_string(),
        Some(n) if n.kind == NodeKind::Ts => "TabularSection".to_string(),
        Some(n) => category(n.tname.as_deref()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Seg<'p> {
    Int(i64),
    Str(&'p str),
    Other,
}

fn json_segs<'a>(p: ArrayRef<'a>) -> Vec<Seg<'a>> {
    p.iter()
        .map(|v| match (v.as_i64(), v.as_str()) {
            (Some(i), _) => Seg::Int(i),
            (None, Some(s)) => Seg::Str(s),
            _ => Seg::Other,
        })
        .collect()
}

pub(crate) fn path_segs<'a>(ent: PropRef<'a>) -> Option<Vec<Seg<'a>>> {
    ent.path.map(json_segs)
}

fn index_seg<'s>(seq: &'s [Node], s: Option<&Seg<'_>>) -> Py<&'s Node> {
    match s {
        Some(Seg::Int(i)) => py_index(seq, *i),
        _ => Err(Raised),
    }
}

fn below_len(seq: &[Node], s: Option<&Seg<'_>>) -> Py<bool> {
    match s {
        Some(Seg::Int(i)) => Ok(usize::try_from(*i).map_or(true, |u| u < seq.len())),
        _ => Err(Raised),
    }
}

pub(crate) fn value_at<'s>(src: &Src<'s>, path: &[Seg<'_>]) -> Py<Option<&'s Node>> {
    let (head, rest) = path.split_first().ok_or(Raised)?;
    let (mut v, rest): (Option<&'s Node>, &[Seg<'_>]) = match *head {
        Seg::Int(_) => {
            let body = src.body()?;
            let v = if below_len(body, Some(head))? {
                Some(index_seg(body, Some(head))?)
            } else {
                None
            };
            (v, rest)
        }
        Seg::Str(m @ ("bag" | "opt")) => {
            if m == "opt" && !src.has_opt() {
                return Err(Raised);
            }
            let key = match rest.first().ok_or(Raised)? {
                Seg::Int(i) => Some(i.to_string()),
                Seg::Str(s) => Some((*s).to_string()),
                Seg::Other => None,
            };
            let found = key.and_then(|k| if m == "bag" { src.bag(&k) } else { src.opt(&k) });
            (
                Some(found.unwrap_or(&NONE_MARK)),
                rest.get(1..).unwrap_or_default(),
            )
        }
        Seg::Str("h") => {
            let h = src.head()?;
            (
                Some(index_seg(h, rest.first())?),
                rest.get(1..).unwrap_or_default(),
            )
        }
        Seg::Str("t") => {
            let t = src.tail()?;
            let v = if below_len(t, rest.first())? {
                Some(index_seg(t, rest.first())?)
            } else {
                None
            };
            (v, rest.get(1..).unwrap_or_default())
        }
        _ => return Ok(None),
    };
    for s in rest {
        let Some(l) = v.and_then(Node::as_list) else {
            return Ok(None);
        };
        if !below_len(l, Some(s))? {
            return Ok(None);
        }
        v = Some(index_seg(l, Some(s))?);
    }
    Ok(v)
}

pub(crate) fn str_of(v: Option<&Node>) -> String {
    v.map_or_else(|| "None".to_string(), |n| py_str(n).into_owned())
}

pub(crate) fn lstr_items(v: Option<&Node>) -> Option<Vec<(String, String)>> {
    let l = v?.as_list()?;
    if l.len() < 2 || py_str(l.first()?) != "1" {
        return None;
    }
    let n = py_int(&py_str(l.get(1)?)).ok()?;
    let es = py_slice(l, 2, Some(n.saturating_add(2)));
    if i64::try_from(es.len()).ok()? != n {
        return None;
    }
    es.iter()
        .map(|e| {
            let p = e.as_list().filter(|p| p.len() == 2)?;
            Some((
                py_str(p.first()?).into_owned(),
                py_str(p.get(1)?).into_owned(),
            ))
        })
        .collect()
}

pub(crate) fn lstr_kids(items: &[(String, String)]) -> Vec<E> {
    items
        .iter()
        .map(|(lang, text)| {
            E::new(
                "v8:item",
                None,
                Vec::new(),
                vec![
                    E::leaf("v8:lang", lang.clone()),
                    E::leaf("v8:content", text.clone()),
                ],
            )
        })
        .collect()
}

pub(crate) fn flstr(prop: &str, v: Option<&Node>) -> Emit {
    match lstr_items(v) {
        None => Emit::Cannot,
        Some(items) if items.is_empty() => Emit::Absent,
        Some(items) => Emit::El(E::new(prop, None, Vec::new(), lstr_kids(&items))),
    }
}

pub(crate) fn fmt_title(prop: &str, v: Option<&Node>) -> Emit {
    let Some(l) = v
        .and_then(Node::as_list)
        .filter(|l| l.len() == 3 && l.first().is_some_and(|x| py_str(x) == "1"))
    else {
        return Emit::Cannot;
    };
    let Some(items) = lstr_items(l.get(1)) else {
        return Emit::Cannot;
    };
    let formatted = l.get(2).is_some_and(|x| py_str(x) == "1");
    if items.is_empty() && !formatted {
        return Emit::Absent;
    }
    Emit::El(E::new(
        prop,
        None,
        vec![(
            "formatted".to_string(),
            if formatted { "true" } else { "false" }.to_string(),
        )],
        lstr_kids(&items),
    ))
}

fn checked(fc: &mut FormCtx<'_>, r: Py<Emit>) -> Option<Emit> {
    Some(fc.check(r).unwrap_or(Emit::Cannot))
}

fn leaf(prop: &str, text: impl Into<String>) -> Emit {
    Emit::El(E::leaf(prop, text))
}

pub(crate) fn parent(prop: &str, kids: Vec<E>) -> Emit {
    Emit::El(E::new(prop, None, Vec::new(), kids))
}

fn item_by_id<'a>(fc: &FormCtx<'a>, id: &str) -> Option<&'a Item> {
    fc.info.item(id)
}

fn name_text(it: &Item) -> Option<String> {
    it.name().map(String::from)
}

fn name_fmt(it: &Item) -> &str {
    it.name().unwrap_or("None")
}

pub(crate) fn c_fmt_title(
    fc: &mut FormCtx<'_>,
    ctx: &str,
    prop: &str,
    src: &Src<'_>,
    _ent: Option<PropRef<'_>>,
) -> Option<Emit> {
    let at = if ctx.ends_with("/ExtendedTooltip") {
        22
    } else if matches!(ctx, "LabelDecoration" | "PictureDecoration") {
        23
    } else {
        return None;
    };
    let v = value_at(src, &[Seg::Int(at)]);
    checked(fc, v.map(|v| fmt_title(prop, v)))
}

fn field_ev(tag: &str) -> Option<i64> {
    Some(match tag {
        "InputField" => 36,
        "LabelField" => 12,
        "SpreadSheetDocumentField" => 18,
        "CalendarField" => 14,
        "ChartField" | "HTMLDocumentField" => 5,
        "GraphicalSchemaField" => 6,
        "PictureField" => 16,
        "PlannerField" => 7,
        _ => return None,
    })
}

pub(crate) fn event_list(
    tables: TableView<'_>,
    node: Option<&Node>,
) -> Py<Option<Vec<(String, String)>>> {
    let Some(l) = node.and_then(Node::as_list) else {
        return Ok(None);
    };
    let Some(n0) = l.first().map(py_str).filter(|s| is_digits(s)) else {
        return Ok(None);
    };
    let n = py_int(&n0)?;
    let mut out = Vec::new();
    let mut i: i64 = 0;
    while i < n {
        let gi = usize::try_from(i)
            .ok()
            .and_then(|i| i.checked_mul(2)?.checked_add(1))
            .ok_or(Raised)?;
        let g = l.get(gi).ok_or(Raised)?;
        let h = l.get(gi.checked_add(1).ok_or(Raised)?).ok_or(Raised)?;
        let names = tables.events.get("names").ok_or(Raised)?;
        let nm = names.get(py_str(g).as_ref()).and_then(JsonRef::as_str);
        let (Some(nm), Node::Str(h)) = (nm, h) else {
            return Ok(None);
        };
        out.push((nm.to_string(), h.clone()));
        i += 1;
    }
    Ok(Some(out))
}

pub(crate) fn events_el(evs: Vec<(String, String)>) -> Emit {
    if evs.is_empty() {
        return Emit::Absent;
    }
    parent(
        "Events",
        evs.into_iter()
            .map(|(n, h)| E::new("Event", Some(h), vec![("name".to_string(), n)], Vec::new()))
            .collect(),
    )
}

pub(crate) fn c_events(
    fc: &mut FormCtx<'_>,
    ctx: &str,
    _prop: &str,
    src: &Src<'_>,
    _ent: Option<PropRef<'_>>,
) -> Option<Emit> {
    let r = events(fc, ctx, src);
    checked(fc, r)
}

fn events(fc: &FormCtx<'_>, ctx: &str, src: &Src<'_>) -> Py<Emit> {
    let tables = fc.tables();
    if ctx == "Form" {
        let f = src.form().ok_or(Raised)?;
        return Ok(event_list(tables, f.events())?.map_or(Emit::Cannot, events_el));
    }
    let tag = src.tag();
    let mut paths: Vec<Vec<Seg<'_>>> = Vec::new();
    if src.cls()? == "Field" {
        paths.push(vec![Seg::Int(40)]);
        if let Some(k) = tag.and_then(field_ev) {
            paths.push(vec![Seg::Int(39), Seg::Int(k)]);
        }
    } else {
        let Some(p) = tables.events.get("paths").ok_or(Raised)?.get(ctx) else {
            return Ok(Emit::Cannot);
        };
        paths.push(json_segs(p.as_array().ok_or(Raised)?));
    }
    let mut evs = Vec::new();
    for p in &paths {
        let Some(e) = event_list(tables, value_at(src, p)?)? else {
            return Ok(Emit::Cannot);
        };
        evs.extend(e);
    }
    Ok(events_el(evs))
}

pub(crate) fn dp_value(fc: &mut FormCtx<'_>, v: Option<&Node>, prop: &str) -> Emit {
    let Some(v) = v.filter(|v| {
        v.as_list()
            .and_then(<[Node]>::first)
            .is_some_and(|x| py_str(x) != "0")
    }) else {
        return Emit::Absent;
    };
    match fc.info.data_path(v) {
        Some(s) => leaf(prop, s),
        None => Emit::Cannot,
    }
}

fn dp_at(fc: &mut FormCtx<'_>, prop: &str, src: &Src<'_>, path: &[Seg<'_>]) -> Option<Emit> {
    let v = fc.check(value_at(src, path))?;
    Some(dp_value(fc, v, prop))
}

pub(crate) fn c_datapath(
    fc: &mut FormCtx<'_>,
    _ctx: &str,
    prop: &str,
    src: &Src<'_>,
    _ent: Option<PropRef<'_>>,
) -> Option<Emit> {
    let at = match fc.check(src.cls()) {
        None => return Some(Emit::Cannot),
        Some("Field" | "Table") => 11,
        Some("Button") => 9,
        Some(_) => return None,
    };
    Some(dp_at(fc, prop, src, &[Seg::Int(at)]).unwrap_or(Emit::Cannot))
}

pub(crate) fn c_dp_footer(
    fc: &mut FormCtx<'_>,
    _ctx: &str,
    prop: &str,
    src: &Src<'_>,
    _ent: Option<PropRef<'_>>,
) -> Option<Emit> {
    Some(dp_at(fc, prop, src, &[Seg::Int(12)]).unwrap_or(Emit::Cannot))
}

pub(crate) fn c_dp_page_title(
    fc: &mut FormCtx<'_>,
    _ctx: &str,
    prop: &str,
    src: &Src<'_>,
    _ent: Option<PropRef<'_>>,
) -> Option<Emit> {
    Some(dp_at(fc, prop, src, &[Seg::Int(20), Seg::Int(4)]).unwrap_or(Emit::Cannot))
}

pub(crate) fn c_dp_group_title(
    fc: &mut FormCtx<'_>,
    _ctx: &str,
    prop: &str,
    src: &Src<'_>,
    _ent: Option<PropRef<'_>>,
) -> Option<Emit> {
    Some(dp_at(fc, prop, src, &[Seg::Int(20), Seg::Int(5)]).unwrap_or(Emit::Cannot))
}

pub(crate) fn c_rowpicture(
    fc: &mut FormCtx<'_>,
    _ctx: &str,
    prop: &str,
    src: &Src<'_>,
    _ent: Option<PropRef<'_>>,
) -> Option<Emit> {
    let r = rowpicture(fc, prop, src);
    checked(fc, r)
}

fn rowpicture(fc: &mut FormCtx<'_>, prop: &str, src: &Src<'_>) -> Py<Emit> {
    let body = src.body()?;
    let v = body.get(43).ok_or(Raised)?;
    let Some(vl) = v.as_list() else {
        return Ok(Emit::Absent);
    };
    let v0 = py_str(vl.first().ok_or(Raised)?);
    if v0 == "0" {
        return Ok(Emit::Absent);
    }
    let base = body.get(11).ok_or(Raised)?;
    let Some(bl) = base.as_list() else {
        return Ok(Emit::Cannot);
    };
    let b0 = py_str(bl.first().ok_or(Raised)?);
    if b0 == "0" {
        return Ok(Emit::Cannot);
    }
    let (nb, nv) = (py_int(&b0)?, py_int(&v0)?);
    let mut full = vec![Node::atom(nb.saturating_add(nv).to_string())];
    full.extend(py_slice(bl, 1, Some(nb.saturating_add(1))).iter().cloned());
    full.extend(py_slice(vl, 1, Some(nv.saturating_add(1))).iter().cloned());
    let s = fc.info.data_path(&Node::List(full));
    if fc.raised() {
        return Err(Raised);
    }
    let s = match s {
        Some(s) => s,
        None => {
            let r = fc.info.resolve(base);
            if fc.raised() {
                return Err(Raised);
            }
            let Some((r0, _)) = r else {
                return Ok(Emit::Cannot);
            };
            let Some(v1) = vl.get(1).filter(|v1| vl.len() == 2 && py_len(v1) == 1) else {
                return Ok(Emit::Cannot);
            };
            let k = str_at(v1, 0)?;
            let Some((_, nm)) = DL_SPECIAL.iter().find(|(id, _)| *id == k) else {
                return Ok(Emit::Cannot);
            };
            format!("{r0}.{nm}")
        }
    };
    Ok(leaf(prop, s))
}

pub(crate) fn c_addition_source(
    fc: &mut FormCtx<'_>,
    _ctx: &str,
    prop: &str,
    src: &Src<'_>,
    _ent: Option<PropRef<'_>>,
) -> Option<Emit> {
    let r = addition_source(fc, prop, src);
    checked(fc, r)
}

fn addition_source(fc: &FormCtx<'_>, prop: &str, src: &Src<'_>) -> Py<Emit> {
    let v = src.body()?.get(19).ok_or(Raised)?;
    let Some([a, b]) = v.as_list() else {
        return Ok(Emit::Cannot);
    };
    let Some(it) = item_by_id(fc, &py_str(a)) else {
        return Ok(Emit::Absent);
    };
    let t = match py_str(b).as_ref() {
        "0" => "SearchStringRepresentation",
        "1" => "ViewStatusRepresentation",
        "2" => "SearchControl",
        _ => return Ok(Emit::Cannot),
    };
    Ok(parent(
        prop,
        vec![
            E::new("Item", name_text(it), Vec::new(), Vec::new()),
            E::leaf("Type", t),
        ],
    ))
}

fn bag_item(fc: &FormCtx<'_>, prop: &str, src: &Src<'_>, key: &str) -> Py<Emit> {
    let Some(v) = src.bag(key) else {
        return Ok(Emit::Absent);
    };
    let id = str_at(v, 1)?;
    if id == "0" {
        return Ok(Emit::Absent);
    }
    Ok(match item_by_id(fc, &id) {
        Some(it) => Emit::El(E::new(prop, name_text(it), Vec::new(), Vec::new())),
        None => Emit::Cannot,
    })
}

pub(crate) fn c_user_settings_group(
    fc: &mut FormCtx<'_>,
    _ctx: &str,
    prop: &str,
    src: &Src<'_>,
    _ent: Option<PropRef<'_>>,
) -> Option<Emit> {
    let r = bag_item(fc, prop, src, "16");
    checked(fc, r)
}

fn cmd_sources(g: &str) -> Option<&'static str> {
    (g == "2ef6d6fa-847a-485e-8684-d37a3ab5efb8").then_some("FormCommandPanelGlobalCommands")
}

pub(crate) fn cmd_source(fc: &FormCtx<'_>, v: &Node) -> Py<Option<String>> {
    let Some(l) = v.as_list() else {
        return Ok(None);
    };
    if l.len() == 1 {
        return Ok(Some(ABSENT.to_string()));
    }
    let a = py_str(l.first().ok_or(Raised)?);
    let b = py_str(l.get(1).ok_or(Raised)?);
    if b == FORMID {
        if a == "0" {
            return Ok(Some("Form".to_string()));
        }
        return Ok(item_by_id(fc, &a).map(|it| format!("Item.{}", name_fmt(it))));
    }
    if a == "0" {
        let misc = || {
            fc.tables()
                .misc
                .get("cmdsrc")
                .and_then(|m| m.get(b.as_ref()))
                .and_then(JsonRef::as_str)
        };
        return Ok(cmd_sources(&b).or_else(misc).map(String::from));
    }
    Ok(None)
}

pub(crate) fn c_cmd_source(
    fc: &mut FormCtx<'_>,
    _ctx: &str,
    prop: &str,
    src: &Src<'_>,
    _ent: Option<PropRef<'_>>,
) -> Option<Emit> {
    let r = cmd_source_at(fc, prop, src);
    checked(fc, r)
}

fn cmd_source_at(fc: &FormCtx<'_>, prop: &str, src: &Src<'_>) -> Py<Emit> {
    let sub = src.body()?.get(20).ok_or(Raised)?;
    let v = py_item(
        sub,
        if src.tag() == Some("ButtonGroup") {
            1
        } else {
            2
        },
    )?;
    Ok(match cmd_source(fc, &v)? {
        None => Emit::Cannot,
        Some(s) if s == ABSENT => Emit::Absent,
        Some(s) => leaf(prop, s),
    })
}

pub(crate) const CMDID: &str = "409b9a53-7f7e-4178-86c1-33176c7c7a7a";

pub(crate) fn std_cmd<'t>(tables: TableView<'t>, key: &str) -> Py<Option<&'t str>> {
    let cmd = tables.misc.get("cmd").ok_or(Raised)?;
    Ok(cmd.get(key).and_then(JsonRef::as_str))
}

pub(crate) fn command_ref(fc: &mut FormCtx<'_>, v: &Node) -> Py<Option<String>> {
    let Some(l) = v.as_list() else {
        return Ok(None);
    };
    if l.len() == 1 {
        return Ok(Some("0".to_string()));
    }
    let k = py_str(l.first().ok_or(Raised)?);
    let g = py_str(l.get(1).ok_or(Raised)?);
    if g == CMDID {
        let cmds = fc.info.form.root.at(5).ok_or(Raised)?;
        let n = py_int(&str_at(cmds, 1)?)?;
        let rows = py_seq(cmds);
        for r in py_slice(&rows, 2, Some(n.saturating_add(2))) {
            let r1 = py_item(r, 1)?;
            if py_str(py_item(r1.as_ref(), 0)?.as_ref()) == k {
                let c = py_str(py_item(r, 2)?.as_ref()).into_owned();
                return Ok(Some(format!("Form.Command.{c}")));
            }
        }
        return Ok(None);
    }
    if let Some(full) = md_name(fc.cfg, &g) {
        return md_command(fc, full, &k);
    }
    if k == "0" {
        let key = format!("{}|{g}", fc.form_cat);
        return Ok(std_cmd(fc.tables(), &key)?.map(|nm| format!("Form.StandardCommand.{nm}")));
    }
    let Some(it) = item_by_id(fc, &k) else {
        return Ok(None);
    };
    let cat = table_cat(fc, it);
    if fc.raised() {
        return Err(Raised);
    }
    let nm = std_cmd(fc.tables(), &format!("T:{cat}|{g}"))?;
    Ok(nm.map(|nm| format!("Form.Item.{}.StandardCommand.{nm}", name_fmt(it))))
}

pub(crate) fn c_command_name(
    fc: &mut FormCtx<'_>,
    _ctx: &str,
    prop: &str,
    src: &Src<'_>,
    _ent: Option<PropRef<'_>>,
) -> Option<Emit> {
    let r = src
        .body()
        .and_then(|b| b.get(8).ok_or(Raised))
        .and_then(|v| command_ref(fc, v))
        .map(|s| s.map_or(Emit::Cannot, |s| leaf(prop, s)));
    checked(fc, r)
}

pub(crate) fn md_order<'c>(fc: &FormCtx<'c>, full: &str) -> Vec<&'c str> {
    let pref = format!("{full}.");
    let names = &fc.cfg.index.names;
    fc.shared
        .names_order()
        .iter()
        .filter_map(|u| names.get(u))
        .map(String::as_str)
        .filter(|n| n.starts_with(&pref))
        .collect()
}

pub(crate) fn md_command(fc: &FormCtx<'_>, full: &str, k: &str) -> Py<Option<String>> {
    let parts: Vec<&str> = full.split('.').collect();
    let p0 = parts.first().copied().unwrap_or_default();
    if k == "0" && (parts.len() != 2 || p0 == "CommonCommand") {
        return Ok(Some(full.to_string()));
    }
    if p0 == "InformationRegister" && parts.len() == 2 && is_digits(k) && py_int(k)? >= 3 {
        let pref = format!("{full}.Dimension.");
        let dims: Vec<&str> = md_order(fc, full)
            .into_iter()
            .filter_map(|n| n.strip_prefix(pref.as_str()))
            .collect();
        let i = py_int(k)?.saturating_sub(3);
        return Ok(usize::try_from(i)
            .ok()
            .and_then(|i| dims.get(i))
            .map(|d| format!("{full}.StandardCommand.OpenByValue.{d}")));
    }
    let Some(nm) = std_cmd(fc.tables(), &format!("md:{p0}|{k}"))? else {
        return Ok(None);
    };
    if parts.len() == 2 {
        return Ok(Some(format!("{full}.StandardCommand.{nm}")));
    }
    let p1 = parts.get(1).ok_or(Raised)?;
    let last = parts.last().copied().unwrap_or_default();
    Ok(Some(format!("{p0}.{p1}.StandardCommand.{nm}.{last}")))
}

pub(crate) fn cmdset_el(tables: TableView<'_>, prop: &str, v: &Node, cat: &str) -> Py<Emit> {
    let seq = py_seq(v);
    let n = py_int(&py_str(seq.first().ok_or(Raised)?))?;
    let mut names = Vec::new();
    for g in py_slice(&seq, 1, Some(n.saturating_add(1))) {
        let Some(nm) = std_cmd(tables, &format!("{cat}|{}", py_str(g)))? else {
            return Ok(Emit::Cannot);
        };
        names.push(nm);
    }
    if names.is_empty() {
        return Ok(Emit::Absent);
    }
    names.sort_unstable();
    Ok(parent(
        prop,
        names
            .into_iter()
            .map(|x| E::leaf("ExcludedCommand", x))
            .collect(),
    ))
}

pub(crate) fn c_cmdset(
    fc: &mut FormCtx<'_>,
    ctx: &str,
    prop: &str,
    src: &Src<'_>,
    _ent: Option<PropRef<'_>>,
) -> Option<Emit> {
    let r = cmdset(fc, ctx, prop, src);
    checked(fc, r)
}

fn cmdset(fc: &mut FormCtx<'_>, ctx: &str, prop: &str, src: &Src<'_>) -> Py<Emit> {
    let tables = fc.tables();
    if ctx == "Form" {
        let v = src.form().ok_or(Raised)?.command_set().ok_or(Raised)?;
        return cmdset_el(tables, prop, v, &fc.form_cat);
    }
    match src.cls()? {
        "Table" => {
            let v = src.body()?.get(56).ok_or(Raised)?;
            let item = src.item().ok_or(Raised)?;
            let cat = table_cat(fc, item);
            if fc.raised() {
                return Err(Raised);
            }
            cmdset_el(tables, prop, v, &format!("T:{cat}"))
        }
        "Field" => {
            let v = src.body()?.get(48).ok_or(Raised)?;
            cmdset_el(
                tables,
                prop,
                v,
                &format!("F:{}", src.tag().unwrap_or_default()),
            )
        }
        _ => Ok(Emit::Cannot),
    }
}

pub(crate) fn c_mobile_cmdbar(
    fc: &mut FormCtx<'_>,
    _ctx: &str,
    prop: &str,
    src: &Src<'_>,
    _ent: Option<PropRef<'_>>,
) -> Option<Emit> {
    let r = mobile_cmdbar(fc, prop, src);
    checked(fc, r)
}

fn mobile_cmdbar(fc: &FormCtx<'_>, prop: &str, src: &Src<'_>) -> Py<Emit> {
    let v = src.tail()?.get(22).ok_or(Raised)?;
    let seq = py_seq(v);
    let n = py_int(&py_str(seq.get(1).ok_or(Raised)?))?;
    if n == 0 {
        return Ok(Emit::Absent);
    }
    let mut items = Vec::new();
    let mut i: i64 = 0;
    while i < n {
        let pi = usize::try_from(i)
            .ok()
            .and_then(|i| i.checked_mul(2)?.checked_add(2))
            .ok_or(Raised)?;
        let p = seq.get(pi).ok_or(Raised)?;
        let val = seq.get(pi.checked_add(1).ok_or(Raised)?).ok_or(Raised)?;
        let id = str_at(val, 1)?;
        let nm = if id == "0" {
            None
        } else {
            match item_by_id(fc, &id) {
                Some(it) => name_text(it),
                None => return Ok(Emit::Cannot),
            }
        };
        let p = py_str(p);
        items.push(E::new(
            "xr:Item",
            None,
            Vec::new(),
            vec![
                E::new(
                    "xr:Presentation",
                    (!p.is_empty()).then(|| p.into_owned()),
                    Vec::new(),
                    Vec::new(),
                ),
                E::leaf("xr:CheckState", "0"),
                E::new(
                    "xr:Value",
                    nm,
                    vec![("xsi:type".to_string(), "xs:string".to_string())],
                    Vec::new(),
                ),
            ],
        ));
        i += 1;
    }
    Ok(parent(prop, items))
}

fn ext_key(prop: &str) -> Option<&'static str> {
    Some(match prop {
        "ReportResult" => "5",
        "DetailsData" => "6",
        "VariantAppearance" => "20",
        _ => return None,
    })
}

pub(crate) fn c_ext_path(
    fc: &mut FormCtx<'_>,
    _ctx: &str,
    prop: &str,
    src: &Src<'_>,
    _ent: Option<PropRef<'_>>,
) -> Option<Emit> {
    let r = ext_path(fc, prop, src);
    checked(fc, r)
}

fn ext_path(fc: &mut FormCtx<'_>, prop: &str, src: &Src<'_>) -> Py<Emit> {
    let Some(v) = src.bag(ext_key(prop).ok_or(Raised)?) else {
        return Ok(Emit::Absent);
    };
    let p = py_item(v, 2)?;
    let n = py_int(&str_at(&p, 0)?)?;
    if n == 0 {
        return Ok(Emit::Absent);
    }
    let mut full = vec![Node::atom(n.to_string())];
    full.extend(
        py_slice(&py_seq(&p), 1, Some(n.saturating_add(1)))
            .iter()
            .cloned(),
    );
    let s = fc.info.data_path(&Node::List(full));
    if fc.raised() {
        return Err(Raised);
    }
    Ok(s.map_or(Emit::Cannot, |s| leaf(prop, s)))
}

pub(crate) fn c_custom_settings_folder(
    fc: &mut FormCtx<'_>,
    _ctx: &str,
    prop: &str,
    src: &Src<'_>,
    _ent: Option<PropRef<'_>>,
) -> Option<Emit> {
    let r = bag_item(fc, prop, src, "23");
    checked(fc, r)
}
