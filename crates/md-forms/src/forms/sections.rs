use super::tables::PropRef;
use brace::Node;

use super::codecs_a::command_ref;
use super::codecs_b::{at, int_of, run, str_item, xsi_type};
use super::ctx::{item_key, md_name};
use super::decode::Rec;
use super::fe::{E, Rendered, ser};
use super::py::{Py, Raised, b64_decode_raw, py_len, py_seq, py_slice, py_str};
use super::section_codecs::{T_TYPEDESC, ZERO, flag, item, leaf, roles_el, schema, type_el};
use super::write::{Emit, FormCtx, Src, emit_prop, esc_attr};
use super::xmlconv::{b64_xml, b64_xml_text, children, conv, parse_bytes};
use crate::xml::XmlNode;

fn rec_id(rec: &Rec) -> String {
    let n = if rec.class == "Column" {
        rec.raw.at(1)
    } else {
        rec.raw.get(&[1, 0])
    };
    n.map_or_else(String::new, |n| py_str(n).into_owned())
}

fn name_id(rec: &Rec) -> Vec<(&'static str, String)> {
    vec![("name", rec.name.clone()), ("id", rec_id(rec))]
}

fn write_record(
    fc: &mut FormCtx<'_>,
    rec: &Rec,
    ctx: &str,
    attrs: &[(&str, String)],
    depth: usize,
    lines: &mut Vec<String>,
    columns: bool,
) {
    let shared = fc.shared;
    let src = Src::Rec { rec, tag: ctx };
    let mut body = Vec::new();
    for el in shared.order(ctx).iter() {
        if fc.raised() {
            return;
        }
        if columns && el == "Columns" {
            write_columns(fc, rec, depth + 1, &mut body);
            continue;
        }
        fc.prop_depth = depth + 1;
        if let Emit::El(e) = emit_prop(fc, ctx, el, &src) {
            ser(&e, depth + 1, &mut body);
        }
    }
    if fc.raised() {
        return;
    }
    let a: String = attrs
        .iter()
        .map(|(k, v)| format!(" {k}=\"{}\"", esc_attr(v)))
        .collect();
    let pad = "\t".repeat(depth);
    if body.is_empty() {
        lines.push(format!("{pad}<{ctx}{a}/>"));
    } else {
        lines.push(format!("{pad}<{ctx}{a}>"));
        lines.append(&mut body);
        lines.push(format!("{pad}</{ctx}>"));
    }
}

fn write_columns(fc: &mut FormCtx<'_>, rec: &Rec, depth: usize, lines: &mut Vec<String>) {
    let mut adds: Vec<(String, Vec<Rec>)> = Vec::new();
    let mut unresolved = false;
    for (p, g) in fc.info.addcols.iter() {
        let Some(p) = p else {
            unresolved = true;
            break;
        };
        if p.split('.').next() == Some(rec.name.as_str()) {
            adds.push((
                p.clone(),
                g.cols.iter().map(|(_, c)| c.rec.clone()).collect(),
            ));
        }
    }
    if unresolved {
        fc.raise();
        return;
    }
    if rec.children.is_empty() && adds.is_empty() {
        return;
    }
    let pad = "\t".repeat(depth);
    lines.push(format!("{pad}<Columns>"));
    for c in &rec.children {
        write_record(fc, c, "Column", &name_id(c), depth + 1, lines, false);
        if fc.raised() {
            return;
        }
    }
    for (p, cs) in adds {
        let table = esc_attr(&p);
        if cs.is_empty() {
            lines.push(format!("{pad}\t<AdditionalColumns table=\"{table}\"/>"));
            continue;
        }
        lines.push(format!("{pad}\t<AdditionalColumns table=\"{table}\">"));
        for c in &cs {
            write_record(fc, c, "Column", &name_id(c), depth + 2, lines, false);
            if fc.raised() {
                return;
            }
        }
        lines.push(format!("{pad}\t</AdditionalColumns>"));
    }
    lines.push(format!("{pad}</Columns>"));
}

fn ca_lines(fc: &FormCtx<'_>) -> Py<Option<Vec<String>>> {
    let Some(v) = fc.info.ca.and_then(Node::as_list).and_then(<[Node]>::first) else {
        return Ok(None);
    };
    let x = b64_xml(v).ok_or(Raised)?;
    let Some(ca) = x
        .children
        .iter()
        .find(|c| c.local == "conditionalAppearance")
    else {
        return Ok(None);
    };
    if ca.children.is_empty() {
        return Ok(None);
    }
    let e = conv(Some(fc.cfg), ca, &[&x], Some("ConditionalAppearance"), None);
    let mut out = Vec::new();
    ser(&e, 2, &mut out);
    Ok(Some(out))
}

pub(crate) fn s_attributes(fc: &mut FormCtx<'_>, lines: &mut Vec<String>) {
    let df = fc.info.form;
    let ca = match ca_lines(fc) {
        Ok(ca) => ca,
        Err(Raised) => {
            fc.raise();
            return;
        }
    };
    if df.attrs.is_empty() && ca.is_none() {
        lines.push("\t<Attributes/>".to_string());
        return;
    }
    lines.push("\t<Attributes>".to_string());
    for r in &df.attrs {
        write_record(fc, r, "Attribute", &name_id(r), 2, lines, true);
        if fc.raised() {
            return;
        }
    }
    lines.extend(ca.unwrap_or_default());
    lines.push("\t</Attributes>".to_string());
}

pub(crate) fn s_commands(fc: &mut FormCtx<'_>, lines: &mut Vec<String>) {
    let df = fc.info.form;
    if df.commands.is_empty() {
        return;
    }
    lines.push("\t<Commands>".to_string());
    for r in &df.commands {
        write_record(fc, r, "Command", &name_id(r), 2, lines, false);
        if fc.raised() {
            return;
        }
    }
    lines.push("\t</Commands>".to_string());
}

pub(crate) fn s_parameters(fc: &mut FormCtx<'_>, lines: &mut Vec<String>) {
    let df = fc.info.form;
    if df.params.is_empty() {
        return;
    }
    lines.push("\t<Parameters>".to_string());
    for r in &df.params {
        write_record(
            fc,
            r,
            "Parameter",
            &[("name", r.name.clone())],
            2,
            lines,
            false,
        );
        if fc.raised() {
            return;
        }
    }
    lines.push("\t</Parameters>".to_string());
}

const DL_LIST_DEFAULTS: [(&str, &str, &str); 3] = [
    (
        "Filter",
        "dcsset:filter",
        "dfcece9d-5077-440b-b6b3-45a5cb4538eb",
    ),
    (
        "Order",
        "dcsset:order",
        "88619765-ccb3-46c6-ac52-38e9c992ebd4",
    ),
    (
        "Appearance",
        "dcsset:conditionalAppearance",
        "b75fecce-942b-4aed-abc9-e6a02e460fb3",
    ),
];

fn virtual_table(kind: &str, cat: &str) -> Option<&'static str> {
    Some(match (kind, cat) {
        ("AccumulationRegister", "3") => "Balance",
        ("InformationRegister", "3") => "SliceLast",
        ("Task", "2") => "TasksByExecutive",
        _ => return None,
    })
}

pub(crate) fn b64_of(v: &Node) -> Option<String> {
    let s = brace::to_compact(v);
    let i = s.find("#base64:")?;
    let end = match s.get(i..).and_then(|r| r.find('}')) {
        Some(j) => i.saturating_add(j),
        None => s.char_indices().last().map_or(0, |(k, _)| k),
    };
    Some(s.get(i..end.max(i)).unwrap_or_default().to_string())
}

pub(crate) fn unchunk(raw: &[u8]) -> Py<Vec<u8>> {
    if raw.get(..2) != Some(b"A\xc1".as_slice()) {
        return Ok(raw.to_vec());
    }
    let mut out = Vec::new();
    let mut p = 2usize;
    while let Some(&m) = raw.get(p) {
        let byte = |k: usize| raw.get(k).copied().map(usize::from).ok_or(Raised);
        let n = match m {
            0x9a => {
                let n = byte(p + 1)?;
                p += 2;
                n
            }
            0x9b => {
                let n = byte(p + 1)? | (byte(p + 2)? << 8);
                p += 3;
                n
            }
            _ => break,
        };
        let end = p.saturating_add(n).min(raw.len());
        out.extend_from_slice(raw.get(p.min(end)..end).unwrap_or_default());
        p = p.saturating_add(n);
    }
    Ok(out)
}

fn server_state_xml(v: &Node) -> Py<XmlNode> {
    let s = str_item(v, 1)?;
    let raw = b64_decode_raw(&s)?;
    parse_bytes(&unchunk(&raw)?).ok_or(Raised)
}

fn b64_doc(v: &Node) -> Py<XmlNode> {
    b64_xml_text(&b64_of(v).ok_or(Raised)?).ok_or(Raised)
}

fn dl_settings(fc: &FormCtx<'_>, prop: &str, ai: usize) -> Py<Emit> {
    let st = &fc.info.attrs.get(ai).ok_or(Raised)?.settings;
    let cfg = Some(fc.cfg);
    let b = |k: &str, d: Option<&str>| -> Py<Option<String>> {
        match st.get(k) {
            Some(v) => str_item(v, 1).map(Some),
            None => Ok(d.map(String::from)),
        }
    };
    let mut kids = Vec::new();
    if b("AutoFillAvailableFields", Some("1"))?.as_deref() == Some("0") {
        kids.push(leaf("AutoFillAvailableFields", "false"));
    }
    let manual = b("ManualQuery", None)?.as_deref() == Some("1");
    kids.push(leaf("ManualQuery", flag(manual)));
    let dynamic = b("DynamicalDataSelection", None)?.as_deref() != Some("1");
    kids.push(leaf("DynamicDataRead", flag(dynamic)));
    let qt = match st.get("QueryText") {
        Some(v) => str_item(v, 1)?,
        None => String::new(),
    };
    if manual || !qt.is_empty() {
        kids.push(leaf("QueryText", qt));
    }
    if let Some(v) = st.get("ServerState") {
        let root = server_state_xml(v)?;
        for c in &root.children {
            let mut e = conv(cfg, c, &[&root], Some(&c.local), None);
            if e.tag == "Parameter" {
                e.attrs.retain(|(k, _)| k != "xsi:type");
            }
            kids.push(e);
        }
    }
    let kt = b("KeyType", Some("0"))?.unwrap_or_default();
    if kt != "0" {
        let t = match kt.as_str() {
            "1" => "FieldValue",
            "2" => "RowKey",
            k => k,
        };
        kids.push(leaf("KeyType", t));
    }
    if let Some(kf) = st.get("KeyFields") {
        let lst = item(kf, 2)?;
        let n = int_of(&item(&lst, 0)?)?;
        let seq = py_seq(&lst);
        for x in py_slice(&seq, 1, Some(n.saturating_add(1))) {
            kids.push(leaf("KeyField", str_item(x, 1)?));
        }
    }
    if let Some(mt) = st.get("MainTable") {
        let u = str_item(mt, 2)?;
        if u != ZERO {
            let Some(nm) = md_name(fc.cfg, &u) else {
                return Ok(Emit::Cannot);
            };
            let cat = b("MainTableCategory", Some("1"))?.unwrap_or_default();
            let nm = if cat == "1" {
                nm.to_string()
            } else {
                let kind = nm.split('.').next().unwrap_or_default();
                let Some(vt) = virtual_table(kind, &cat) else {
                    return Ok(Emit::Cannot);
                };
                format!("{nm}.{vt}")
            };
            kids.push(leaf("MainTable", nm));
        }
    }
    if b("GetInvisibleFieldPresentations", Some("1"))?.as_deref() == Some("0") {
        kids.push(leaf("GetInvisibleFieldPresentations", "false"));
    }
    if b("AutoSaveUserSettings", Some("1"))?.as_deref() == Some("0") {
        kids.push(leaf("AutoSaveUserSettings", "false"));
    }
    let mut ls = Vec::new();
    for (key, tag, uid) in DL_LIST_DEFAULTS {
        match st.get(key) {
            Some(v) => {
                let x = b64_doc(v)?;
                let e = conv(cfg, &x, &[], Some(tag), None);
                if !e.kids.is_empty() {
                    ls.push(e);
                }
            }
            None => ls.push(E::new(
                tag,
                None,
                Vec::new(),
                vec![
                    leaf("dcsset:viewMode", "Normal"),
                    leaf("dcsset:userSettingID", uid),
                ],
            )),
        }
        if key == "Filter"
            && let Some(v) = st.get("DataParameters")
        {
            let x = b64_doc(v)?;
            let e = conv(cfg, &x, &[], Some("dcsset:dataParameters"), None);
            if !e.kids.is_empty() {
                ls.push(e);
            }
        }
    }
    if let Some(v) = st.get("Group") {
        let x = b64_doc(v)?;
        let gi = children(cfg, &x, &[]);
        if !gi.is_empty() {
            ls.push(E::new(
                "dcsset:item",
                None,
                xsi_type("dcsset:StructureItemGroup"),
                vec![E::new("dcsset:groupItems", None, Vec::new(), gi)],
            ));
        }
    }
    let gvm = b("GroupSelectedSettingViewMode", Some("0"))?.unwrap_or_default();
    if gvm != "1" {
        let m = match gvm.as_str() {
            "0" => "Normal",
            "2" => "QuickAccess",
            "3" => "Inaccessible",
            g => g,
        };
        ls.push(leaf("dcsset:itemsViewMode", m));
    }
    let gid = b(
        "GroupSelectedSettingId",
        Some("911b6018-f537-43e8-a417-da56b22f9aec"),
    )?
    .unwrap_or_default();
    if !gid.is_empty() {
        ls.push(leaf("dcsset:itemsUserSettingID", gid));
    }
    kids.push(E::new("ListSettings", None, Vec::new(), ls));
    Ok(Emit::El(E::new(prop, None, xsi_type("DynamicList"), kids)))
}

pub(crate) fn c_settings(
    fc: &mut FormCtx<'_>,
    _ctx: &str,
    prop: &str,
    src: &Src<'_>,
    _ent: Option<PropRef<'_>>,
) -> Option<Emit> {
    let r = settings(fc, prop, src);
    run(fc, r)
}

fn settings(fc: &mut FormCtx<'_>, prop: &str, src: &Src<'_>) -> Py<Option<Emit>> {
    let id = match (src.rec(), src.item()) {
        (Some(r), _) => rec_id(r),
        (None, Some(it)) => item_key(it),
        (None, None) => return Err(Raised),
    };
    let Some(ai) = fc.info.attr_by_key(&id) else {
        return Ok(Some(Emit::Absent));
    };
    let st = &fc.info.attrs.get(ai).ok_or(Raised)?.settings;
    if st.is_empty() {
        return Ok(Some(Emit::Absent));
    }
    if st.contains_key("QueryText") {
        return dl_settings(fc, prop, ai).map(Some);
    }
    if st.len() == 1
        && let Some(v) = st.get("ElementType")
    {
        if str_item(v, 1)? != T_TYPEDESC {
            return Ok(Some(Emit::Cannot));
        }
        let t = type_el(&fc.info, prop, Some(&item(v, 2)?))?;
        return Ok(Some(match t {
            None => Emit::Cannot,
            Some(mut t) => {
                t.attrs = xsi_type("v8:TypeDescription");
                Emit::El(t)
            }
        }));
    }
    let Some(v) = st
        .iter()
        .map(|(_, v)| *v)
        .find(|v| crate::graph::settings_kind(v).is_some())
    else {
        return Ok(Some(Emit::Cannot));
    };
    let r = crate::graph::form_settings_lines_resolved(fc.cfg, v, fc.prop_depth);
    graph_settings(fc, r).map(Some)
}

fn graph_settings(fc: &mut FormCtx<'_>, r: Option<Vec<String>>) -> Py<Emit> {
    match r {
        Some(lines) if lines.is_empty() => Err(Raised),
        Some(lines) => Ok(Emit::El(E::rendered(Rendered::new(move |_| lines.clone())))),
        None => {
            fc.deferred = true;
            Ok(Emit::Cannot)
        }
    }
}

fn ci_items(fc: &mut FormCtx<'_>, v: &[Node]) -> Py<Option<Vec<E>>> {
    let n = int_of(at(v, Some(1))?)?;
    let mut out = Vec::new();
    for it in py_slice(v, 2, Some(n.saturating_add(2))) {
        let Some(name) = command_ref(fc, &item(it, 2)?)? else {
            return Ok(None);
        };
        let added = str_item(it, 4)? == "1";
        let mut kids = vec![
            leaf("Command", name),
            leaf("Type", if added { "Added" } else { "Auto" }),
        ];
        let a = item(it, 3)?;
        if a.as_list().is_some_and(|l| l.len() > 1) {
            let Some(ap) = fc.info.path_py(&a)? else {
                return Ok(None);
            };
            kids.push(leaf("Attribute", ap));
        }
        let grp = item(it, 5)?;
        if py_len(&grp) == 2 {
            let g = str_item(&grp, 1)?;
            let gn = schema()?
                .ui()
                .command_groups
                .get(&g)
                .filter(|n| !n.is_empty())
                .cloned()
                .or_else(|| md_name(fc.cfg, &g).map(String::from));
            let Some(gn) = gn else {
                return Ok(None);
            };
            kids.push(leaf("CommandGroup", gn));
        }
        let index = str_item(it, 6)?;
        if index != "0" {
            kids.push(leaf("Index", index));
        }
        if str_item(it, 7)? == "0" {
            kids.push(leaf("DefaultVisible", "false"));
        }
        match roles_el(fc, "Visible", Some(&item(it, 8)?))? {
            Emit::Cannot => return Ok(None),
            Emit::Absent => {}
            Emit::El(e) => kids.push(e),
        }
        out.push(E::new("Item", None, Vec::new(), kids));
    }
    Ok(Some(out))
}

pub(crate) fn s_cmdinterface(fc: &mut FormCtx<'_>, lines: &mut Vec<String>) {
    let r = cmdinterface(fc);
    if let Some(e) = fc.check(r).flatten() {
        ser(&e, 1, lines);
    }
}

fn cmdinterface(fc: &mut FormCtx<'_>) -> Py<Option<E>> {
    let tree = fc.info.form.root.as_list().ok_or(Raised)?;
    let parts_src = [
        ("NavigationPanel", tree.get(6).ok_or(Raised)?),
        ("CommandBar", tree.get(7).ok_or(Raised)?),
    ];
    let mut parts = Vec::new();
    for (tag, v) in parts_src {
        let Some(l) = v.as_list() else {
            continue;
        };
        if int_of(at(l, Some(1))?)? == 0 {
            continue;
        }
        let Some(items) = ci_items(fc, l)? else {
            return Ok(None);
        };
        parts.push(E::new(tag, None, Vec::new(), items));
    }
    Ok((!parts.is_empty()).then(|| E::new("CommandInterface", None, Vec::new(), parts)))
}
