use super::tables::{JsonRef, PropRef};
use brace::{Guid, Node};

use super::codecs_a::{Seg, lstr_items, lstr_kids, parent, path_segs, value_at};
use super::ctx::md_name;
use super::fe::E;
use super::py::{
    Py, Raised, b64_decode, py_index, py_int, py_item, py_len, py_seq, py_slice, py_str, tenth_g,
};
use super::section_codecs::{ZERO, flag, schema};
use super::write::{Emit, FormCtx, Src};
use crate::model::TypedValue;
use crate::objects::codecs::{Ctx, color_str};
use crate::objects::types::design_time_ref;

const T_DESTIME: &str = "0e704aa2-07bd-48b9-8223-a0212c4d5fc2";
const T_DTREF: &str = "5c14e26f-099b-4d37-84a6-b433d87400da";
const T_FIXARR: &str = "4500381b-db30-4a10-9db4-990038032acf";
fn attr(k: &str, v: impl Into<String>) -> (String, String) {
    (k.to_string(), v.into())
}

pub(crate) fn xsi_type(t: &str) -> Vec<(String, String)> {
    vec![attr("xsi:type", t)]
}

pub(crate) fn at(l: &[Node], i: Option<i64>) -> Py<&Node> {
    py_index(l, i.ok_or(Raised)?)
}

pub(crate) fn text(n: &Node) -> String {
    py_str(n).into_owned()
}

pub(crate) fn str_item(v: &Node, i: i64) -> Py<String> {
    Ok(text(py_item(v, i)?.as_ref()))
}

pub(crate) fn int_of(v: &Node) -> Py<i64> {
    py_int(&py_str(v))
}

pub(crate) fn is_default(ent: PropRef<'_>, v: &Node) -> bool {
    let bv = brace::to_compact(v);
    ent.default.iter().any(|d| d.as_str() == Some(bv.as_str()))
}

pub(crate) fn learned_path<'a>(ent: Option<PropRef<'a>>) -> Option<Py<Vec<Seg<'a>>>> {
    let ent = ent?;
    ent.raw.get("path")?;
    Some(path_segs(ent).ok_or(Raised))
}

pub(crate) fn run(fc: &mut FormCtx<'_>, r: Py<Option<Emit>>) -> Option<Emit> {
    match r {
        Ok(r) => r,
        Err(Raised) => {
            fc.raise();
            Some(Emit::Cannot)
        }
    }
}

const PIC_MAGIC: &[(&[u8], &str)] = &[
    (b"\x89PNG", "png"),
    (b"BM", "bmp"),
    (b"GIF8", "gif"),
    (b"PK\x03\x04", "zip"),
    (b"<svg", "svg"),
    (b"<?xm", "svg"),
    (b"\xff\xd8", "jpg"),
    (b"\x00\x00\x01\x00", "ico"),
    (b"\xd7\xcd\xc6\x9a", "wmf"),
    (b"\x01\x00\x00\x00", "emf"),
];

pub(crate) fn picture_ext(b: &[u8]) -> &'static str {
    PIC_MAGIC
        .iter()
        .find(|(m, _)| b.starts_with(m))
        .map_or("bin", |(_, e)| e)
}

fn std_pict(fc: &FormCtx<'_>, key: &str) -> Py<Option<String>> {
    let s = schema()?;
    if let Some(p) = s
        .std_picture(key)
        .or_else(|| s.ui().std_pictures.get(key).map(String::as_str))
    {
        return Ok(Some(p.to_string()));
    }
    let misc = fc.tables().misc.get("pict").and_then(|m| m.get(key));
    Ok(misc
        .and_then(JsonRef::as_str)
        .map(|v| v.strip_prefix("StdPicture.").unwrap_or(v).to_string()))
}

pub(crate) fn picture_el(
    fc: &mut FormCtx<'_>,
    prop: &str,
    v: Option<&Node>,
    item_name: Option<&str>,
) -> Py<Emit> {
    let Some(l) = v
        .and_then(Node::as_list)
        .filter(|l| l.len() >= 7 && l.first().is_some_and(|x| py_str(x) == "4"))
    else {
        return Ok(Emit::Cannot);
    };
    let kind = text(at(l, Some(1))?);
    if kind == "0" {
        return Ok(Emit::Absent);
    }
    let transparent = py_str(at(l, Some(6))?) == "1";
    let lt = E::leaf(
        "xr:LoadTransparent",
        if transparent { "true" } else { "false" },
    );
    let (x, y) = (text(at(l, Some(4))?), text(at(l, Some(5))?));
    let tp = (x != "-1" || y != "-1").then(|| {
        E::new(
            "xr:TransparentPixel",
            None,
            vec![attr("x", x), attr("y", y)],
            Vec::new(),
        )
    });
    if kind == "3" {
        let mut x = at(l, Some(7))?;
        while let Some(first) = x.as_list().and_then(<[Node]>::first) {
            x = first;
        }
        let s = if x.is_list() {
            "[]".to_string()
        } else {
            text(x)
        };
        let data = b64_decode(&s)?;
        let fname = format!("{prop}.{}", picture_ext(&data));
        let Some(name) = item_name else {
            return Ok(Emit::Cannot);
        };
        fc.files.push((format!("Items/{name}/{fname}"), data));
        let mut kids = vec![E::leaf("xr:Abs", fname), lt];
        kids.extend(tp);
        return Ok(parent(prop, kids));
    }
    let r = at(l, Some(2))?;
    let r0 = str_item(r, 0)?;
    let key = if r0 == "0" && py_len(r) > 1 {
        str_item(r, 1)?
    } else {
        r0
    };
    let nm = match md_name(fc.cfg, &key) {
        Some(n) => n.to_string(),
        None => match std_pict(fc, &key)? {
            Some(p) => format!("StdPicture.{p}"),
            None => return Ok(Emit::Cannot),
        },
    };
    let mut kids = vec![E::leaf("xr:Ref", nm), lt];
    kids.extend(tp);
    Ok(parent(prop, kids))
}

fn find_pic(v: Option<&Node>, depth: u8) -> Option<&Node> {
    let l = v?.as_list()?;
    if matches!(l.len(), 9 | 10)
        && l.first().is_some_and(|x| py_str(x) == "4")
        && l.get(2).is_some_and(Node::is_list)
    {
        return v;
    }
    if depth < 1 {
        return l.iter().find_map(|e| find_pic(Some(e), depth + 1));
    }
    None
}

fn src_name<'s>(src: &Src<'s>) -> Py<Option<&'s str>> {
    if let Some(it) = src.item() {
        return Ok(it.name());
    }
    src.rec().map(|r| Some(r.name.as_str())).ok_or(Raised)
}

pub(crate) fn c_picture(
    fc: &mut FormCtx<'_>,
    _ctx: &str,
    prop: &str,
    src: &Src<'_>,
    ent: Option<PropRef<'_>>,
) -> Option<Emit> {
    let path = learned_path(ent)?;
    let r = picture(fc, prop, src, path);
    run(fc, r)
}

fn picture(
    fc: &mut FormCtx<'_>,
    prop: &str,
    src: &Src<'_>,
    path: Py<Vec<Seg<'_>>>,
) -> Py<Option<Emit>> {
    let Some(v) = find_pic(value_at(src, &path?)?, 0) else {
        return Ok(None);
    };
    let name = src_name(src)?;
    picture_el(fc, prop, Some(v), name).map(Some)
}

fn misc_color(fc: &FormCtx<'_>, key: &str) -> Py<Option<String>> {
    let misc = fc.tables().misc.get("colors").and_then(|m| m.get(key));
    if let Some(t) = misc.and_then(JsonRef::as_str) {
        return Ok(Some(t.to_string()));
    }
    let t = schema()?.ui_tables();
    Ok(match key.split_once('|') {
        Some(("3", k)) => t.style_colors.get(k).map(|v| format!("style:{v}")),
        Some(("1", k)) => t.win_colors.get(k).map(|v| format!("win:{v}")),
        _ => None,
    })
}

fn style_ref(nm: &str) -> Py<String> {
    let (_, rest) = nm.split_once('.').ok_or(Raised)?;
    Ok(format!("style:{rest}"))
}

fn color_str_py(v: &Node) -> Py<Option<String>> {
    let l = v.as_list().ok_or(Raised)?;
    let kind = text(at(l, Some(1))?);
    let v2 = at(l, Some(2))?;
    if kind == "0" {
        let val = match v2 {
            Node::List(x) => x.first().map(text).unwrap_or_default(),
            _ => String::new(),
        };
        let n = py_int(&val)?;
        return Ok(Some(format!(
            "#{:02X}{:02X}{:02X}",
            n & 255,
            (n >> 8) & 255,
            (n >> 16) & 255
        )));
    }
    Ok(color_str(v, schema()?))
}

pub(crate) fn color_text(fc: &FormCtx<'_>, v: &Node) -> Py<Option<String>> {
    let l = v.as_list().ok_or(Raised)?;
    let kind = text(at(l, Some(1))?);
    if matches!(kind.as_str(), "1" | "2" | "3")
        && let Some(r) = at(l, Some(2))?.as_list()
    {
        if r.len() == 2 {
            let Some(nm) = md_name(fc.cfg, &text(at(r, Some(1))?)) else {
                return Ok(None);
            };
            return style_ref(nm).map(Some);
        }
        if r.len() == 1
            && let Some(t) = misc_color(fc, &format!("{kind}|{}", text(at(r, Some(0))?)))?
        {
            return Ok(Some(t));
        }
    }
    color_str_py(v)
}

fn is_color(v: &Node) -> bool {
    v.as_list()
        .is_some_and(|l| l.len() == 4 && l.first().is_some_and(|x| py_str(x) == "4"))
}

pub(crate) fn color_el(fc: &FormCtx<'_>, prop: &str, v: &Node) -> Py<Option<E>> {
    if !is_color(v) {
        return Ok(None);
    }
    Ok(color_text(fc, v)?.map(|s| E::leaf(prop, s)))
}

pub(crate) fn c_color(
    fc: &mut FormCtx<'_>,
    _ctx: &str,
    prop: &str,
    src: &Src<'_>,
    ent: Option<PropRef<'_>>,
) -> Option<Emit> {
    let path = learned_path(ent)?;
    let r = color(fc, prop, src, ent, path);
    run(fc, r)
}

fn color(
    fc: &FormCtx<'_>,
    prop: &str,
    src: &Src<'_>,
    ent: Option<PropRef<'_>>,
    path: Py<Vec<Seg<'_>>>,
) -> Py<Option<Emit>> {
    let Some(v) = value_at(src, &path?)?.filter(|v| is_color(v)) else {
        return Ok(None);
    };
    if ent.is_some_and(|e| is_default(e, v)) {
        return Ok(Some(Emit::Absent));
    }
    Ok(color_el(fc, prop, v)?.map(Emit::El))
}

fn font_kind(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "0" => "Absolute",
        "1" => "WindowsFont",
        "2" => "StyleItem",
        "3" => "AutoFont",
        _ => return None,
    })
}

pub(crate) fn height(v: &Node) -> Py<String> {
    let n = int_of(v)?;
    Ok(if n.rem_euclid(10) == 0 {
        n.div_euclid(10).to_string()
    } else {
        tenth_g(n)
    })
}

fn misc_font(fc: &FormCtx<'_>, key: &str) -> Py<Option<String>> {
    let misc = fc.tables().misc.get("fonts").and_then(|m| m.get(key));
    if let Some(t) = misc.and_then(JsonRef::as_str) {
        return Ok(Some(t.to_string()));
    }
    let t = schema()?.ui_tables();
    Ok(match key.split_once('|') {
        Some(("2", k)) => t.style_fonts.get(k).map(|v| format!("style:{v}")),
        Some(("1", k)) => t.windows_fonts.get(k).map(|v| format!("sys:{v}")),
        _ => None,
    })
}

fn font_attrs_absolute(l: &[Node]) -> Py<Vec<(String, String)>> {
    let f = py_slice(l, 3, None);
    let face = if f.len() > 13 {
        text(at(f, Some(13))?)
    } else {
        String::new()
    };
    let flag_at = |i| Ok::<_, Raised>(flag(py_str(at(f, Some(i))?) == "1"));
    Ok(vec![
        attr("faceName", face),
        attr("height", height(at(f, Some(0))?)?),
        attr("bold", flag(int_of(at(f, Some(4))?)? >= 700)),
        attr("italic", flag_at(5)?),
        attr("underline", flag_at(6)?),
        attr("strikeout", flag_at(7)?),
        attr("kind", "Absolute"),
        attr("scale", text(at(f, Some(15))?)),
    ])
}

pub(crate) fn font_attrs_form(fc: &FormCtx<'_>, v: &Node) -> Py<Option<Vec<(String, String)>>> {
    let Node::List(l) = v else {
        return Ok(None);
    };
    if py_str(at(l, Some(0))?) != "8" {
        return Ok(None);
    }
    let kind = text(at(l, Some(1))?);
    let mask = int_of(at(l, Some(2))?)?;
    if kind == "0" {
        return font_attrs_absolute(l).map(Some);
    }
    let Some(kind_name) = font_kind(&kind) else {
        return Ok(None);
    };
    let mut a = Vec::new();
    if kind != "3" {
        let r = at(l, Some(3))?;
        if py_len(r) == 2 {
            let Some(nm) = md_name(fc.cfg, &str_item(r, 1)?) else {
                return Ok(None);
            };
            a.push(attr("ref", style_ref(nm)?));
        } else {
            let Some(r) = misc_font(fc, &format!("{kind}|{}", str_item(r, 0)?))? else {
                return Ok(None);
            };
            a.push(attr("ref", r));
        }
    }
    let mut i: i64 = if kind == "3" { 3 } else { 4 };
    let mut vals: Vec<(i64, &Node)> = Vec::new();
    for bit in [2, 4, 8, 16, 32] {
        if mask & bit != 0 {
            vals.push((bit, at(l, Some(i))?));
            i += 1;
        }
    }
    if mask & 1 != 0 {
        a.push(attr("faceName", text(at(l, Some(i))?)));
        i += 1;
    }
    for (bit, x) in vals {
        match bit {
            2 => a.push(attr("height", height(x)?)),
            4 => a.push(attr("bold", flag(int_of(x)? >= 700))),
            8 => a.push(attr("italic", flag(py_str(x) == "1"))),
            16 => a.push(attr("underline", flag(py_str(x) == "1"))),
            _ => a.push(attr("strikeout", flag(py_str(x) == "1"))),
        }
    }
    a.push(attr("kind", kind_name));
    if mask & 512 != 0 {
        a.push(attr("scale", text(at(l, i.checked_add(1))?)));
    }
    Ok(Some(a))
}

pub(crate) fn font_el(fc: &FormCtx<'_>, prop: &str, v: &Node) -> Py<Option<E>> {
    Ok(font_attrs_form(fc, v)?.map(|a| E::new(prop, None, a, Vec::new())))
}

pub(crate) fn c_font(
    fc: &mut FormCtx<'_>,
    _ctx: &str,
    prop: &str,
    src: &Src<'_>,
    ent: Option<PropRef<'_>>,
) -> Option<Emit> {
    let path = learned_path(ent)?;
    let r = font(fc, prop, src, ent, path);
    run(fc, r)
}

fn font(
    fc: &FormCtx<'_>,
    prop: &str,
    src: &Src<'_>,
    ent: Option<PropRef<'_>>,
    path: Py<Vec<Seg<'_>>>,
) -> Py<Option<Emit>> {
    let Some(v) = value_at(src, &path?)?.filter(|v| {
        v.as_list()
            .and_then(<[Node]>::first)
            .is_some_and(|x| py_str(x) == "8")
    }) else {
        return Ok(None);
    };
    if ent.is_some_and(|e| is_default(e, v)) {
        return Ok(Some(Emit::Absent));
    }
    Ok(font_el(fc, prop, v)?.map(Emit::El))
}

fn fmt_date(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let sl = |a: usize, b: usize| -> String {
        c.get(a.min(c.len())..b.min(c.len()))
            .unwrap_or_default()
            .iter()
            .collect()
    };
    format!(
        "{}-{}-{}T{}:{}:{}",
        sl(0, 4),
        sl(4, 6),
        sl(6, 8),
        sl(8, 10),
        sl(10, 12),
        sl(12, 14)
    )
}

fn dtref(fc: &FormCtx<'_>, v: &Node) -> Py<Option<String>> {
    let ou = str_item(v, 1)?;
    py_item(v, 2)?;
    let cfg = fc.cfg;
    if ou != ZERO && md_name(cfg, &ou).is_none() {
        let tn = Guid::parse(&ou).and_then(|g| cfg.index.types.get(&g));
        if tn.is_some_and(|t| !t.contains('.')) {
            return Err(Raised);
        }
    }
    let ctx = Ctx {
        schema: schema()?,
        index: &cfg.index,
        predef: &cfg.predef,
    };
    Ok(design_time_ref(&ctx, v))
}

fn tvalue(fc: &FormCtx<'_>, v: &Node) -> Py<Option<TypedValue>> {
    let Node::List(l) = v else {
        return Ok(None);
    };
    let Some(c) = l.first() else {
        return Ok(None);
    };
    let arg = || at(l, Some(1)).map(text);
    Ok(Some(match py_str(c).as_ref() {
        "U" => TypedValue::Nil,
        "S" => TypedValue::Str(arg()?),
        "N" => TypedValue::Number(arg()?),
        "B" => TypedValue::Bool(arg()? == "1"),
        "D" => TypedValue::Date(fmt_date(&arg()?)),
        "#" => match arg()?.as_str() {
            T_DTREF => return Ok(dtref(fc, at(l, Some(2))?)?.map(TypedValue::DesignTimeRef)),
            T_FIXARR => {
                let arr = at(l, Some(2))?;
                let n = int_of(py_item(arr, 0)?.as_ref())?;
                let seq = py_seq(arr);
                let mut vals = Vec::new();
                for x in py_slice(&seq, 1, Some(n.saturating_add(1))) {
                    match tvalue(fc, x)? {
                        Some(t) => vals.push(t),
                        None => return Ok(None),
                    }
                }
                TypedValue::FixedArray(vals)
            }
            _ => return Ok(None),
        },
        _ => return Ok(None),
    }))
}

fn typed_text(t: &TypedValue) -> (&'static str, Option<String>) {
    match t {
        TypedValue::Nil => ("nil", None),
        TypedValue::Str(s) => ("xs:string", Some(s.clone())),
        TypedValue::Number(s) => ("xs:decimal", Some(s.clone())),
        TypedValue::Bool(b) => ("xs:boolean", Some(flag(*b).to_string())),
        TypedValue::Date(s) => ("xs:dateTime", Some(s.clone())),
        TypedValue::DesignTimeRef(s) => ("xr:DesignTimeRef", Some(s.clone())),
        TypedValue::EmptyTypeDescription => ("v8:TypeDescription", None),
        TypedValue::FixedArray(_) => ("v8:FixedArray", None),
    }
}

fn nil_el(tag: &str) -> E {
    E::new(tag, None, vec![attr("xsi:nil", "true")], Vec::new())
}

pub(crate) fn value_el(fc: &FormCtx<'_>, tag: &str, tv: &Node) -> Py<Option<E>> {
    let Some(r) = tvalue(fc, tv)? else {
        return Ok(None);
    };
    Ok(Some(match &r {
        TypedValue::Nil => nil_el(tag),
        TypedValue::FixedArray(kids) => {
            let out = kids
                .iter()
                .map(|k| match k {
                    TypedValue::Nil => nil_el("v8:Value"),
                    k => {
                        let (t, s) = typed_text(k);
                        E::new("v8:Value", s, xsi_type(t), Vec::new())
                    }
                })
                .collect();
            E::new(tag, None, xsi_type("v8:FixedArray"), out)
        }
        other => {
            let (t, s) = typed_text(other);
            E::new(tag, s, xsi_type(t), Vec::new())
        }
    }))
}

fn fixed_values(fc: &FormCtx<'_>, arr: &Node) -> Py<Option<Vec<E>>> {
    let n = int_of(py_item(arr, 0)?.as_ref())?;
    let seq = py_seq(arr);
    let mut vals = Vec::new();
    for x in py_slice(&seq, 1, Some(n.saturating_add(1))) {
        match choice_param_value(fc, "v8:Value", x)? {
            Some(e) => vals.push(e),
            None => return Ok(None),
        }
    }
    Ok(Some(vals))
}

fn destime_kids(fc: &FormCtx<'_>, d: &Node) -> Py<Option<Vec<E>>> {
    let Some(l) = d.as_list().filter(|l| l.len() >= 6) else {
        return Ok(None);
    };
    let items = lstr_items(l.get(5)).unwrap_or_default();
    let pres = E::new("Presentation", None, Vec::new(), lstr_kids(&items));
    let d2 = at(l, Some(2))?;
    let fixarr = d2.as_list().filter(|x| {
        x.len() == 3
            && x.first().is_some_and(|h| py_str(h) == "#")
            && x.get(1).is_some_and(|t| py_str(t) == T_FIXARR)
    });
    let val = if py_str(at(l, Some(1))?) == "0" {
        let r = Node::List(vec![
            Node::atom("0"),
            at(l, Some(3))?.clone(),
            at(l, Some(4))?.clone(),
        ]);
        let Some(r) = dtref(fc, &r)? else {
            return Ok(None);
        };
        E::new("Value", Some(r), xsi_type("xr:DesignTimeRef"), Vec::new())
    } else if let Some(x) = fixarr {
        let Some(vals) = fixed_values(fc, at(x, Some(2))?)? else {
            return Ok(None);
        };
        E::new("Value", None, xsi_type("v8:FixedArray"), vals)
    } else {
        let Some(v) = value_el(fc, "Value", d2)? else {
            return Ok(None);
        };
        v
    };
    Ok(Some(vec![pres, val]))
}

fn is_hash_of(v: &Node, t: &str) -> Py<bool> {
    Ok(str_item(v, 0)? == "#" && str_item(v, 1)? == t)
}

pub(crate) fn choice_param_value(fc: &FormCtx<'_>, tag: &str, v: &Node) -> Py<Option<E>> {
    if is_hash_of(v, T_DESTIME)? {
        let Some(kids) = destime_kids(fc, py_item(v, 2)?.as_ref())? else {
            return Ok(None);
        };
        return Ok(Some(E::new(
            tag,
            None,
            xsi_type("FormChoiceListDesTimeValue"),
            kids,
        )));
    }
    if is_hash_of(v, T_FIXARR)? {
        let Some(vals) = fixed_values(fc, py_item(v, 2)?.as_ref())? else {
            return Ok(None);
        };
        return Ok(Some(E::new(tag, None, xsi_type("FixedArray"), vals)));
    }
    value_el(fc, tag, v)
}

fn field_list<'s>(src: &Src<'s>, i: i64, head: &str) -> Py<Option<&'s [Node]>> {
    let Some(Node::List(l)) = value_at(src, &[Seg::Int(39), Seg::Int(i)])? else {
        return Ok(None);
    };
    Ok((py_str(at(l, Some(0))?) == head).then_some(l.as_slice()))
}

fn pair(i: i64, k: i64) -> Option<i64> {
    i.checked_mul(2)?.checked_add(k)
}

pub(crate) fn c_choice_list(
    fc: &mut FormCtx<'_>,
    _ctx: &str,
    prop: &str,
    src: &Src<'_>,
    _ent: Option<PropRef<'_>>,
) -> Option<Emit> {
    let r = choice_list(fc, prop, src);
    run(fc, r)
}

fn choice_list(fc: &FormCtx<'_>, prop: &str, src: &Src<'_>) -> Py<Option<Emit>> {
    let Some(v) = field_list(src, 1, "3")? else {
        return Ok(None);
    };
    let n = int_of(at(v, Some(1))?)?;
    if n == 0 {
        return Ok(Some(Emit::Absent));
    }
    let mut items = Vec::new();
    let mut i: i64 = 0;
    while i < n {
        let p = at(v, pair(i, 2))?;
        let d = at(v, pair(i, 3))?;
        let st = at(v, pair(n, 2).and_then(|x| x.checked_add(i)))?;
        if !is_hash_of(d, T_DESTIME)? {
            return Ok(Some(Emit::Cannot));
        }
        let Some(kids) = destime_kids(fc, py_item(d, 2)?.as_ref())? else {
            return Ok(Some(Emit::Cannot));
        };
        let p = text(p);
        items.push(E::new(
            "xr:Item",
            None,
            Vec::new(),
            vec![
                E::new(
                    "xr:Presentation",
                    (!p.is_empty()).then_some(p),
                    Vec::new(),
                    Vec::new(),
                ),
                E::leaf("xr:CheckState", str_item(st, 0)?),
                E::new(
                    "xr:Value",
                    None,
                    xsi_type("FormChoiceListDesTimeValue"),
                    kids,
                ),
            ],
        ));
        i += 1;
    }
    Ok(Some(parent(prop, items)))
}

pub(crate) fn c_choice_params(
    fc: &mut FormCtx<'_>,
    _ctx: &str,
    prop: &str,
    src: &Src<'_>,
    _ent: Option<PropRef<'_>>,
) -> Option<Emit> {
    let r = choice_params(fc, prop, src);
    run(fc, r)
}

fn choice_params(fc: &FormCtx<'_>, prop: &str, src: &Src<'_>) -> Py<Option<Emit>> {
    let Some(v) = field_list(src, 27, "0")? else {
        return Ok(None);
    };
    let n = int_of(at(v, Some(1))?)?;
    if n == 0 {
        return Ok(Some(Emit::Absent));
    }
    let mut items = Vec::new();
    let mut i: i64 = 0;
    while i < n {
        let name = text(at(v, pair(i, 2))?);
        let val = at(v, pair(i, 3))?;
        let Some(e) = choice_param_value(fc, "app:value", val)? else {
            return Ok(Some(Emit::Cannot));
        };
        items.push(E::new("app:item", None, vec![attr("name", name)], vec![e]));
        i += 1;
    }
    Ok(Some(parent(prop, items)))
}

pub(crate) fn c_choice_links(
    fc: &mut FormCtx<'_>,
    _ctx: &str,
    prop: &str,
    src: &Src<'_>,
    _ent: Option<PropRef<'_>>,
) -> Option<Emit> {
    let r = choice_links(fc, prop, src);
    run(fc, r)
}

fn segs_path(fc: &mut FormCtx<'_>, k: i64, segs: &[Node]) -> Py<Option<String>> {
    let mut full = vec![Node::atom(k.to_string())];
    full.extend(segs.iter().cloned());
    let p = fc.info.data_path(&Node::List(full));
    if fc.raised() {
        return Err(Raised);
    }
    Ok(p)
}

fn choice_links(fc: &mut FormCtx<'_>, prop: &str, src: &Src<'_>) -> Py<Option<Emit>> {
    let Some(v) = field_list(src, 26, "5006")? else {
        return Ok(None);
    };
    let n = int_of(at(v, Some(1))?)?;
    if n == 0 {
        return Ok(Some(Emit::Absent));
    }
    let mut links = Vec::new();
    let mut i: i64 = 2;
    let mut done: i64 = 0;
    while done < n {
        let name = text(at(v, Some(i))?);
        let k = int_of(at(v, i.checked_add(1))?)?;
        let start = i.checked_add(2).ok_or(Raised)?;
        let segs = py_slice(v, start, Some(start.checked_add(k).ok_or(Raised)?));
        let mode = text(at(v, start.checked_add(k))?);
        i = i
            .checked_add(3)
            .and_then(|x| x.checked_add(k))
            .ok_or(Raised)?;
        let Some(p) = segs_path(fc, k, segs)? else {
            return Ok(Some(Emit::Cannot));
        };
        let change = match mode.as_str() {
            "0" => "Clear".to_string(),
            "1" => "DontChange".to_string(),
            _ => mode,
        };
        links.push(E::new(
            "xr:Link",
            None,
            Vec::new(),
            vec![
                E::leaf("xr:Name", name),
                E::new("xr:DataPath", Some(p), xsi_type("xs:string"), Vec::new()),
                E::leaf("xr:ValueChange", change),
            ],
        ));
        done += 1;
    }
    Ok(Some(parent(prop, links)))
}

pub(crate) fn c_type_link(
    fc: &mut FormCtx<'_>,
    _ctx: &str,
    prop: &str,
    src: &Src<'_>,
    _ent: Option<PropRef<'_>>,
) -> Option<Emit> {
    let r = type_link(fc, prop, src);
    run(fc, r)
}

fn type_link(fc: &mut FormCtx<'_>, prop: &str, src: &Src<'_>) -> Py<Option<Emit>> {
    let Some(v) = field_list(src, 42, "3")? else {
        return Ok(None);
    };
    let k = int_of(at(v, Some(1))?)?;
    if k == 0 {
        return Ok(Some(Emit::Absent));
    }
    let segs = py_slice(v, 2, Some(k.checked_add(2).ok_or(Raised)?));
    let Some(p) = segs_path(fc, k, segs)? else {
        return Ok(Some(Emit::Cannot));
    };
    let item = text(at(v, k.checked_add(2))?);
    Ok(Some(parent(
        prop,
        vec![E::leaf("xr:DataPath", p), E::leaf("xr:LinkItem", item)],
    )))
}
