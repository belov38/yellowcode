use super::types::{type_desc, typed_value};
use super::ui::color_with;
use crate::index::Index;
use crate::model::{LocalString, MdRef, Value, XmlElem};
use crate::xml_out::typed_elem;
use brace::{Guid, Node};
use mdschema::{Codec, PropSpec, Schema};
use std::collections::HashMap;

pub struct Ctx<'a> {
    pub schema: &'static Schema,
    pub index: &'a Index,
    pub predef: &'a HashMap<Guid, String>,
}

pub(crate) struct Owner {
    pub kind: String,
    pub name: String,
}

const T_MDREF: &str = "157fa490-4ce9-11d4-9415-008048da11f9";
const ZERO_GUID: &str = "00000000-0000-0000-0000-000000000000";
const MODIFIERS: [(i64, &str); 3] = [(8, "Ctrl"), (16, "Alt"), (4, "Shift")];
const KEY_F1: i64 = 112;
const KEY_F24: i64 = 135;
const PURPOSES: [(&str, &str); 2] = [
    ("1", "PlatformApplication"),
    ("2", "MobilePlatformApplication"),
];
const CA_USE: [(&str, &str); 3] = [("0", "Auto"), ("1", "Use"), ("2", "DontUse")];

fn lookup(table: &[(&str, &'static str)], key: &str) -> Option<&'static str> {
    table.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

pub(super) fn int(n: &Node) -> Option<i64> {
    n.text()?.trim().parse().ok()
}

pub(super) fn count(n: &Node) -> Option<usize> {
    usize::try_from(int(n)?).ok()
}

pub(super) fn slice(l: &[Node], from: usize, n: usize) -> &[Node] {
    let tail = l.get(from..).unwrap_or(&[]);
    tail.get(..n.min(tail.len())).unwrap_or(tail)
}

fn name_of(ctx: &Ctx, n: &Node) -> Option<(Guid, String)> {
    let g = Guid::parse(n.text()?)?;
    Some((g, ctx.index.names.get(&g)?.clone()))
}

fn md_ref(ctx: &Ctx, n: &Node) -> Option<MdRef> {
    let (g, name) = name_of(ctx, n)?;
    Some(MdRef { id: Some(g), name })
}

fn md_object_ref(ctx: &Ctx, it: &Node) -> Option<MdRef> {
    let l = it.as_list()?;
    if l.first()?.text()? != "#" || l.get(1)?.text()? != T_MDREF {
        return None;
    }
    md_ref(ctx, l.get(2)?.as_list()?.get(1)?)
}

pub(crate) fn decode(
    ctx: &Ctx,
    tag: &str,
    spec: &PropSpec,
    v: &Node,
    owner: Option<&Owner>,
    path2: Option<&str>,
) -> Option<Value> {
    let ui = ctx.schema.ui();
    match spec.codec {
        Codec::Bool => Some(Value::Bool(v.text() == Some("1"))),
        Codec::Int | Codec::Str => Some(Value::Text(py_str(v))),
        Codec::Enum => Some(Value::Text(spec.map.get(v.text()?)?.clone())),
        Codec::LStr => lstr_items(v).map(Value::LStr),
        Codec::Ref => {
            let t = v.text()?;
            Guid::parse(t)?;
            if t == ZERO_GUID {
                return Some(Value::Ref(MdRef::default()));
            }
            Some(Value::Ref(md_ref(ctx, v)?))
        }
        Codec::MdList => {
            let l = v.as_list()?;
            let n = count(l.get(1)?)?;
            let refs = slice(l, 2, n)
                .iter()
                .map(|it| md_object_ref(ctx, it))
                .collect::<Option<Vec<_>>>()?;
            Some(Value::RefList(refs))
        }
        Codec::FieldList => {
            let inner = v.as_list()?.get(1)?.as_list()?;
            let n = count(inner.get(1)?)?;
            let o = owner?;
            let names = slice(inner, 2, n)
                .iter()
                .map(|it| field_ref(ctx, o, it.as_list()?.get(2)?))
                .collect::<Option<Vec<_>>>()?;
            Some(Value::FieldList(names))
        }
        Codec::Event => Some(Value::Text(py_str(v).split('_').next()?.to_string())),
        Codec::Method => {
            let (_, module) = name_of(ctx, v)?;
            Some(Value::Text(format!("{module}.{}", path2?)))
        }
        Codec::FoContent => {
            let l = v.as_list()?;
            let n = count(l.get(1)?)?;
            let refs = slice(l, 2, n)
                .iter()
                .map(|it| md_object_ref(ctx, it.as_list()?.get(2)?.as_list()?.get(1)?))
                .collect::<Option<Vec<_>>>()?;
            Some(Value::RefList(refs))
        }
        Codec::CmdGroup => {
            let g = match v {
                Node::List(l) => l.get(1)?.text()?,
                _ => v.text()?,
            };
            let nm = match ui.command_groups.get(g).filter(|s| !s.is_empty()) {
                Some(s) => s.clone(),
                None => ctx.index.names.get(&Guid::parse(g)?)?.clone(),
            };
            Some(Value::Text(nm))
        }
        Codec::Picture => {
            let l = v.as_list()?;
            if l.get(1)?.text()? == "0" {
                return Some(Value::Xml(XmlElem::new(tag)));
            }
            let r = l.get(2)?.as_list()?;
            let std = |k: &str| ui.std_pictures.get(k).filter(|s| !s.is_empty());
            let first = r.first()?.text()?;
            let nm = if first == "0" && r.len() > 1 {
                let id = r.get(1)?.text()?;
                match Guid::parse(id).and_then(|g| ctx.index.names.get(&g)) {
                    Some(n) => n.clone(),
                    None => format!("StdPicture.{}", std(id)?),
                }
            } else {
                format!("StdPicture.{}", std(first)?)
            };
            let transparent = if l.get(6)?.text()? == "1" {
                "true"
            } else {
                "false"
            };
            Some(Value::Xml(
                XmlElem::new(tag)
                    .child(XmlElem::leaf("xr:Ref", &nm))
                    .child(XmlElem::leaf("xr:LoadTransparent", transparent)),
            ))
        }
        Codec::Shortcut => {
            let l = v.as_list()?;
            let (key, mods) = (int(l.get(1)?)?, int(l.get(2)?)?);
            if key == 0 {
                return Some(Value::Text(String::new()));
            }
            let mut parts: Vec<String> = MODIFIERS
                .iter()
                .filter(|(b, _)| mods & b != 0)
                .map(|(_, n)| n.to_string())
                .collect();
            let key_name = if (KEY_F1..=KEY_F24).contains(&key) {
                format!("F{}", key - KEY_F1 + 1)
            } else if (48..=57).contains(&key) || (65..=90).contains(&key) {
                char::from(u8::try_from(key).ok()?).to_string()
            } else {
                ui.keys.get(&key.to_string())?.clone()
            };
            parts.push(key_name);
            Some(Value::Text(parts.join("+")))
        }
        Codec::Color => color_str(v, ctx.schema).map(Value::Text),
        Codec::Purposes => {
            let l = v.as_list()?;
            let n = count(l.first()?)?;
            let vals = slice(l, 1, n)
                .iter()
                .map(|it| lookup(&PURPOSES, it.as_list()?.get(2)?.text()?))
                .collect::<Option<Vec<_>>>()?;
            let mut e = XmlElem::new(tag);
            for x in vals {
                e = e.child(
                    XmlElem::leaf("v8:Value", x).with_attr("xsi:type", "app:ApplicationUsePurpose"),
                );
            }
            Some(Value::Xml(e))
        }
        Codec::CaContent => {
            let l = v.as_list()?;
            let n = count(l.get(1)?)?;
            let mut e = XmlElem::new(tag);
            for i in 0..n {
                let (_, nm) = name_of(ctx, l.get(2 + 2 * i)?)?;
                let sep = l.get(3 + 2 * i)?.as_list()?;
                let use_ = lookup(&CA_USE, sep.get(1)?.text()?)?;
                let cs = sep.get(2)?.text()?;
                let csn = if cs == ZERO_GUID {
                    None
                } else {
                    Some(ctx.index.names.get(&Guid::parse(cs)?)?.as_str())
                };
                let mut sepe = XmlElem::new("xr:ConditionalSeparation");
                sepe.text = csn.map(str::to_string);
                e = e.child(
                    XmlElem::new("xr:Item")
                        .child(XmlElem::leaf("xr:Metadata", &nm))
                        .child(XmlElem::leaf("xr:Use", use_))
                        .child(sepe),
                );
            }
            Some(Value::Xml(e))
        }
        Codec::Type => type_desc(ctx, v).map(Value::Type),
        Codec::TValue => typed_value(ctx, v).map(Value::Typed),
        Codec::CpLinks => cp_links(ctx, tag, v, owner?).map(Value::Xml),
        Codec::LinkByType => link_by_type(ctx, tag, v, owner?).map(Value::Xml),
        Codec::CParams => cparams(ctx, tag, v).map(Value::Xml),
        Codec::StdAttrs
        | Codec::Characteristics
        | Codec::StyleValue
        | Codec::XdtoType
        | Codec::XdtoPackages => super::bags::decode(ctx, tag, spec, v, owner, path2),
        _ => None,
    }
}

pub(crate) fn cf_text(s: &str) -> std::borrow::Cow<'_, str> {
    if s.contains("\r\n") {
        std::borrow::Cow::Owned(s.replace("\r\n", "\n"))
    } else {
        std::borrow::Cow::Borrowed(s)
    }
}

pub(crate) fn py_str(n: &Node) -> String {
    match n {
        Node::Str(s) | Node::Atom(s) => s.clone(),
        Node::List(_) => {
            let mut out = String::new();
            py_repr(n, &mut out);
            out
        }
    }
}

fn py_repr(n: &Node, out: &mut String) {
    match n {
        Node::Atom(s) => {
            out.push('A');
            str_repr(s, out);
        }
        Node::Str(s) => {
            out.push('S');
            str_repr(s, out);
        }
        Node::List(l) => {
            out.push('[');
            for (i, x) in l.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                py_repr(x, out);
            }
            out.push(']');
        }
    }
}

fn str_repr(s: &str, out: &mut String) {
    use std::fmt::Write;
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    out.push(quote);
    for c in s.chars() {
        let u = u32::from(c);
        match c {
            _ if c == quote || c == '\\' => {
                out.push('\\');
                out.push(c);
            }
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ if u < 0x20 || u == 0x7f => {
                let _ = write!(out, "\\x{u:02x}");
            }
            _ if c.is_ascii() || py_printable(c) => out.push(c),
            _ if u <= 0xff => {
                let _ = write!(out, "\\x{u:02x}");
            }
            _ if u <= 0xffff => {
                let _ = write!(out, "\\u{u:04x}");
            }
            _ => {
                let _ = write!(out, "\\U{u:08x}");
            }
        }
    }
    out.push(quote);
}

pub(crate) fn py_printable(c: char) -> bool {
    const NOT: &[(u32, u32)] = &[
        (0x80, 0xa0),
        (0xad, 0xad),
        (0x600, 0x605),
        (0x61c, 0x61c),
        (0x6dd, 0x6dd),
        (0x70f, 0x70f),
        (0x890, 0x891),
        (0x8e2, 0x8e2),
        (0x1680, 0x1680),
        (0x180e, 0x180e),
        (0x2000, 0x200f),
        (0x2028, 0x202f),
        (0x205f, 0x2064),
        (0x2066, 0x206f),
        (0x3000, 0x3000),
        (0xe000, 0xf8ff),
        (0xfdd0, 0xfdef),
        (0xfeff, 0xfeff),
        (0xfff9, 0xfffb),
        (0x110bd, 0x110bd),
        (0x110cd, 0x110cd),
        (0x13430, 0x1343f),
        (0x1bca0, 0x1bca3),
        (0x1d173, 0x1d17a),
        (0xe0001, 0xe0001),
        (0xe0020, 0xe007f),
        (0xf0000, 0x10ffff),
    ];
    let u = u32::from(c);
    u & 0xfffe != 0xfffe && !NOT.iter().any(|&(a, b)| (a..=b).contains(&u))
}

fn cp_links(ctx: &Ctx, tag: &str, v: &Node, owner: &Owner) -> Option<XmlElem> {
    let l = v.as_list()?;
    let n = count(l.get(1)?)?;
    let mut e = XmlElem::new(tag);
    let mut i = 2;
    for _ in 0..n {
        let name = l.get(i)?.text()?;
        let k = count(l.get(i + 1)?)?;
        let path = field_path(ctx, owner, l.get(i + 2..i + 2 + k)?)?;
        let change = if l.get(i + 2 + k)?.text()? == "1" {
            "DontChange"
        } else {
            "Clear"
        };
        i += 3 + k;
        e = e.child(
            XmlElem::new("xr:Link")
                .child(XmlElem::leaf("xr:Name", name))
                .child(XmlElem::leaf("xr:DataPath", &path).with_attr("xsi:type", "xs:string"))
                .child(XmlElem::leaf("xr:ValueChange", change)),
        );
    }
    Some(e)
}

fn link_by_type(ctx: &Ctx, tag: &str, v: &Node, owner: &Owner) -> Option<XmlElem> {
    let l = v.as_list()?;
    let k = count(l.get(1)?)?;
    if k == 0 {
        return Some(XmlElem::new(tag));
    }
    let path = field_path(ctx, owner, l.get(2..2 + k)?)?;
    let item = l.get(2 + k)?.text()?;
    Some(
        XmlElem::new(tag)
            .child(XmlElem::leaf("xr:DataPath", &path))
            .child(XmlElem::leaf("xr:LinkItem", item)),
    )
}

fn cparams(ctx: &Ctx, tag: &str, v: &Node) -> Option<XmlElem> {
    let l = v.as_list()?;
    let n = count(l.get(1)?)?;
    let mut e = XmlElem::new(tag);
    for i in 0..n {
        let name = l.get(2 + 2 * i)?.text()?;
        let tv = typed_value(ctx, l.get(3 + 2 * i)?)?;
        e = e.child(
            XmlElem::new("app:item")
                .with_attr("name", name)
                .child(typed_elem("app:value", &tv)),
        );
    }
    Some(e)
}

fn lstr_items(v: &Node) -> Option<LocalString> {
    let l = v.as_list()?;
    let n = count(l.first()?)?;
    let items = (0..n)
        .map(|i| {
            Some((
                l.get(1 + 2 * i)?.text()?.to_string(),
                l.get(2 + 2 * i)?.text()?.to_string(),
            ))
        })
        .collect::<Option<Vec<_>>>()?;
    Some(LocalString(items))
}

pub fn color_str(v: &Node, s: &Schema) -> Option<String> {
    let ui = s.ui();
    color_with(v, |kind, val| {
        let (prefix, table) = match kind {
            "1" => ("win", &ui.win_colors),
            "2" => ("web", &ui.web_colors),
            "3" => ("style", &ui.style_colors),
            _ => return None,
        };
        Some(format!("{prefix}:{}", table.get(val)?))
    })
}

pub(crate) fn field_ref(ctx: &Ctx, owner: &Owner, f: &Node) -> Option<String> {
    let l = f.as_list()?;
    let first = l.first()?.text()?;
    if first == "0" && l.len() > 1 {
        return ctx
            .index
            .names
            .get(&Guid::parse(l.get(1)?.text()?)?)
            .cloned();
    }
    let nm = ctx.schema.std_attr(&owner.kind, first)?;
    Some(format!(
        "{}.{}.StandardAttribute.{nm}",
        owner.kind, owner.name
    ))
}

pub(crate) fn field_path(ctx: &Ctx, owner: &Owner, segs: &[Node]) -> Option<String> {
    if let Some(ids) = other_document_ids(ctx, owner, segs) {
        return Some(ids);
    }
    let mut prefix: Option<String> = None;
    for f in segs {
        let l = f.as_list()?;
        let first = l.first()?.text()?;
        let full = if first == "0" && l.len() > 1 {
            ctx.index
                .names
                .get(&Guid::parse(l.get(1)?.text()?)?)?
                .clone()
        } else if let Some(p) = &prefix {
            let mut it = p.rsplit('.');
            it.next()?;
            let nm = ctx.schema.std_attr(it.next()?, first)?;
            format!("{p}.StandardAttribute.{nm}")
        } else {
            field_ref(ctx, owner, f)?
        };
        prefix = Some(full);
    }
    prefix
}

fn other_document_ids(ctx: &Ctx, owner: &Owner, segs: &[Node]) -> Option<String> {
    let mut ids = Vec::new();
    let mut last = None;
    for f in segs {
        let l = f.as_list()?;
        if l.first()?.text()? != "0" || l.len() != 2 {
            return None;
        }
        let id = Guid::parse(l.get(1)?.text()?)?;
        last = ctx.index.names.get(&id);
        ids.push(format!("0:{id}"));
    }
    let mut parts = last?.split('.');
    let (kind, name) = (parts.next()?, parts.next()?);
    (kind == "Document" && !(owner.kind == "Document" && owner.name == name)).then(|| ids.join("/"))
}
