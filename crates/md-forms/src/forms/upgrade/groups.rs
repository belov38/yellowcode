use brace::Node;

use super::{Cx, Refusal};

struct R<'a> {
    l: &'a [Node],
    i: usize,
    what: &'static str,
    kind: &'a str,
    ver: i64,
}

impl<'a> R<'a> {
    fn new(what: &'static str, kind: &'a str, v: &'a Node) -> Result<R<'a>, Refusal> {
        let l = v.as_list().ok_or(Refusal::Shape(what))?;
        let ver = l
            .first()
            .and_then(Node::as_i64)
            .ok_or(Refusal::Shape(what))?;
        Ok(R {
            l,
            i: 1,
            what,
            kind,
            ver,
        })
    }

    fn refuse(&self) -> Refusal {
        Refusal::NoRuleKind {
            what: self.what,
            kind: self.kind.to_string(),
            version: self.ver.to_string(),
        }
    }

    fn get(&mut self) -> Result<Node, Refusal> {
        let n = self.l.get(self.i).ok_or_else(|| self.refuse())?;
        self.i += 1;
        Ok(n.clone())
    }

    fn int(&mut self) -> Result<i64, Refusal> {
        let n = self.get()?;
        n.as_i64().ok_or_else(|| self.refuse())
    }

    fn bool(&mut self) -> Result<bool, Refusal> {
        Ok(self.get()?.text() != Some("0"))
    }

    fn skip(&mut self) -> Result<(), Refusal> {
        self.get().map(|_| ())
    }
}

fn i(n: i64) -> Node {
    Node::int(n)
}

fn b(x: bool) -> Node {
    Node::int(i64::from(x))
}

fn dflt(cx: &mut Cx<'_>, name: &str) -> Result<Node, Refusal> {
    let text = cx
        .tables
        .defaults
        .get(name)
        .map_or("0", String::as_str)
        .to_string();
    cx.parse_default(&text)
}

fn enum2(x: bool) -> i64 {
    if x { 2 } else { 0 }
}

fn usual_group(v: &Node, cx: &mut Cx<'_>) -> Result<Node, Refusal> {
    let mut r = R::new("group-kind", "5", v)?;
    let ver = r.ver;
    let (mut f4c, mut f50, mut f54, mut f58, mut f5c, mut f60, mut f64, mut f68, mut f6c) =
        (0i64, 0i64, 0i64, 3i64, 3i64, 0i64, 0i64, 3i64, 0i64);
    let (mut f74, mut f78) = (2i64, 2i64);
    let mut f80 = dflt(cx, "EmptyRef")?;
    let mut f98 = Node::str("");
    let mut fb0 = dflt(cx, "EmptyLocalString")?;
    let mut ff0 = dflt(cx, "EmptyLocalString")?;
    let mut f130 = 0i64;
    let mut f138 = Node::atom("0");
    let mut f140 = dflt(cx, "AutoColor")?;
    let mut f15c = dflt(cx, "AutoColor")?;
    let (mut f178, mut f179, mut f17a, mut f17c, mut f180, mut f184, mut f188, mut f18c, mut f190) =
        (0i64, 1i64, 1i64, 2i64, 0i64, 0i64, 0i64, 0i64, 0i64);
    let mut f198 = dflt(cx, "EmptyPicture")?;
    let mut f20 = dflt(cx, "EmptyEvents")?;
    let mut f228 = 2i64;
    if ver < 7 {
        if ver > 1 {
            r.skip()?;
        }
        r.skip()?;
    }
    let x = r.int()?;
    let mut f48 = if ver >= 8 { x } else { i64::from(1 < x) };
    if ver > 9 {
        f60 = r.int()?;
    }
    let mut f70 = r.int()?;
    if ver > 10 {
        f78 = enum2(r.bool()?);
    }
    if (5..=8).contains(&ver) {
        r.skip()?;
    }
    if (1..=5).contains(&ver) {
        r.skip()?;
    }
    if ver > 2 {
        f80 = r.get()?;
    }
    if ver > 3 {
        fb0 = r.get()?;
    }
    if ver > 11 {
        f138 = r.get()?;
        f98 = r.get()?;
    }
    if ver > 12 {
        f140 = cx.upgrade_value("Color", &r.get()?, "UsualGroup")?;
    }
    if ver > 13 {
        f68 = r.int()?;
        f6c = r.int()?;
    }
    if ver > 14 {
        f178 = r.int()?;
    }
    if ver > 15 {
        f179 = r.int()?;
    }
    if ver > 16 {
        ff0 = r.get()?;
    }
    if ver > 17 {
        f50 = r.int()?;
        f54 = r.int()?;
    }
    if ver > 18 {
        f58 = r.int()?;
        f5c = r.int()?;
    }
    if ver > 19 {
        f74 = r.int()?;
    }
    if ver > 20 {
        f4c = r.int()?;
    }
    if ver > 21 {
        f17a = r.int()?;
    }
    if ver > 22 {
        f48 = r.int()?;
    }
    if ver > 24 {
        f15c = cx.upgrade_value("Color", &r.get()?, "UsualGroup")?;
    }
    if ver > 25 {
        f68 = r.int()?;
    }
    if ver > 26 {
        f17c = r.int()?;
        f180 = r.int()?;
    }
    if ver > 27 {
        f48 = r.int()?;
    }
    if ver > 28 {
        f68 = r.int()?;
    }
    if ver > 29 {
        f184 = r.int()?;
        f188 = r.int()?;
        f190 = r.int()?;
        f198 = cx.upgrade_value("Picture", &r.get()?, "UsualGroup")?;
        f20 = r.get()?;
    }
    if ver > 30 {
        f18c = r.int()?;
    }
    if ver > 31 {
        f70 = r.int()?;
    }
    if ver > 32 {
        f48 = r.int()?;
    }
    if ver > 33 {
        f228 = i64::from(r.bool()?);
    }
    if ver > 34 {
        f228 = r.int()?;
    }
    if ver > 35 {
        let x = r.int()?;
        f64 = if x == 4 { 0 } else { x };
    }
    if ver > 36 {
        f130 = r.int()?;
    }
    if ver > 37 {
        f78 = r.int()?.min(2);
    }
    let (g, h, a) = (f48, f68, f70);
    Ok(Node::list(vec![
        i(38),
        i(if g > 1 { 1 } else { g }),
        i(f60),
        i(if a == 4 { 2 } else { a }),
        b(f78 != 0),
        f80,
        fb0,
        f138,
        f98,
        f140,
        i(if h > 1 { 1 } else { h }),
        i(f6c),
        i(f178),
        i(f179),
        ff0,
        i(f50),
        i(f54),
        i(f58),
        i(f5c),
        i(f74),
        i(f4c),
        i(f17a),
        i(if g > 2 { 1 } else { g }),
        f15c,
        i(if h > 2 { 0 } else { h }),
        i(f17c),
        i(f180),
        i(if g == 4 || g == 5 { 2 } else { g }),
        i(h),
        i(f184),
        i(f188),
        i(f190),
        f198,
        f20,
        i(f18c),
        i(a),
        i(g),
        b(f228 == 1),
        i(f228),
        i(f64),
        i(f130),
        i(f78),
    ]))
}

fn page(v: &Node, cx: &mut Cx<'_>) -> Result<Node, Refusal> {
    let mut r = R::new("group-kind", "4", v)?;
    let ver = r.ver;
    let (mut fbc, mut fc0, mut fc4, mut fc8, mut fcc, mut fd0, mut fd4) =
        (0i64, 0i64, 0i64, 3i64, 3i64, 0i64, 0i64);
    let mut fd8 = dflt(cx, "EmptyRef")?;
    let mut ff0 = Node::str("");
    let mut f108 = dflt(cx, "EmptyLocalString")?;
    let mut f148 = Node::atom("0");
    let mut f150 = 2i64;
    let mut f154 = dflt(cx, "AutoColor")?;
    let mut f170 = dflt(cx, "AutoColor")?;
    let mut f18c = dflt(cx, "AutoFont")?;
    let mut f214 = 2i64;
    if ver < 6 {
        if ver > 1 {
            r.skip()?;
        }
        r.skip()?;
    }
    let f28 = cx.upgrade_value("Picture", &r.get()?, "Page")?;
    let x = r.int()?;
    let mut fb8 = if ver < 7 { i64::from(1 < x) } else { x };
    if ver > 7 {
        fd0 = r.int()?;
    }
    if (1..=4).contains(&ver) {
        r.skip()?;
    }
    if ver > 2 {
        fd8 = r.get()?;
    }
    if ver > 3 {
        f108 = r.get()?;
    }
    if ver > 8 {
        f150 = enum2(r.bool()?);
    }
    if ver > 9 {
        f148 = r.get()?;
        ff0 = r.get()?;
    }
    if ver > 10 {
        f154 = cx.upgrade_value("Color", &r.get()?, "Page")?;
    }
    if ver > 11 {
        fc0 = r.int()?;
        fc4 = r.int()?;
    }
    if ver > 12 {
        fc8 = r.int()?;
        fcc = r.int()?;
    }
    if ver > 13 {
        fbc = r.int()?;
    }
    if ver > 14 {
        f214 = i64::from(r.bool()?);
    }
    if ver > 15 {
        fb8 = r.int()?;
    }
    if ver > 16 {
        fb8 = r.int()?;
    }
    if ver > 17 {
        f170 = cx.upgrade_value("Color", &r.get()?, "Page")?;
        f18c = cx.upgrade_value("Font", &r.get()?, "Page")?;
    }
    if ver > 18 {
        fb8 = r.int()?;
    }
    if ver > 19 {
        f214 = r.int()?;
    }
    if ver > 20 {
        let x = r.int()?;
        fd4 = if x == 4 { 0 } else { x };
    }
    if ver > 21 {
        f150 = r.int()?.min(2);
    }
    let g = fb8;
    Ok(Node::list(vec![
        i(22),
        f28,
        i(if g > 1 { 1 } else { g }),
        i(fd0),
        fd8,
        f108,
        b(f150 != 0),
        f148,
        ff0,
        f154,
        i(fc0),
        i(fc4),
        i(fc8),
        i(fcc),
        i(fbc),
        b(f214 == 1),
        i(if g > 2 { 1 } else { g }),
        i(if g == 4 || g == 5 { 0 } else { g }),
        f170,
        f18c,
        i(g),
        i(f214),
        i(fd4),
        i(f150),
    ]))
}

fn pages(v: &Node) -> Result<Node, Refusal> {
    let mut r = R::new("group-kind", "3", v)?;
    let ver = r.ver;
    let (mut f48, mut f4c, mut f50) = (1i64, 2i64, 0i64);
    if ver > 0 {
        f48 = r.int()?;
    }
    if ver == 2 {
        f4c = r.int()?;
        f50 = r.int()?;
    }
    let f20 = r.get()?;
    if ver > 2 {
        f4c = r.int()?;
        f50 = r.int()?;
    }
    if ver > 3 {
        f48 = r.int()?;
    }
    let g = f48;
    Ok(Node::list(vec![
        i(4),
        i(if g > 5 { 1 } else { g }),
        f20,
        i(f4c),
        i(f50),
        i(g),
    ]))
}

fn popup(v: &Node, cx: &mut Cx<'_>) -> Result<Node, Refusal> {
    let mut r = R::new("group-kind", "1", v)?;
    let ver = r.ver;
    let mut f28 = 3i64;
    let mut fc0 = dflt(cx, "EmptyRef")?;
    let (mut fd8, mut fdc, mut fe0) = (2i64, 0i64, 0i64);
    let mut fe4 = dflt(cx, "AutoColor")?;
    let mut f100 = dflt(cx, "AutoColor")?;
    let mut f11c = 1i64;
    if ver < 3 {
        r.skip()?;
    }
    let f30 = cx.upgrade_value("Picture", &r.get()?, "Popup")?;
    if ver < 5 {
        r.skip()?;
    }
    if ver < 2 {
        r.skip()?;
    }
    if ver > 0 {
        fc0 = r.get()?;
    }
    if ver > 3 {
        fd8 = r.int()?;
    }
    if ver > 5 {
        f28 = r.int()?;
    }
    if ver > 6 {
        fdc = r.int()?;
        fe0 = r.int()?;
        fe4 = cx.upgrade_value("Color", &r.get()?, "Popup")?;
        f100 = cx.upgrade_value("Color", &r.get()?, "Popup")?;
    }
    if ver > 7 {
        f11c = r.int()?;
    }
    Ok(Node::list(vec![
        i(8),
        f30,
        fc0,
        i(fd8),
        i(f28),
        i(fdc),
        i(fe0),
        fe4,
        f100,
        i(f11c),
    ]))
}

fn column_group(v: &Node, cx: &mut Cx<'_>) -> Result<Node, Refusal> {
    let mut r = R::new("group-kind", "2", v)?;
    let ver = r.ver;
    let (mut f2c, mut f30, mut f34) = (2i64, 0i64, 3i64);
    let mut f38 = dflt(cx, "EmptyPicture")?;
    let mut fc8 = dflt(cx, "AutoColor")?;
    let mut fe8 = dflt(cx, "EmptyRef")?;
    let mut f118 = Node::atom("0");
    let mut f100 = Node::str("");
    let mut f120 = dflt(cx, "EmptyLocalString")?;
    let (mut f160, mut f164, mut f168, mut f16c) = (0i64, 1i64, 2i64, 2i64);
    let f28 = r.int()?;
    if ver > 0 {
        f2c = enum2(r.bool()?);
        f30 = r.int()?;
        f34 = r.int()?;
        f38 = cx.upgrade_value("Picture", &r.get()?, "ColumnGroup")?;
        fc8 = cx.upgrade_value("Color", &r.get()?, "ColumnGroup")?;
        fe8 = r.get()?;
        f118 = r.get()?;
        f100 = r.get()?;
        f120 = r.get()?;
    }
    if ver > 1 {
        f160 = r.int()?;
    }
    if ver > 2 {
        f164 = r.int()?;
        f168 = r.int()?;
    }
    if ver > 3 {
        f16c = r.int()?;
    }
    if ver > 4 {
        f2c = r.int()?.min(2);
    }
    Ok(Node::list(vec![
        i(5),
        i(f28),
        b(f2c != 0),
        i(f30),
        i(f34),
        f38,
        fc8,
        fe8,
        f118,
        f100,
        f120,
        i(f160),
        i(f164),
        i(f168),
        i(f16c),
        i(f2c),
    ]))
}

fn command_bar(v: &Node, cx: &mut Cx<'_>) -> Result<Node, Refusal> {
    let mut r = R::new("group-kind", "0", v)?;
    let ver = r.ver;
    let mut f2c = dflt(cx, "EmptyRef")?;
    let mut f44 = 0i64;
    let f28 = r.int()?;
    if ver > 0 {
        f2c = r.get()?;
    }
    if ver > 1 {
        f44 = r.int()?;
    }
    Ok(Node::list(vec![i(2), i(f28), f2c, i(f44)]))
}

fn button_group(v: &Node) -> Result<Node, Refusal> {
    let mut r = R::new("group-kind", "6", v)?;
    let ver = r.ver;
    let (mut f40, mut f44) = (2i64, 0i64);
    let f28 = r.get()?;
    if ver > 0 {
        f40 = r.int()?;
        if ver > 1 {
            f44 = r.int()?;
        }
    }
    Ok(Node::list(vec![i(2), f28, i(f40), i(f44)]))
}

fn context_menu(v: &Node) -> Result<Node, Refusal> {
    let mut r = R::new("group-kind", "8", v)?;
    let mut f28 = 1i64;
    if r.ver < 1 {
        r.skip()?;
    } else {
        f28 = r.int()?;
    }
    Ok(Node::list(vec![i(1), i(f28)]))
}

fn auto_command_bar(v: &Node) -> Result<Node, Refusal> {
    let mut r = R::new("group-kind", "9", v)?;
    let mut f30 = 0i64;
    let f2c = r.int()?;
    let f28 = r.int()?;
    if r.ver > 0 {
        f30 = r.int()?;
    }
    Ok(Node::list(vec![i(1), i(f2c), i(f28), i(f30)]))
}

pub(crate) fn group_kind(kind: &str, v: &Node, cx: &mut Cx<'_>) -> Result<Node, Refusal> {
    match kind {
        "5" => usual_group(v, cx),
        "4" => page(v, cx),
        "3" => pages(v),
        "1" => popup(v, cx),
        "2" => column_group(v, cx),
        "0" => command_bar(v, cx),
        "6" => button_group(v),
        "8" => context_menu(v),
        "9" => auto_command_bar(v),
        _ => Err(Refusal::NoRuleKind {
            what: "group-kind",
            kind: kind.into(),
            version: v.at(0).and_then(Node::text).unwrap_or("?").into(),
        }),
    }
}

fn label_decoration(v: &Node, cx: &mut Cx<'_>) -> Result<Node, Refusal> {
    let mut r = R::new("decoration-kind", "0", v)?;
    let ver = r.ver;
    let (mut f50, mut f54) = (3i64, 0i64);
    let mut f58 = dflt(cx, "AutoColor")?;
    let mut f74 = dflt(cx, "AutoColor")?;
    let mut f90 = Node::atom("0");
    if ver < 4 {
        r.skip()?;
    }
    let f48 = r.int()?;
    let f4c = r.int()?;
    if ver > 0 {
        f50 = r.int()?;
    }
    if ver > 2 {
        f54 = r.int()?;
    }
    let f20 = r.get()?;
    if ver > 4 {
        f58 = cx.upgrade_value("Color", &r.get()?, "LabelDecoration")?;
        f74 = cx.upgrade_value("Color", &r.get()?, "LabelDecoration")?;
        f90 = cx.upgrade_value("Border", &r.get()?, "LabelDecoration")?;
    }
    Ok(Node::list(vec![
        i(5),
        i(f48),
        i(f4c),
        i(f50),
        i(f54),
        f20,
        f58,
        f74,
        f90,
    ]))
}

fn picture_decoration(v: &Node, owner_color: &Node, cx: &mut Cx<'_>) -> Result<Node, Refusal> {
    let mut r = R::new("decoration-kind", "1", v)?;
    let ver = r.ver;
    let (mut fdc, mut fe0, mut fe4) = (0i64, 0i64, 100i64);
    let mut fe8 = dflt(cx, "EmptyLocalString")?;
    let mut f128 = dflt(cx, "AutoColor")?;
    let mut f144 = Node::atom("0");
    let (mut f17c, mut f17d, mut f184) = (0i64, 0i64, 0i64);
    let f48 = cx.upgrade_value("Picture", &r.get()?, "PictureDecoration")?;
    let fd8 = r.int()?;
    if ver > 1 {
        fdc = r.int()?;
        fe0 = r.int()?;
        fe8 = r.get()?;
        f128 = cx.upgrade_value("Color", &r.get()?, "PictureDecoration")?;
        f144 = cx.upgrade_value("Border", &r.get()?, "PictureDecoration")?;
    }
    if ver > 0 {
        f17c = r.int()?;
        f17d = r.int()?;
    }
    let f20 = r.get()?;
    let f180 = if ver >= 3 { r.int()? } else { 0 };
    if ver > 3 {
        fe4 = r.int()?;
    }
    if ver > 4 {
        f184 = r.int()?;
    }
    let oc = if ver > 5 {
        cx.upgrade_value("Color", &r.get()?, "PictureDecoration")?
    } else {
        owner_color.clone()
    };
    Ok(Node::list(vec![
        i(6),
        f48,
        i(fd8),
        i(fdc),
        i(fe0),
        fe8,
        f128,
        f144,
        i(f17c),
        i(f17d),
        f20,
        i(f180),
        i(fe4),
        i(f184),
        oc,
    ]))
}

pub(crate) fn decoration_kind(
    kind: &str,
    v: &Node,
    owner_color: &Node,
    cx: &mut Cx<'_>,
) -> Result<Node, Refusal> {
    match kind {
        "0" => label_decoration(v, cx),
        "1" => picture_decoration(v, owner_color, cx),
        _ => Err(Refusal::NoRuleKind {
            what: "decoration-kind",
            kind: kind.into(),
            version: v.at(0).and_then(Node::text).unwrap_or("?").into(),
        }),
    }
}
