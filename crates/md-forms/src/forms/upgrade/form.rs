use brace::Node;

use super::{Cx, Refusal, at, groups, int_at, values};
use crate::forms::decode::{DecodedForm, FORMID, Item, NESTED};

#[derive(Clone)]
enum Slot {
    One(Node),
    FlagObj(Option<Node>),
}

const TAIL: [(i64, bool); 36] = [
    (22, false),
    (23, false),
    (27, true),
    (33, false),
    (33, false),
    (34, false),
    (35, false),
    (37, true),
    (37, true),
    (39, false),
    (39, false),
    (39, false),
    (39, false),
    (40, false),
    (41, false),
    (41, false),
    (42, false),
    (43, false),
    (44, false),
    (45, false),
    (46, false),
    (47, false),
    (48, false),
    (49, false),
    (50, false),
    (50, false),
    (50, true),
    (50, true),
    (51, false),
    (52, false),
    (53, false),
    (54, false),
    (55, false),
    (56, true),
    (57, false),
    (58, false),
];

fn parse_tail(v: i64, tail: &[Node]) -> Result<Vec<Option<Slot>>, Refusal> {
    let mut i = 0;
    let mut out = Vec::with_capacity(36);
    for (cond, fo) in TAIL {
        if v <= cond {
            out.push(None);
            continue;
        }
        let n = tail.get(i).ok_or(Refusal::FormTail)?;
        i += 1;
        if fo {
            let flag = n.as_bool().ok_or(Refusal::FormTail)?;
            let obj = if flag {
                let o = tail.get(i).ok_or(Refusal::FormTail)?.clone();
                i += 1;
                Some(o)
            } else {
                None
            };
            out.push(Some(Slot::FlagObj(obj)));
        } else {
            out.push(Some(Slot::One(n.clone())));
        }
    }
    if i != tail.len() {
        return Err(Refusal::FormTail);
    }
    Ok(out)
}

fn one(slots: &[Option<Slot>], t: usize) -> Result<Node, Refusal> {
    match slots.get(t) {
        Some(Some(Slot::One(n))) => Ok(n.clone()),
        _ => Err(Refusal::FormTail),
    }
}

fn one_int(slots: &[Option<Slot>], t: usize) -> Result<i64, Refusal> {
    one(slots, t)?.as_i64().ok_or(Refusal::FormTail)
}

fn flag_obj(slots: &[Option<Slot>], t: usize) -> Result<Option<Node>, Refusal> {
    match slots.get(t) {
        Some(Some(Slot::FlagObj(o))) => Ok(o.clone()),
        _ => Err(Refusal::FormTail),
    }
}

fn i(n: i64) -> Node {
    Node::int(n)
}

fn head_tail(
    v: i64,
    head: &[Node],
    tail: &[Node],
    cx: &mut Cx<'_>,
) -> Result<(Vec<Node>, Vec<Node>), Refusal> {
    let t = parse_tail(v, tail)?;
    let h = |k: usize| int_at(head, k, "form head");
    let mut f6c = h(1)?;
    let mut f170 = h(16)?;
    let mut f100 = h(10)?;
    if v < 18 {
        f100 = i64::from(f100 > 1);
    }
    let mut f144 = 2;
    let mut f124 = 0;
    if v > 34 {
        f144 = one_int(&t, 5)?;
    }
    if v > 35 {
        f124 = one_int(&t, 6)?;
    }
    if v > 41 {
        f100 = one_int(&t, 14)?;
        f144 = one_int(&t, 15)?;
    }
    let mut f1b0 = 2;
    if v > 43 {
        f1b0 = if one_int(&t, 17)? != 0 { 2 } else { 0 };
    }
    if v > 47 {
        f100 = one_int(&t, 21)?;
    }
    let mut f84 = 1;
    if v > 49 {
        f84 = one_int(&t, 23)?;
    }
    if v > 51 {
        f6c = one_int(&t, 28)?;
    }
    let mut f298 = 0;
    if v > 52 {
        f298 = one_int(&t, 29)?;
    }
    let f29c;
    if v < 54 {
        f29c = if f170 == 1 { 2 } else { i64::from(f170 != 0) };
    } else {
        f29c = one_int(&t, 30)?;
        if f29c == 2 {
            f170 = 1;
        } else if f29c == 0 {
            f170 = 0;
        } else if f170 != 2 && f170 != 3 {
            f170 = 2;
        }
    }
    if v > 54 {
        f100 = one_int(&t, 31)?;
    }
    let mut f11c = 0;
    if v > 55 {
        f11c = one_int(&t, 32)?;
        if f11c == 4 {
            f11c = 0;
        }
    }
    if v > 57 {
        f1b0 = one_int(&t, 34)?.min(2);
    }
    let mut f120 = 0;
    if v > 58 {
        f120 = one_int(&t, 35)?;
    }
    let mut eh: Vec<Node> = head.to_vec();
    let set = |eh: &mut Vec<Node>, k: usize, n: Node| -> Result<(), Refusal> {
        *eh.get_mut(k).ok_or(Refusal::FormTail)? = n;
        Ok(())
    };
    set(&mut eh, 1, i(if f6c == 3 { 0 } else { f6c }))?;
    set(&mut eh, 10, i(if f100 > 1 { 1 } else { f100 }))?;
    set(&mut eh, 16, i(f170))?;
    let t22 = match t.get(22) {
        Some(Some(Slot::One(n))) => {
            let l = n.as_list().ok_or(Refusal::FormTail)?;
            let mut out = vec![i(59)];
            out.extend(l.iter().skip(1).cloned());
            Node::list(out)
        }
        _ => Node::list(vec![i(59), i(0)]),
    };
    let or_default = |k: usize, d: Node| -> Result<Node, Refusal> {
        match t.get(k) {
            Some(Some(Slot::One(n))) => Ok(n.clone()),
            _ => Ok(d),
        }
    };
    let or_none = |k: usize| -> Result<Option<Node>, Refusal> {
        match t.get(k) {
            Some(Some(Slot::FlagObj(o))) => Ok(o.clone()),
            _ => Ok(None),
        }
    };
    let navigator = match flag_obj(&t, 2)? {
        Some(obj) => {
            cx.stats.hit("form t2 Navigator kept and converted");
            Some(navigator(&obj, cx)?)
        }
        None => None,
    };
    let empty_picture = cx.empty_picture.clone();
    let empty_string = cx.parse_default(
        cx.tables
            .defaults
            .get("EmptyLocalString")
            .map_or("{1,0}", String::as_str),
    )?;
    let slots: [Slot; 36] = [
        Slot::One(one(&t, 0)?),
        Slot::One(one(&t, 1)?),
        Slot::FlagObj(navigator),
        Slot::One(one(&t, 3)?),
        Slot::One(one(&t, 4)?),
        Slot::One(i(if f144 > 2 { 0 } else { f144 })),
        Slot::One(i(f124)),
        Slot::FlagObj(None),
        Slot::FlagObj(None),
        Slot::One(one(&t, 9)?),
        Slot::One(one(&t, 10)?),
        Slot::One(one(&t, 11)?),
        Slot::One(one(&t, 12)?),
        Slot::One(one(&t, 13)?),
        Slot::One(i(if f100 > 2 { 1 } else { f100 })),
        Slot::One(i(f144)),
        Slot::One(one(&t, 16)?),
        Slot::One(i(i64::from(f1b0 != 0))),
        Slot::One(one(&t, 18)?),
        Slot::One(one(&t, 19)?),
        Slot::One(one(&t, 20)?),
        Slot::One(i(if f100 == 4 || f100 == 5 { 0 } else { f100 })),
        Slot::One(t22),
        Slot::One(i(f84)),
        Slot::One(or_default(24, empty_string)?),
        Slot::One(or_default(25, empty_picture)?),
        Slot::FlagObj(or_none(26)?),
        Slot::FlagObj(or_none(27)?),
        Slot::One(i(f6c)),
        Slot::One(i(f298)),
        Slot::One(i(f29c)),
        Slot::One(i(f100)),
        Slot::One(i(f11c)),
        Slot::FlagObj(or_none(33)?),
        Slot::One(i(f1b0)),
        Slot::One(i(f120)),
    ];
    let mut et = Vec::with_capacity(40);
    for s in slots {
        match s {
            Slot::One(n) => et.push(n),
            Slot::FlagObj(None) => et.push(i(0)),
            Slot::FlagObj(Some(o)) => {
                et.push(i(1));
                et.push(o);
            }
        }
    }
    Ok((eh, et))
}

fn navigator(raw: &Node, cx: &mut Cx<'_>) -> Result<Node, Refusal> {
    let mut o: Vec<Node> = raw.as_list().ok_or(Refusal::Navigator)?.to_vec();
    if o.first().and_then(Node::text) != Some("22") {
        return Err(Refusal::Navigator);
    }
    let uv = take_user_visible(&mut o, 4);
    group22_raw(&mut o, cx).map_err(|_| Refusal::Navigator)?;
    for x in o.iter_mut() {
        let is_tooltip = x.as_list().is_some_and(|e| {
            matches!(e.first().and_then(Node::text), Some("11" | "12"))
                && e.len() > 30
                && e.get(1)
                    .and_then(Node::as_list)
                    .is_some_and(|p| p.len() == 1 && p.first().and_then(Node::text) == Some("0"))
        });
        if is_tooltip {
            let mut e: Vec<Node> = x.as_list().unwrap_or_default().to_vec();
            let euv = take_user_visible(&mut e, 4);
            decoration12_raw(&mut e, cx).map_err(|_| Refusal::Navigator)?;
            if let Some(v) = euv {
                e.insert(5.min(e.len()), v);
            }
            *x = Node::list(e);
        }
    }
    if let Some(v) = uv {
        o.insert(5.min(o.len()), v);
    }
    Ok(Node::list(o))
}

fn take_user_visible(o: &mut Vec<Node>, flag_at: usize) -> Option<Node> {
    if o.get(flag_at).and_then(Node::text) == Some("1")
        && o.get(flag_at + 1).is_some_and(Node::is_list)
    {
        Some(o.remove(flag_at + 1))
    } else {
        None
    }
}

fn group22_raw(o: &mut [Node], cx: &mut Cx<'_>) -> Result<(), Refusal> {
    let c16 = cx.upgrade_value("Color", at(o, 16, "Navigator[16]")?, "Navigator")?;
    let f17 = cx.upgrade_value("Font", at(o, 17, "Navigator[17]")?, "Navigator")?;
    *o.get_mut(16).ok_or(Refusal::Navigator)? = c16;
    *o.get_mut(17).ok_or(Refusal::Navigator)? = f17;
    let kind = at(o, 5, "Navigator[5]")?.text().unwrap_or("").to_string();
    if at(o, 19, "Navigator[19]")?.text() == Some("1") && kind != "7" {
        let s = groups::group_kind(&kind, at(o, 20, "Navigator[20]")?, cx)?;
        *o.get_mut(20).ok_or(Refusal::Navigator)? = s;
    }
    Ok(())
}

fn decoration12_raw(e: &mut Vec<Node>, cx: &mut Cx<'_>) -> Result<(), Refusal> {
    let old_ver = int_at(e, 0, "tooltip")?;
    let c14 = cx.upgrade_value("Color", at(e, 14, "tooltip[14]")?, "Tooltip")?;
    let f15 = cx.upgrade_value("Font", at(e, 15, "tooltip[15]")?, "Tooltip")?;
    *e.get_mut(0).ok_or(Refusal::Navigator)? = i(12);
    *e.get_mut(15).ok_or(Refusal::Navigator)? = f15;
    let kind = at(e, 5, "tooltip[5]")?.text().unwrap_or("").to_string();
    if at(e, 17, "tooltip[17]")?.text() == Some("1") {
        let s = groups::decoration_kind(&kind, at(e, 18, "tooltip[18]")?, &c14, cx)?;
        *e.get_mut(18).ok_or(Refusal::Navigator)? = s;
    }
    *e.get_mut(14).ok_or(Refusal::Navigator)? = c14;
    if old_ver < 12 {
        e.push(i(0));
    }
    Ok(())
}

fn form_bag(
    v: i64,
    pairs: Vec<(&str, &Node)>,
    cx: &mut Cx<'_>,
) -> Result<Vec<(i64, Node)>, Refusal> {
    let mut out: Vec<(i64, Node)> = Vec::new();
    for (k, val) in pairs {
        let key: i64 = k.parse().map_err(|_| Refusal::FormBag)?;
        if key == 25 || key == 26 || key >= 32 {
            cx.stats.hit("form bag key dropped by reader");
            continue;
        }
        match out.iter_mut().find(|(kk, _)| *kk == key) {
            Some(slot) => slot.1 = val.clone(),
            None => out.push((key, val.clone())),
        }
    }
    if v < 27 && !out.iter().any(|(k, _)| *k == 21) {
        let g = cx
            .tables
            .guids
            .get("FormBagKey21Enum")
            .ok_or_else(|| Refusal::Table("FormBagKey21Enum".into()))?;
        out.push((
            21,
            Node::list(vec![Node::str("#"), Node::atom(g.clone()), i(3)]),
        ));
    }
    out.sort_by_key(|(k, _)| *k);
    Ok(out)
}

pub(crate) fn translate_form(
    d: &DecodedForm,
    acb: Item,
    children: Vec<Item>,
    cx: &mut Cx<'_>,
) -> Result<Item, Refusal> {
    let v: i64 = d.form.version.parse().map_err(|_| Refusal::FormTail)?;
    let (head, tail) = head_tail(v, d.head(), d.tail(), cx)?;
    let bag = form_bag(v, d.ext(), cx)?;
    let events_old = d.events().ok_or(Refusal::Events)?;
    let events = values::events(events_old)?;
    if events != *events_old {
        cx.stats.hit("form events normalized by reader");
    }
    let command_set = cx.upgrade_value(
        "GuidSet",
        d.command_set().ok_or(Refusal::Shape("CommandSet"))?,
        "Form",
    )?;
    let mut body = vec![i(59)];
    body.extend(head);
    body.push(i(i64::try_from(bag.len()).unwrap_or(0)));
    for (k, val) in bag {
        body.push(i(k));
        body.push(val);
    }
    body.push(events);
    body.push(command_set);
    body.push(i(1));
    let acb_at = body.len();
    body.push(Node::atom(NESTED));
    let children_at = body.len();
    body.push(i(i64::try_from(children.len()).unwrap_or(0)));
    body.extend(tail);
    Ok(Item {
        id_text: d.form.id_text.clone(),
        class: None,
        version: "59".to_string(),
        body,
        children,
        nested: vec![(acb_at, acb)],
        children_at: Some(children_at),
        opt: Vec::new(),
        bag: Vec::new(),
        bag_at: None,
    })
}

pub(crate) fn id_pass(
    acb: &mut Item,
    children: &mut [Item],
    cx: &mut Cx<'_>,
) -> Result<(), Refusal> {
    fn max_id(it: &Item, mx: &mut i64) {
        if let Ok(n) = it.id_text.parse::<i64>() {
            *mx = (*mx).max(n);
        }
        for (_, n) in &it.nested {
            max_id(n, mx);
        }
        for c in &it.children {
            max_id(c, mx);
        }
    }
    fn visit(parent: &mut Item, next: &mut i64, cx: &mut Cx<'_>) -> Result<(), Refusal> {
        let parent_id = parent.id_text.clone();
        let parent_name = parent.name().map(str::to_string);
        let mut kids: Vec<&mut Item> = parent.nested.iter_mut().map(|(_, n)| n).collect();
        kids.extend(parent.children.iter_mut());
        for ch in kids {
            if ch.id_text == "0" || ch.id_text == parent_id {
                ch.id_text = next.to_string();
                *ch.body.get_mut(1).ok_or(Refusal::Shape("item id"))? =
                    Node::list(vec![i(*next), Node::atom(FORMID)]);
                *next += 1;
                cx.stats.hit("id pass: id assigned");
            }
            if parent_name.is_some() && ch.name().map(str::to_string) == parent_name {
                return Err(Refusal::IdPassRename);
            }
            visit(ch, next, cx)?;
        }
        Ok(())
    }
    let mut mx = 0;
    max_id(acb, &mut mx);
    for c in children.iter() {
        max_id(c, &mut mx);
    }
    let mut next = mx + 1;
    visit(acb, &mut next, cx)?;
    for c in children.iter_mut() {
        visit(c, &mut next, cx)?;
    }
    Ok(())
}
