use super::codecs::{Ctx, Owner, cf_text, count, decode as decode_prop, int, slice};
use super::decode::description_value;
use super::ui::{border, color_text, font_attrs};
use crate::model::{Prop, StandardAttribute, Value, XmlElem};
use crate::xml_out::{NS_DECL, typed_elem, value_elem};
use brace::{Guid, Node};
use mdschema::{Codec, PropSpec};

const T_DTREF: &str = "5c14e26f-099b-4d37-84a6-b433d87400da";
const T_FIXARR: &str = "4500381b-db30-4a10-9db4-990038032acf";
const T_CHARACT: &str = "fe839d42-d094-40ba-b903-75bccc21ba30";
const T_MDREF: &str = "157fa490-4ce9-11d4-9415-008048da11f9";
const T_COLOR: &str = "9cd510c7-abfc-11d4-9434-004095e12fc7";
const T_FONT: &str = "9cd510c8-abfc-11d4-9434-004095e12fc7";
const T_BORDER: &str = "4d10ca00-111a-4d43-9c96-92cd773716de";
const T_ACCOUNTTYPE: &str = "872f7198-7083-4e3e-b57e-a2a9802c769e";

pub(crate) fn decode(
    ctx: &Ctx,
    tag: &str,
    spec: &PropSpec,
    v: &Node,
    owner: Option<&Owner>,
    _path2: Option<&str>,
) -> Option<Value> {
    match spec.codec {
        Codec::StdAttrs => {
            let own;
            let owner = match &spec.std {
                Some(k) if !k.is_empty() => {
                    own = Owner {
                        kind: k.clone(),
                        name: String::new(),
                    };
                    Some(&own)
                }
                _ => owner,
            };
            std_attrs(ctx, v, owner).map(Value::StandardAttributes)
        }
        Codec::Characteristics => characteristics(ctx, tag, v).map(Value::Xml),
        Codec::StyleValue => style_value(ctx, tag, v).map(Value::Xml),
        Codec::XdtoType => xdto_type(tag, v).map(Value::Xml),
        Codec::XdtoPackages => xdto_packages(ctx, tag, v).map(Value::Xml),
        _ => None,
    }
}

fn bag_payload(v: &Node) -> &Node {
    match v.as_list() {
        Some([a, t, p])
            if a.text() == Some("#") && !matches!(t.text(), Some(T_DTREF | T_FIXARR)) =>
        {
            p
        }
        _ => v,
    }
}

fn second_or_self(v: &Node) -> Option<&Node> {
    match v.as_list() {
        Some(l) => l.get(1),
        None => Some(v),
    }
}

fn std_attrs(ctx: &Ctx, v: &Node, owner: Option<&Owner>) -> Option<Vec<StandardAttribute>> {
    let [_, inner] = v.as_list()? else {
        return None;
    };
    let kind = owner.map_or("", |o| o.kind.as_str());
    std_attr_list(ctx, inner.as_list()?, kind, owner)
}

pub(crate) fn std_attr_list(
    ctx: &Ctx,
    inner: &[Node],
    kind: &str,
    owner: Option<&Owner>,
) -> Option<Vec<StandardAttribute>> {
    let n = count(inner.get(1)?)?;
    let mut out = Vec::new();
    for i in 0..n {
        let id = inner.get(2 + 3 * i)?.as_list()?;
        let sid = id.first()?.text()?;
        let pair = id
            .get(1)
            .and_then(Node::text)
            .and_then(|class| super::overlay::ext_dimension_attr(kind, sid, class));
        let name: String = match pair {
            Some(n) => n,
            None => ctx
                .schema
                .std_attr(kind, sid)
                .or_else(|| super::overlay::std_attr(kind, sid))?
                .to_string(),
        };
        let bag = inner.get(4 + 3 * i)?.as_list()?;
        let k = count(bag.get(1)?)?;
        let mut props = Vec::new();
        for j in 0..k {
            let pg = bag.get(2 + 2 * j)?.text()?;
            let val = bag.get(3 + 2 * j)?;
            let pe = ctx.schema.bag_prop(pg)?;
            let spec = PropSpec {
                codec: pe.codec,
                map: pe.map.clone(),
                ..Default::default()
            };
            let tag = format!("xr:{}", pe.xml);
            let value = match pe.codec {
                Codec::TValue => match account_type(&tag, val) {
                    Some(v) => v,
                    None => decode_prop(ctx, &tag, &spec, val, owner, None)?,
                },
                Codec::Bool => Value::Bool(second_or_self(val)?.text() == Some("1")),
                Codec::Str => decode_prop(ctx, &tag, &spec, second_or_self(val)?, owner, None)?,
                Codec::Enum | Codec::Ref => {
                    let p = second_or_self(bag_payload(val))?;
                    decode_prop(ctx, &tag, &spec, p, owner, None)?
                }
                _ => decode_prop(ctx, &tag, &spec, bag_payload(val), owner, None)?,
            };
            props.push(Prop {
                name: pe.xml.clone(),
                codec: pe.codec,
                value,
            });
        }
        out.push(StandardAttribute { name, props });
    }
    Some(out)
}

fn account_type(tag: &str, v: &Node) -> Option<Value> {
    let [a, t, code] = v.as_list()? else {
        return None;
    };
    if a.text()? != "#" || t.text()? != T_ACCOUNTTYPE {
        return None;
    }
    let name = match code.text()? {
        "0" => "Active",
        "1" => "Passive",
        "2" => "ActivePassive",
        _ => return None,
    };
    Some(Value::Xml(
        XmlElem::leaf(tag, name).with_attr("xsi:type", "ent:AccountType"),
    ))
}

pub(crate) fn std_tabular_sections(ctx: &Ctx, owner: &Owner, v: &Node) -> Option<Value> {
    let [flag, body] = v.as_list()? else {
        return None;
    };
    if flag.text()? != "1" {
        return None;
    }
    let body = body.as_list()?;
    let n = count(body.get(1)?)?;
    let mut root = XmlElem::new("StandardTabularSections");
    for i in 0..n {
        let name = super::overlay::std_section(&owner.kind, body.get(2 + 2 * i)?.text()?)?;
        let sec = body.get(3 + 2 * i)?.as_list()?;
        let (syn, comment, fill, attrs, tip) = (
            sec.get(1)?,
            sec.get(2)?,
            sec.get(3)?,
            sec.get(5)?,
            sec.get(6)?,
        );
        let lstr = |tag: &str, node: &Node| -> Option<XmlElem> {
            let spec = PropSpec {
                codec: Codec::LStr,
                ..Default::default()
            };
            let value = decode_prop(ctx, tag, &spec, node, Some(owner), None)?;
            Some(value_elem(tag, &description_value(value)))
        };
        if sec.get(4)?.text()? != "0" {
            return None;
        }
        let fill = match fill.text()? {
            "0" => "DontCheck",
            "1" => "ShowError",
            _ => return None,
        };
        let kind = format!("{}.{name}", owner.kind);
        let list = std_attr_list(ctx, attrs.as_list()?, &kind, Some(owner))?;
        let sa = section_attributes_elem(list);
        let comment = XmlElem::leaf("xr:Comment", &cf_text(comment.text()?));
        root = root.child(
            XmlElem::new("xr:StandardTabularSection")
                .with_attr("name", name)
                .child(lstr("xr:Synonym", syn)?)
                .child(comment)
                .child(lstr("xr:ToolTip", tip)?)
                .child(XmlElem::leaf("xr:FillChecking", fill))
                .child(sa),
        );
    }
    Some(Value::Xml(root.expanded()))
}

pub(crate) fn section_attributes_elem(list: Vec<StandardAttribute>) -> XmlElem {
    let mut sa = XmlElem::new("xr:StandardAttributes").expanded();
    for a in list {
        let mut e = XmlElem::new("xr:StandardAttribute").with_attr("name", &a.name);
        for p in a.props {
            let tag = format!("xr:{}", p.name);
            e = e.child(value_elem(&tag, &description_value(p.value)));
        }
        sa = sa.child(e);
    }
    sa
}

pub(super) fn char_field(ctx: &Ctx, f: &Node, owner_full: &str) -> Option<String> {
    let r = f.as_list()?.get(1)?.as_list()?;
    let first = r.first()?.text()?;
    if first == "-1" {
        return Some("-1".into());
    }
    if first == "0" {
        return match r.get(1) {
            Some(x) => ctx.index.names.get(&Guid::parse(x.text()?)?).cloned(),
            None => Some("0".into()),
        };
    }
    let parts: Vec<&str> = owner_full.split('.').collect();
    let kind = if parts.len() >= 4 {
        parts.get(parts.len() - 2)?
    } else {
        parts.first()?
    };
    let mut nm = ctx.schema.std_attr(kind, first);
    if nm.is_none() && *kind == "TabularSection" {
        nm = ctx.schema.std_attr(parts.first()?, first);
    }
    Some(format!("{owner_full}.StandardAttribute.{}", nm?))
}

fn characteristics(ctx: &Ctx, tag: &str, v: &Node) -> Option<XmlElem> {
    let items = v.as_list()?.get(1)?.as_list()?;
    let n = count(items.first()?)?;
    let mut e = XmlElem::new(tag);
    for it in slice(items, 1, n) {
        let it = it.as_list()?;
        if it.get(1)?.text()? != T_CHARACT {
            return None;
        }
        let c = it.get(2)?.as_list()?;
        let from = |i: usize| -> Option<String> {
            let g = Guid::parse(c.get(i)?.as_list()?.get(1)?.text()?)?;
            ctx.index.names.get(&g).cloned()
        };
        let (tfrom, vfrom) = (from(1)?, from(2)?);
        let fld = |i: usize, owner_full: &str| char_field(ctx, c.get(i)?, owner_full);
        let key = fld(8, &tfrom)?;
        let types_filter = fld(6, &tfrom)?;
        let data_path = fld(9, &tfrom)?;
        let multi_use = fld(10, &tfrom)?;
        let object = fld(3, &vfrom)?;
        let type_ = fld(4, &vfrom)?;
        let value = fld(5, &vfrom)?;
        let multi_key = fld(11, &vfrom)?;
        let multi_order = fld(12, &vfrom)?;
        let tv = super::types::typed_value(ctx, c.get(7)?)?;
        let leaf = |name: &str, text: &str| XmlElem::leaf(name, text);
        let types = XmlElem::new("xr:CharacteristicTypes")
            .with_attr("from", &tfrom)
            .child(leaf("xr:KeyField", &key))
            .child(leaf("xr:TypesFilterField", &types_filter))
            .child(typed_elem("xr:TypesFilterValue", &tv))
            .child(leaf("xr:DataPathField", &data_path))
            .child(leaf("xr:MultipleValuesUseField", &multi_use));
        let values = XmlElem::new("xr:CharacteristicValues")
            .with_attr("from", &vfrom)
            .child(leaf("xr:ObjectField", &object))
            .child(leaf("xr:TypeField", &type_))
            .child(leaf("xr:ValueField", &value))
            .child(leaf("xr:MultipleValuesKeyField", &multi_key))
            .child(leaf("xr:MultipleValuesOrderField", &multi_order));
        e = e.child(XmlElem::new("xr:Characteristic").child(types).child(values));
    }
    Some(e)
}

fn style_value(ctx: &Ctx, tag: &str, v: &Node) -> Option<XmlElem> {
    let l = v.as_list()?;
    if l.len() <= 3 {
        return None;
    }
    let payload = l.get(3)?;
    let mut e = XmlElem::new(tag);
    match l.get(1)?.text()? {
        T_COLOR => {
            e = e.with_attr("xsi:type", "v8ui:Color");
            e.text = Some(color_text(payload, ctx.schema)?);
        }
        T_BORDER => {
            let (attrs, kids) = border(payload, ctx.schema)?;
            e = e.with_attr("xsi:type", "v8ui:Border");
            for (k, val) in &attrs {
                e = e.with_attr(k, val);
            }
            for (ct, xt, text) in &kids {
                e = e.child(XmlElem::leaf(ct, text).with_attr("xsi:type", xt));
            }
        }
        T_FONT => {
            e = e.with_attr("xsi:type", "v8ui:Font");
            for (k, val) in &font_attrs(payload, ctx.schema)? {
                e = e.with_attr(k, val);
            }
        }
        _ => return None,
    }
    Some(e)
}

fn ns_prefix(uri: &str) -> Option<&'static str> {
    NS_DECL.split_whitespace().find_map(|d| {
        let (p, u) = d.strip_prefix("xmlns:")?.split_once('=')?;
        (u.trim_matches('"') == uri).then_some(p)
    })
}

fn xdto_type(tag: &str, v: &Node) -> Option<XmlElem> {
    let l = v.as_list()?;
    if l.len() < 3 {
        return None;
    }
    let (ns, name) = (l.get(1)?.text()?, l.get(2)?.text()?);
    if ns.is_empty() {
        return Some(XmlElem::leaf(tag, name));
    }
    if let Some(p) = ns_prefix(ns) {
        return Some(XmlElem::leaf(tag, &format!("{p}:{name}")));
    }
    let depth = if tag == "XDTOReturningValueType" {
        6
    } else {
        8
    };
    let p = format!("d{depth}p1");
    Some(XmlElem::leaf(tag, &format!("{p}:{name}")).with_attr(&format!("xmlns:{p}"), ns))
}

fn xdto_packages(ctx: &Ctx, tag: &str, v: &Node) -> Option<XmlElem> {
    let l = v.as_list()?;
    let refs = l.get(3)?.as_list()?;
    let uris = l.get(5)?.as_list()?;
    let n = count(refs.get(1)?)?;
    let mut items: Vec<(&str, String)> = Vec::new();
    for it in slice(refs, 2, n) {
        let it = it.as_list()?;
        if it.first()?.text()? != "#" || it.get(1)?.text()? != T_MDREF {
            return None;
        }
        let g = Guid::parse(it.get(2)?.as_list()?.get(1)?.text()?)?;
        items.push(("xr:MDObjectRef", ctx.index.names.get(&g)?.clone()));
    }
    let k = usize::try_from(int(uris.first()?)?).ok()?;
    for x in slice(uris, 1, k) {
        items.push(("xs:string", x.text()?.to_string()));
    }
    let mut e = XmlElem::new(tag);
    for (xt, nm) in items {
        e = e.child(
            XmlElem::new("xr:Item")
                .child(XmlElem::new("xr:Presentation"))
                .child(XmlElem::leaf("xr:CheckState", "0"))
                .child(XmlElem::leaf("xr:Value", &nm).with_attr("xsi:type", xt)),
        );
    }
    Some(e)
}
