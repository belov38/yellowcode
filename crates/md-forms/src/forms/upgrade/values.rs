use super::Refusal;
use brace::Node;

pub(crate) const NULL_UUID: &str = "00000000-0000-0000-0000-000000000000";
pub(crate) const CONTROLS_BORDERS_CLASS: &str = "48312c09-257f-4b29-b280-284dd89efc1e";
pub(crate) const CHOICE_LIST_ITEM: &str = "0e704aa2-07bd-48b9-8223-a0212c4d5fc2";

fn refuse(ty: &'static str, v: &Node) -> Refusal {
    Refusal::Value {
        ty,
        version: v.at(0).and_then(Node::text).unwrap_or("?").to_string(),
    }
}

struct Cursor<'a> {
    items: &'a [Node],
    at: usize,
    ty: &'static str,
    whole: &'a Node,
}

impl<'a> Cursor<'a> {
    fn new(ty: &'static str, v: &'a Node) -> Result<(i64, Cursor<'a>), Refusal> {
        let items = v.as_list().ok_or_else(|| refuse(ty, v))?;
        let ver = items
            .first()
            .and_then(Node::as_i64)
            .ok_or_else(|| refuse(ty, v))?;
        Ok((
            ver,
            Cursor {
                items,
                at: 1,
                ty,
                whole: v,
            },
        ))
    }

    fn next(&mut self) -> Result<&'a Node, Refusal> {
        let n = self
            .items
            .get(self.at)
            .ok_or_else(|| refuse(self.ty, self.whole))?;
        self.at += 1;
        Ok(n)
    }

    fn int(&mut self) -> Result<i64, Refusal> {
        let n = self.next()?;
        n.as_i64().ok_or_else(|| refuse(self.ty, self.whole))
    }
}

fn int_of(ty: &'static str, whole: &Node, n: Option<&Node>) -> Result<i64, Refusal> {
    n.and_then(Node::as_i64).ok_or_else(|| refuse(ty, whole))
}

fn atom(n: i64) -> Node {
    Node::int(n)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Cid {
    id: i64,
    uuid: String,
    null: bool,
}

pub(crate) fn cid_read(ty: &'static str, whole: &Node, v: &Node) -> Result<Cid, Refusal> {
    match v.as_list() {
        Some(l) => {
            let id = int_of(ty, whole, l.first())?;
            match l.get(1) {
                None => Ok(Cid {
                    id,
                    uuid: NULL_UUID.to_string(),
                    null: true,
                }),
                Some(u) => {
                    let uuid = u.text().ok_or_else(|| refuse(ty, whole))?.to_string();
                    let null = id == 0 && uuid.eq_ignore_ascii_case(NULL_UUID);
                    Ok(Cid { id, uuid, null })
                }
            }
        }
        None => cid_from_int(ty, whole, v),
    }
}

fn cid_from_int(ty: &'static str, whole: &Node, v: &Node) -> Result<Cid, Refusal> {
    Ok(Cid {
        id: int_of(ty, whole, Some(v))?,
        uuid: NULL_UUID.to_string(),
        null: true,
    })
}

pub(crate) fn cid_write(c: &Cid) -> Node {
    if c.null {
        Node::list(vec![atom(c.id)])
    } else {
        Node::list(vec![atom(c.id), Node::atom(c.uuid.clone())])
    }
}

fn cid_null() -> Cid {
    Cid {
        id: 0,
        uuid: NULL_UUID.to_string(),
        null: true,
    }
}

fn color(v: &Node) -> Result<Node, Refusal> {
    const T: &str = "Color";
    let (ver, mut r) = Cursor::new(T, v)?;
    let (kind, cid) = if ver == 1 {
        (0, cid_from_int(T, v, r.next()?)?)
    } else {
        let mut kind = r.int()?;
        let cid = if ver == 2 {
            cid_from_int(T, v, r.next()?)?
        } else {
            let c = cid_read(T, v, r.next()?)?;
            if ver >= 4 {
                kind = r.int()?;
            }
            c
        };
        (kind, cid)
    };
    Ok(Node::list(vec![
        atom(4),
        atom(kind.min(4)),
        cid_write(&cid),
        atom(kind),
    ]))
}

struct Font {
    kind: i64,
    h: i64,
    w: i64,
    esc: i64,
    ori: i64,
    weight: i64,
    b: [i64; 8],
    name: Node,
    cid: Cid,
    mask: i64,
    flag: i64,
    scale: i64,
    x60: i64,
}

fn font_read(v: &Node) -> Result<Font, Refusal> {
    const T: &str = "Font";
    let (ver, mut r) = Cursor::new(T, v)?;
    let mut m = Font {
        kind: 1,
        h: 0,
        w: 0,
        esc: 0,
        ori: 0,
        weight: 0,
        b: [0; 8],
        name: Node::str(""),
        cid: cid_null(),
        mask: 0,
        flag: 1,
        scale: 100,
        x60: 0,
    };
    if ver < 6 {
        m.kind = 0;
        m.h = r.int()?;
        m.w = r.int()?;
        m.esc = r.int()?;
        m.ori = r.int()?;
        m.weight = r.int()?;
        for b in &mut m.b {
            *b = r.int()?;
        }
        m.name = r.next()?.clone();
        if ver > 1 {
            m.kind = r.int()?;
            m.cid = if ver < 5 {
                cid_from_int(T, v, r.next()?)?
            } else {
                cid_read(T, v, r.next()?)?
            };
            m.mask = r.int()?;
            if ver > 2 {
                m.flag = r.int()?;
            }
        }
        if ver < 4 && m.h != 0 {
            m.h = (-m.h * 720) / 96;
            m.w = 0;
        }
    } else {
        m.kind = r.int()?;
        m.mask = r.int()?;
        if m.kind == 0 {
            m.h = r.int()?;
            m.w = r.int()?;
            m.esc = r.int()?;
            m.ori = r.int()?;
            m.weight = r.int()?;
            for b in &mut m.b {
                *b = r.int()?;
            }
            m.name = r.next()?.clone();
        } else {
            if m.kind == 1 || m.kind == 2 {
                m.cid = cid_read(T, v, r.next()?)?;
            }
            let k = m.mask;
            if k & 2 != 0 {
                m.h = r.int()?;
            }
            if k & 4 != 0 {
                m.weight = r.int()?;
            }
            for (bit, j) in [(8, 0), (0x10, 1), (0x20, 2), (0x40, 3)] {
                if k & bit != 0 {
                    let x = r.int()?;
                    if let Some(b) = m.b.get_mut(j) {
                        *b = x;
                    }
                }
            }
            if k & 1 != 0 {
                m.name = r.next()?.clone();
            }
            if k & 0x80 != 0 {
                m.esc = r.int()?;
            }
        }
        m.flag = r.int()?;
        if ver > 6 {
            m.scale = r.int()?;
        }
        if ver > 7 && (m.kind == 0 || m.mask & 0x100 != 0) {
            m.x60 = r.int()?;
        }
    }
    m.weight = if m.weight > 400 { 700 } else { 400 };
    Ok(m)
}

fn font_write(m: &Font) -> Node {
    let mut out = vec![atom(8), atom(m.kind), atom(m.mask)];
    let k = m.mask;
    if m.kind == 0 {
        out.extend([m.h, m.w, m.esc, m.ori, m.weight].map(atom));
        out.extend(m.b.map(atom));
        out.push(m.name.clone());
    } else {
        if m.kind == 1 || m.kind == 2 {
            out.push(cid_write(&m.cid));
        }
        if k & 2 != 0 {
            out.push(atom(m.h));
        }
        if k & 4 != 0 {
            out.push(atom(m.weight));
        }
        for (bit, j) in [(8, 0), (0x10, 1), (0x20, 2), (0x40, 3)] {
            if k & bit != 0 {
                out.push(atom(m.b.get(j).copied().unwrap_or(0)));
            }
        }
        if k & 1 != 0 {
            out.push(m.name.clone());
        }
        if k & 0x80 != 0 {
            out.push(atom(m.esc));
        }
    }
    out.push(atom(m.flag));
    out.push(atom(m.scale));
    if m.kind == 0 || k & 0x100 != 0 {
        out.push(atom(m.x60));
    }
    Node::list(out)
}

fn font(v: &Node) -> Result<Node, Refusal> {
    Ok(font_write(&font_read(v)?))
}

pub(crate) fn font_xml_roundtrip(v: &Node) -> Result<Node, Refusal> {
    let m = font_read(v)?;
    if m.kind != 0 {
        return Ok(font_write(&m));
    }
    let n = Font {
        kind: 0,
        mask: 0x23f,
        h: m.h,
        w: 0,
        esc: 0,
        ori: 0,
        weight: m.weight,
        b: [m.b[0], m.b[1], m.b[2], 0, 0, 0, 0, 0],
        name: m.name,
        cid: cid_null(),
        flag: 1,
        scale: m.scale,
        x60: 0,
    };
    Ok(font_write(&n))
}

fn border(v: &Node, line: bool) -> Result<Node, Refusal> {
    let ty: &'static str = if line { "Line" } else { "Border" };
    let (ver, mut r) = Cursor::new(ty, v)?;
    let kind = r.int()?;
    let cid = if ver < 3 {
        cid_from_int(ty, v, r.next()?)?
    } else {
        cid_read(ty, v, r.next()?)?
    };
    let width = r.int()?;
    if ver == 0 {
        r.next()?;
    }
    let style = r.int()?;
    let x = r.int()?;
    let cls = if ver < 2 || kind != 0 {
        Node::atom(CONTROLS_BORDERS_CLASS)
    } else {
        r.next()?.clone()
    };
    let y = if line && ver > 3 { r.int()? } else { 0 };
    let mut out = vec![
        atom(if line { 4 } else { 3 }),
        atom(kind),
        cid_write(&cid),
        atom(width),
        atom(style),
        atom(x),
    ];
    if kind == 0 {
        out.push(cls);
    }
    if line {
        out.push(atom(y));
    }
    Ok(Node::list(out))
}

fn picture(v: &Node) -> Result<Node, Refusal> {
    const T: &str = "Picture";
    let (ver, mut r) = Cursor::new(T, v)?;
    let kind = r.int()?;
    let cid = if ver < 3 {
        cid_from_int(T, v, r.next()?)?
    } else {
        cid_read(T, v, r.next()?)?
    };
    let url = r.next()?.clone();
    let i60 = r.next()?.clone();
    let i64_ = r.next()?.clone();
    let b68 = r.next()?.clone();
    let data = if kind == 3 {
        if ver < 2 {
            return Err(refuse(T, v));
        }
        Some(r.next()?.clone())
    } else {
        None
    };
    let mut b69 = 0;
    let mut ext = None;
    if ver != 0 {
        b69 = r.int()?;
        if b69 != 0 {
            r.next()?;
            r.next()?;
            ext = Some((r.next()?.clone(), r.next()?.clone()));
        }
    }
    let s78 = if ver > 3 {
        r.next()?.clone()
    } else {
        Node::str("")
    };
    let mut out = vec![atom(4), atom(kind), cid_write(&cid), url, i60, i64_, b68];
    if let Some(d) = data {
        out.push(d);
    }
    out.push(atom(b69));
    if let Some((a, b)) = ext {
        out.extend([atom(0), atom(0), a, b]);
    }
    out.push(s78);
    Ok(Node::list(out))
}

fn local_string(v: &Node) -> Result<Node, Refusal> {
    const T: &str = "LocalWString";
    let l = v.as_list().ok_or_else(|| refuse(T, v))?;
    let n = usize::try_from(int_of(T, v, l.get(1))?).map_err(|_| refuse(T, v))?;
    let mut out = Vec::new();
    for e in l.get(2..).unwrap_or_default().iter().take(n) {
        let el = e.as_list().ok_or_else(|| refuse(T, v))?;
        let text = el.get(1).ok_or_else(|| refuse(T, v))?;
        if text.text() != Some("") {
            out.push(e.clone());
        }
    }
    let mut r = vec![atom(1), atom(i64::try_from(out.len()).unwrap_or(0))];
    r.extend(out);
    Ok(Node::list(r))
}

fn shortcut(v: &Node) -> Result<Node, Refusal> {
    const T: &str = "ShortCut";
    let l = v.as_list().ok_or_else(|| refuse(T, v))?;
    Ok(Node::list(vec![
        atom(0),
        l.get(1).ok_or_else(|| refuse(T, v))?.clone(),
        l.get(2).ok_or_else(|| refuse(T, v))?.clone(),
    ]))
}

fn data_path(v: &Node) -> Result<Node, Refusal> {
    const T: &str = "DataPath";
    let l = v.as_list().ok_or_else(|| refuse(T, v))?;
    let n = usize::try_from(int_of(T, v, l.first())?).map_err(|_| refuse(T, v))?;
    let mut out = vec![atom(i64::try_from(n).unwrap_or(0))];
    for s in l.get(1..).unwrap_or_default().iter().take(n) {
        out.push(cid_write(&cid_read(T, v, s)?));
    }
    Ok(Node::list(out))
}

fn guid_key(n: &Node) -> String {
    n.text().unwrap_or("").to_lowercase()
}

fn guid_set(v: &Node) -> Result<Node, Refusal> {
    const T: &str = "GuidSet";
    let l = v.as_list().ok_or_else(|| refuse(T, v))?;
    let n = usize::try_from(int_of(T, v, l.first())?).map_err(|_| refuse(T, v))?;
    let mut gs: Vec<&Node> = l.get(1..).unwrap_or_default().iter().take(n).collect();
    gs.sort_by_key(|g| guid_key(g));
    gs.dedup();
    let mut out = vec![atom(i64::try_from(gs.len()).unwrap_or(0))];
    out.extend(gs.into_iter().cloned());
    Ok(Node::list(out))
}

type Handlers = Vec<(Node, Node)>;

pub(crate) fn events(v: &Node) -> Result<Node, Refusal> {
    let l = v.as_list().ok_or(Refusal::Events)?;
    let bad = || Refusal::Events;
    let n = usize::try_from(l.first().and_then(Node::as_i64).ok_or(Refusal::Events)?)
        .map_err(|_| bad())?;
    let mut simple: Vec<(&Node, &Node)> = Vec::with_capacity(n);
    for i in 0..n {
        simple.push((
            l.get(1 + 2 * i).ok_or(bad())?,
            l.get(2 + 2 * i).ok_or(bad())?,
        ));
    }
    let rest = l.get(1 + 2 * n..).unwrap_or_default();
    let mut ev: Vec<(&Node, Handlers)> = Vec::new();
    for (g, name) in &simple {
        if !ev.iter().any(|(k, _)| k.text() == g.text()) {
            ev.push((g, vec![((*name).clone(), atom(0))]));
        }
    }
    let mut multi: Vec<(&Node, Vec<Handlers>)> = Vec::new();
    if !rest.is_empty() {
        let int_at = |p: usize| -> Result<usize, Refusal> {
            usize::try_from(rest.get(p).and_then(Node::as_i64).ok_or(bad())?).map_err(|_| bad())
        };
        let mut p = 2;
        let m = int_at(1)?;
        let mut mlist: Vec<(&Node, &[Node])> = Vec::new();
        for _ in 0..m {
            let g = rest.get(p).ok_or(bad())?;
            let k = int_at(p + 1)?;
            mlist.push((g, rest.get(p + 2..p + 2 + k).ok_or(bad())?));
            p += 2 + k;
        }
        let pairs = |tail: &[Node]| -> Vec<(Node, Node)> {
            tail.as_chunks::<2>()
                .0
                .iter()
                .map(|[a, b]| (a.clone(), b.clone()))
                .collect()
        };
        for (_, name) in simple.iter().take(n) {
            let g = rest.get(p).ok_or(bad())?;
            let flag = rest.get(p + 1).ok_or(bad())?;
            let cnt = int_at(p + 2)?;
            p += 3;
            let extra = cnt.saturating_sub(1) * 2;
            let tail = rest.get(p..p + extra).ok_or(bad())?;
            p += extra;
            let ent = if cnt > 0 {
                let mut e = vec![((*name).clone(), flag.clone())];
                e.extend(pairs(tail));
                e
            } else {
                Vec::new()
            };
            match ev.iter_mut().find(|(k, _)| k.text() == g.text()) {
                Some(slot) => slot.1 = ent,
                None => ev.push((g, ent)),
            }
        }
        for (g, names) in mlist {
            let k = int_at(p + 1)?;
            p += 2;
            let mut vecs = Vec::new();
            for j in 0..k {
                let flag = rest.get(p).ok_or(bad())?;
                let cnt = int_at(p + 1)?;
                p += 2;
                let extra = cnt.saturating_sub(1) * 2;
                let tail = rest.get(p..p + extra).ok_or(bad())?;
                p += extra;
                let mut e = vec![(names.get(j).ok_or(bad())?.clone(), flag.clone())];
                e.extend(pairs(tail));
                vecs.push(e);
            }
            match multi.iter_mut().find(|(k, _)| k.text() == g.text()) {
                Some(slot) => slot.1 = vecs,
                None => multi.push((g, vecs)),
            }
        }
    }
    ev.sort_by_key(|(g, _)| guid_key(g));
    multi.sort_by_key(|(g, _)| guid_key(g));
    let count = |n: usize| atom(i64::try_from(n).unwrap_or(0));
    let mut out = vec![count(ev.len())];
    for (g, e) in &ev {
        out.push((*g).clone());
        out.push(
            e.first()
                .map_or_else(|| Node::str(""), |(name, _)| name.clone()),
        );
    }
    out.push(atom(1));
    out.push(count(multi.len()));
    for (g, vecs) in &multi {
        out.push((*g).clone());
        out.push(count(vecs.len()));
        for vec in vecs {
            out.push(
                vec.first()
                    .map_or_else(|| Node::str(""), |(name, _)| name.clone()),
            );
        }
    }
    for (g, e) in &ev {
        out.push((*g).clone());
        out.push(e.first().map_or_else(|| atom(0), |(_, flag)| flag.clone()));
        out.push(count(e.len()));
        for (name, fl) in e.iter().skip(1) {
            out.push(name.clone());
            out.push(fl.clone());
        }
    }
    for (g, vecs) in &multi {
        out.push((*g).clone());
        out.push(count(vecs.len()));
        for vec in vecs {
            out.push(
                vec.first()
                    .map_or_else(|| atom(0), |(_, flag)| flag.clone()),
            );
            out.push(count(vec.len()));
            for (name, fl) in vec.iter().skip(1) {
                out.push(name.clone());
                out.push(fl.clone());
            }
        }
    }
    Ok(Node::list(out))
}

pub(crate) fn choice_list_items(v: &Node, empty_picture: &Node) -> Node {
    match v.as_list() {
        None => v.clone(),
        Some(l) => {
            if l.len() == 3
                && l.get(1).and_then(Node::text) == Some(CHOICE_LIST_ITEM)
                && let Some(inner) = l.get(2).and_then(Node::as_list)
                && inner.len() == 6
                && inner.first().and_then(Node::text) == Some("0")
            {
                let mut n = vec![atom(1)];
                n.extend(
                    inner
                        .iter()
                        .skip(1)
                        .map(|x| choice_list_items(x, empty_picture)),
                );
                n.push(empty_picture.clone());
                return Node::list(vec![
                    l.first().cloned().unwrap_or_else(Node::empty),
                    l.get(1).cloned().unwrap_or_else(Node::empty),
                    Node::list(n),
                ]);
            }
            Node::list(
                l.iter()
                    .map(|x| choice_list_items(x, empty_picture))
                    .collect(),
            )
        }
    }
}

pub(crate) fn upgrade(ty: &str, v: &Node, empty_picture: &Node) -> Result<Node, Refusal> {
    match ty {
        "Color" => color(v),
        "Font" => font(v),
        "Border" => border(v, false),
        "Line" => border(v, true),
        "Picture" => picture(v),
        "CompositeID" => Ok(cid_write(&cid_read("CompositeID", v, v)?)),
        "LocalWString" => local_string(v),
        "ShortCut" => shortcut(v),
        "DataPath" => data_path(v),
        "Events" => events(v),
        "GuidSet" => guid_set(v),
        "Plain" => Ok(choice_list_items(v, empty_picture)),
        "Raw" => Ok(v.clone()),
        _ => Err(Refusal::Shape("value type")),
    }
}
