use super::tables::PropRef;
use brace::Node;

use super::codecs_a::{Seg, parent, value_at};
use super::codecs_b::{
    at, int_of, is_default, learned_path, picture_el, run, str_item, text, xsi_type,
};
use super::ctx::{DL_SPECIAL, FormInfo, md_name};
use super::fe::E;
use super::py::{Py, Raised, py_item, py_len, py_seq, py_slice, py_str};
use super::write::{Emit, FormCtx, Src};

pub(crate) const T_TYPEDESC: &str = "f5c65050-3bbb-11d5-b988-0050bae0a95d";
const T_MDOBJECTREF: &str = "fc01b5df-97fe-449b-83d4-218a090e681e";
pub(crate) const ZERO: &str = "00000000-0000-0000-0000-000000000000";

pub(crate) fn schema() -> Py<&'static mdschema::Schema> {
    mdschema::schema().map_err(|_| Raised)
}

pub(crate) fn leaf(tag: &str, s: impl Into<String>) -> E {
    E::leaf(tag, s)
}

pub(crate) fn flag(b: bool) -> &'static str {
    if b { "true" } else { "false" }
}

pub(crate) fn item(v: &Node, i: i64) -> Py<Node> {
    py_item(v, i).map(std::borrow::Cow::into_owned)
}

pub(crate) fn type_el(fi: &FormInfo<'_>, prop: &str, pat: Option<&Node>) -> Py<Option<E>> {
    let Some(pat) = pat.filter(|p| {
        p.as_list()
            .and_then(<[Node]>::first)
            .is_some_and(|x| py_str(x) == "Pattern")
    }) else {
        return Ok(None);
    };
    let Some(names) = fi.type_names_py(pat)? else {
        return Ok(None);
    };
    let items = pat.as_list().and_then(|l| l.get(1..)).unwrap_or_default();
    let (mut qs, mut qn, mut qd, mut qr) = (None, None, None, None);
    for it in items {
        let code = str_item(it, 0)?;
        let len = py_len(it);
        let s = |i: i64| str_item(it, i);
        let var = |len: usize| -> Py<&'static str> {
            Ok(if len < 3 || s(2)? == "1" {
                "Variable"
            } else {
                "Fixed"
            })
        };
        match code.as_str() {
            "S" => qs = Some((if len > 1 { s(1)? } else { "0".into() }, var(len)?)),
            "N" => {
                qn = Some((
                    if len > 1 { s(1)? } else { "0".into() },
                    if len > 2 { s(2)? } else { "0".into() },
                    if len > 3 && s(3)? == "1" {
                        "Nonnegative"
                    } else {
                        "Any"
                    },
                ));
            }
            "D" => {
                let k = if len > 1 { s(1)? } else { String::new() };
                qd = Some(match k.as_str() {
                    "D" => "Date",
                    "T" => "Time",
                    _ => "DateTime",
                });
            }
            "R" => qr = Some((if len > 1 { s(1)? } else { "0".into() }, var(len)?)),
            _ => {}
        }
    }
    let mut kids = Vec::new();
    for want in ["Type", "TypeSet"] {
        for t in names.iter().filter(|t| t.kind == want) {
            let mut attrs = Vec::new();
            if let Some(ns) = t.ns.as_deref().filter(|ns| !ns.is_empty()) {
                let k = if t.name.starts_with("@:") {
                    "xmlns:@".to_string()
                } else {
                    format!("xmlns:{}", t.name.split(':').next().unwrap_or_default())
                };
                attrs.push((k, ns.to_string()));
            }
            kids.push(E::new(
                format!("v8:{}", t.kind),
                Some(t.name.clone()),
                attrs,
                Vec::new(),
            ));
        }
    }
    let group = |tag: &str, kids: Vec<E>| E::new(tag, None, Vec::new(), kids);
    if let Some((d, f, sign)) = qn {
        kids.push(group(
            "v8:NumberQualifiers",
            vec![
                leaf("v8:Digits", d),
                leaf("v8:FractionDigits", f),
                leaf("v8:AllowedSign", sign),
            ],
        ));
    }
    if let Some((n, al)) = qs {
        kids.push(group(
            "v8:StringQualifiers",
            vec![leaf("v8:Length", n), leaf("v8:AllowedLength", al)],
        ));
    }
    if let Some(d) = qd {
        kids.push(group(
            "v8:DateQualifiers",
            vec![leaf("v8:DateFractions", d)],
        ));
    }
    if let Some((n, al)) = qr {
        kids.push(group(
            "v8:BinaryDataQualifiers",
            vec![leaf("v8:Length", n), leaf("v8:AllowedLength", al)],
        ));
    }
    Ok(Some(group(prop, kids)))
}

fn type_emit(e: Option<E>) -> Emit {
    e.map_or(Emit::Cannot, Emit::El)
}

pub(crate) fn c_type(
    fc: &mut FormCtx<'_>,
    ctx: &str,
    prop: &str,
    src: &Src<'_>,
    _ent: Option<PropRef<'_>>,
) -> Option<Emit> {
    let p = match ctx {
        "Attribute" | "Column" => 5,
        "Parameter" => 2,
        _ => return None,
    };
    let r = value_at(src, &[Seg::Int(p)])
        .and_then(|v| type_el(&fc.info, prop, v))
        .map(|e| Some(type_emit(e)));
    run(fc, r)
}

pub(crate) fn fo_el(fc: &FormCtx<'_>, prop: &str, v: Option<&Node>) -> Py<Emit> {
    let Some(l) = v.and_then(Node::as_list).filter(|l| l.len() >= 2) else {
        return Ok(Emit::Cannot);
    };
    let n = int_of(at(l, Some(1))?)?;
    if n == 0 {
        return Ok(Emit::Absent);
    }
    let mut kids = Vec::new();
    for u in py_slice(l, 2, Some(n.saturating_add(2))) {
        let Some(nm) = md_name(fc.cfg, &py_str(u)) else {
            return Ok(Emit::Cannot);
        };
        kids.push(leaf("Item", nm));
    }
    Ok(parent(prop, kids))
}

pub(crate) fn c_fo(
    fc: &mut FormCtx<'_>,
    ctx: &str,
    prop: &str,
    src: &Src<'_>,
    _ent: Option<PropRef<'_>>,
) -> Option<Emit> {
    let p = match ctx {
        "Attribute" => 15,
        "Column" => 8,
        "Command" => 12,
        _ => return None,
    };
    let r = value_at(src, &[Seg::Int(p)])
        .and_then(|v| fo_el(fc, prop, v))
        .map(Some);
    run(fc, r)
}

pub(crate) fn roles_el(fc: &FormCtx<'_>, prop: &str, v: Option<&Node>) -> Py<Emit> {
    let Some(r) = v
        .and_then(Node::as_list)
        .filter(|l| l.len() == 2)
        .and_then(|l| l.get(1))
        .and_then(Node::as_list)
    else {
        return Ok(Emit::Cannot);
    };
    let common = str_item(at(r, Some(1))?, 1)? == "1";
    let n = int_of(at(r, Some(2))?)?;
    if common && n == 0 {
        return Ok(Emit::Absent);
    }
    let mut kids = vec![leaf("xr:Common", flag(common))];
    for i in 0..n.max(0) {
        let k = i.checked_mul(2);
        let u = py_str(at(r, k.and_then(|k| k.checked_add(3)))?);
        let b = at(r, k.and_then(|k| k.checked_add(4)))?;
        let Some(nm) = md_name(fc.cfg, &u) else {
            return Ok(Emit::Cannot);
        };
        kids.push(E::new(
            "xr:Value",
            Some(flag(str_item(b, 1)? == "1").to_string()),
            vec![("name".to_string(), nm.to_string())],
            Vec::new(),
        ));
    }
    Ok(parent(prop, kids))
}

pub(crate) fn c_roles(
    fc: &mut FormCtx<'_>,
    ctx: &str,
    prop: &str,
    src: &Src<'_>,
    _ent: Option<PropRef<'_>>,
) -> Option<Emit> {
    let p = match (ctx, prop) {
        ("Attribute" | "Column", "View") => 6,
        ("Attribute" | "Column", "Edit") => 7,
        ("Command", "Use") => 5,
        _ => return None,
    };
    let r = value_at(src, &[Seg::Int(p)])
        .and_then(|v| roles_el(fc, prop, v))
        .map(Some);
    run(fc, r)
}

fn num_key(key: i64) -> Option<&'static str> {
    Some(match key {
        106 => "Num *",
        107 => "Num +",
        109 => "Num -",
        110 => "Num .",
        111 => "Num /",
        _ => return None,
    })
}

pub(crate) fn shortcut_text(v: &[Node]) -> Py<Option<String>> {
    let key = int_of(at(v, Some(1))?)?;
    let mods = int_of(at(v, Some(2))?)?;
    if key == 0 {
        return Ok(Some(String::new()));
    }
    let mut parts: Vec<String> = [(8, "Ctrl"), (16, "Alt"), (4, "Shift")]
        .iter()
        .filter(|(b, _)| mods & b != 0)
        .map(|(_, n)| (*n).to_string())
        .collect();
    let kn = if (112..=135).contains(&key) {
        Some(format!("F{}", key - 111))
    } else if (48..=57).contains(&key) || (65..=90).contains(&key) {
        u32::try_from(key)
            .ok()
            .and_then(char::from_u32)
            .map(String::from)
    } else if (96..=105).contains(&key) {
        Some(format!("Num {}", key - 96))
    } else {
        schema()?
            .ui()
            .keys
            .get(&key.to_string())
            .filter(|k| !k.is_empty())
            .cloned()
            .or_else(|| num_key(key).map(String::from))
    };
    let Some(kn) = kn else {
        return Ok(None);
    };
    parts.push(kn);
    Ok(Some(parts.join("+")))
}

pub(crate) fn c_shortcut(
    fc: &mut FormCtx<'_>,
    _ctx: &str,
    prop: &str,
    src: &Src<'_>,
    ent: Option<PropRef<'_>>,
) -> Option<Emit> {
    let path = learned_path(ent)?;
    let r = (|| {
        let v = value_at(src, &path?)?;
        let Some(l) = v.and_then(Node::as_list).filter(|l| l.len() == 3) else {
            return Ok(None);
        };
        Ok(Some(match shortcut_text(l)? {
            None => Emit::Cannot,
            Some(s) if s.is_empty() => Emit::Absent,
            Some(s) => Emit::El(leaf(prop, s)),
        }))
    })();
    run(fc, r)
}

pub(crate) fn c_cmd_picture(
    fc: &mut FormCtx<'_>,
    _ctx: &str,
    prop: &str,
    src: &Src<'_>,
    _ent: Option<PropRef<'_>>,
) -> Option<Emit> {
    let r = value_at(src, &[Seg::Int(7)])
        .and_then(|v| picture_el(fc, prop, v, None))
        .map(Some);
    run(fc, r)
}

pub(crate) fn c_assoc_table(
    fc: &mut FormCtx<'_>,
    _ctx: &str,
    prop: &str,
    src: &Src<'_>,
    _ent: Option<PropRef<'_>>,
) -> Option<Emit> {
    let r = (|| {
        let v = text(at(src.body()?, Some(14))?);
        if v == "0" {
            return Ok(Some(Emit::Absent));
        }
        Ok(Some(match fc.info.item(&v) {
            None => Emit::Cannot,
            Some(it) => Emit::El(E::new(
                prop,
                it.name().map(String::from),
                xsi_type("xs:string"),
                Vec::new(),
            )),
        }))
    })();
    run(fc, r)
}

fn rel_fields(fc: &mut FormCtx<'_>, attr_name: &str, v: &Node) -> Py<Option<Vec<String>>> {
    let n = int_of(&item(v, 1)?)?;
    let aid = fc
        .info
        .attrs
        .iter()
        .find(|x| x.name == attr_name)
        .map(|a| a.id.clone());
    let seq = py_seq(v);
    let mut out = Vec::new();
    for p in py_slice(&seq, 2, Some(n.saturating_add(2))) {
        let k = int_of(&item(p, 0)?)?;
        let ps = py_seq(p);
        let segs = py_slice(&ps, 1, Some(k.saturating_add(1)));
        if segs.is_empty() {
            out.push(attr_name.to_string());
            continue;
        }
        let aid = aid.as_deref().ok_or(Raised)?;
        let mut full = vec![
            Node::atom(segs.len().saturating_add(1).to_string()),
            Node::list(vec![Node::atom(aid)]),
        ];
        full.extend(segs.iter().cloned());
        let Some(s) = fc.info.path_py(&Node::list(full))? else {
            return Ok(None);
        };
        out.push(s);
    }
    out.sort();
    Ok(Some(out))
}

pub(crate) fn c_fields(
    fc: &mut FormCtx<'_>,
    _ctx: &str,
    prop: &str,
    src: &Src<'_>,
    _ent: Option<PropRef<'_>>,
) -> Option<Emit> {
    let r = fields(fc, prop, src);
    run(fc, r)
}

fn fields(fc: &mut FormCtx<'_>, prop: &str, src: &Src<'_>) -> Py<Option<Emit>> {
    let r = src.rec().ok_or(Raised)?;
    let v = at(&r.body, Some(if prop == "Save" { 9 } else { 8 }))?;
    let Some(mut names) = rel_fields(fc, &r.name, v)? else {
        return Ok(Some(Emit::Cannot));
    };
    if prop == "UseAlways"
        && let Some(ai) = fc.info.attr_index(&r.name)
        && fc
            .info
            .attrs
            .get(ai)
            .is_some_and(|a| !a.dl_fields.is_empty())
    {
        let mut i = 0usize;
        loop {
            let key = format!("ReqMapFieldId{i}");
            let Some(v) = fc
                .info
                .attrs
                .get(ai)
                .and_then(|a| a.settings.get(&key))
                .copied()
            else {
                break;
            };
            let fid = str_item(v, 1)?;
            let a = fc.info.attrs.get(ai).ok_or(Raised)?;
            let in_dl = a.dl_fields.contains_key(&fid);
            let nm = a
                .dl_fields
                .get(&fid)
                .filter(|n| !n.is_empty())
                .cloned()
                .or_else(|| {
                    DL_SPECIAL
                        .iter()
                        .find(|(k, _)| *k == fid)
                        .map(|(_, n)| (*n).to_string())
                });
            let Some(nm) = nm else {
                return Ok(Some(Emit::Cannot));
            };
            let miss = if in_dl {
                fc.info.dl_field_missing(ai, &nm)?
            } else {
                fc.info.dl_special_missing(ai)?
            };
            names.push(format!("{}{}.{nm}", if miss { "~" } else { "" }, r.name));
            i = i.saturating_add(1);
        }
        names.sort();
    }
    if names.is_empty() {
        return Ok(Some(Emit::Absent));
    }
    let kids = names.into_iter().map(|x| leaf("Field", x)).collect();
    Ok(Some(parent(prop, kids)))
}

pub(crate) fn c_user_visible(
    fc: &mut FormCtx<'_>,
    _ctx: &str,
    prop: &str,
    src: &Src<'_>,
    ent: Option<PropRef<'_>>,
) -> Option<Emit> {
    let v = if src.has_opt() {
        src.opt("UserVisible")
    } else {
        let path = learned_path(ent)?;
        match path.and_then(|p| value_at(src, &p)) {
            Ok(v) => v,
            Err(Raised) => return run(fc, Err(Raised)),
        }
    };
    let Some(v) = v.filter(|v| v.is_list()) else {
        return Some(Emit::Absent);
    };
    let r = roles_el(fc, prop, Some(v)).map(|r| (r != Emit::Cannot).then_some(r));
    run(fc, r)
}

pub(crate) fn c_md_ref(
    fc: &mut FormCtx<'_>,
    _ctx: &str,
    prop: &str,
    src: &Src<'_>,
    ent: Option<PropRef<'_>>,
) -> Option<Emit> {
    let path = learned_path(ent)?;
    let r = (|| {
        let v = value_at(src, &path?)?;
        let Some(v) = v.and_then(Node::text) else {
            return Ok(None);
        };
        if v.chars().count() != 36 || v.chars().nth(8) != Some('-') {
            return Ok(None);
        }
        if v == ZERO {
            return Ok(Some(Emit::Absent));
        }
        Ok(md_name(fc.cfg, v).map(|nm| Emit::El(leaf(prop, nm))))
    })();
    run(fc, r)
}

pub(crate) fn c_type_prop(
    fc: &mut FormCtx<'_>,
    _ctx: &str,
    prop: &str,
    src: &Src<'_>,
    ent: Option<PropRef<'_>>,
) -> Option<Emit> {
    let path = learned_path(ent)?;
    let r = (|| {
        let v = value_at(src, &path?)?;
        let Some((v, l)) = v.and_then(|v| v.as_list().map(|l| (v, l))) else {
            return Ok(None);
        };
        let hash_of = |t: &str| {
            l.len() == 3
                && l.first().is_some_and(|x| py_str(x) == "#")
                && l.get(1).is_some_and(|x| py_str(x) == t)
        };
        if hash_of(T_TYPEDESC) {
            return Ok(type_el(&fc.info, prop, l.get(2))?.map(|mut t| {
                t.attrs = xsi_type("v8:TypeDescription");
                Emit::El(t)
            }));
        }
        if hash_of(T_MDOBJECTREF) {
            let u = l.get(2).map(py_str).unwrap_or_default();
            return Ok(md_name(fc.cfg, &u).map(|nm| {
                Emit::El(E::new(
                    prop,
                    Some(nm.to_string()),
                    xsi_type("xr:MDObjectRef"),
                    Vec::new(),
                ))
            }));
        }
        if l.first().is_some_and(|x| py_str(x) == "Pattern") {
            if ent.is_some_and(|e| is_default(e, v)) {
                return Ok(Some(Emit::Absent));
            }
            return Ok(type_el(&fc.info, prop, Some(v))?.map(Emit::El));
        }
        Ok(None)
    })();
    run(fc, r)
}

pub(crate) fn c_group_list(
    fc: &mut FormCtx<'_>,
    _ctx: &str,
    prop: &str,
    src: &Src<'_>,
    _ent: Option<PropRef<'_>>,
) -> Option<Emit> {
    let Some(l) = src
        .bag("1")
        .and_then(Node::as_list)
        .filter(|l| l.len() == 2 && l.first().is_some_and(|x| py_str(x) == "N"))
    else {
        return Some(Emit::Absent);
    };
    let id = l.get(1).map(py_str).unwrap_or_default();
    if id == "0" {
        return Some(Emit::Absent);
    }
    let it = fc.info.item(&id)?;
    Some(Emit::El(E::new(
        prop,
        it.name().map(String::from),
        Vec::new(),
        Vec::new(),
    )))
}

pub(crate) fn c_table_show_cmdbar(
    fc: &mut FormCtx<'_>,
    _ctx: &str,
    prop: &str,
    src: &Src<'_>,
    _ent: Option<PropRef<'_>>,
) -> Option<Emit> {
    let b = match src.body() {
        Ok(b) => b,
        Err(Raised) => return run(fc, Err(Raised)),
    };
    let (Some(f), Some(v)) = (b.get(110), b.get(111)) else {
        return Some(Emit::Absent);
    };
    if py_str(f) != "1" {
        return Some(Emit::Absent);
    }
    let v = match py_str(v).as_ref() {
        "0" => "false",
        "1" => "true",
        "2" => "auto",
        _ => return None,
    };
    Some(Emit::El(leaf(prop, v)))
}

fn multi_values<'s>(src: &Src<'s>) -> Py<Option<&'s [Node]>> {
    let b = src.body()?;
    let Some(b39) = b.get(39).and_then(Node::as_list).filter(|l| l.len() >= 63) else {
        return Ok(None);
    };
    Ok(b39
        .get(62)
        .and_then(Node::as_list)
        .filter(|v| v.len() > 9 && v.first().is_some_and(|x| py_str(x) == "1")))
}

pub(crate) fn c_show_checkboxes(
    fc: &mut FormCtx<'_>,
    _ctx: &str,
    prop: &str,
    src: &Src<'_>,
    _ent: Option<PropRef<'_>>,
) -> Option<Emit> {
    let r = multi_values(src).map(|v| {
        Some(match v.and_then(|v| v.get(4)) {
            Some(x) if py_str(x) == "1" => Emit::El(leaf(prop, "true")),
            _ => Emit::Absent,
        })
    });
    run(fc, r)
}

pub(crate) fn c_multi_value_dp(
    fc: &mut FormCtx<'_>,
    _ctx: &str,
    prop: &str,
    src: &Src<'_>,
    _ent: Option<PropRef<'_>>,
) -> Option<Emit> {
    let r = multi_value_dp(fc, prop, src);
    run(fc, r)
}

fn multi_value_dp(fc: &mut FormCtx<'_>, prop: &str, src: &Src<'_>) -> Py<Option<Emit>> {
    let Some(v9) = multi_values(src)?
        .and_then(|v| v.get(9))
        .and_then(Node::as_list)
    else {
        return Ok(Some(Emit::Absent));
    };
    if py_str(at(v9, Some(0))?) == "0" {
        return Ok(Some(Emit::Absent));
    }
    let dp = at(src.body()?, Some(11))?;
    let Some(dpl) = dp.as_list() else {
        return Ok(None);
    };
    if py_str(at(dpl, Some(0))?) == "0" {
        return Ok(None);
    }
    let k = int_of(at(dpl, Some(0))?)?;
    let m = int_of(at(v9, Some(0))?)?;
    let mut full = vec![Node::atom(k.saturating_add(m).to_string())];
    full.extend(py_slice(dpl, 1, Some(k.saturating_add(1))).iter().cloned());
    full.extend(v9.get(1..).unwrap_or_default().iter().cloned());
    Ok(fc
        .info
        .path_py(&Node::list(full))?
        .map(|s| Emit::El(leaf(prop, s))))
}
