use super::{fetch, int, miss, slice, uint};
use crate::entries::EntryData;
use crate::load::LoadCtx;
use crate::model::{Prop, Value, XmlElem};
use crate::objects::codecs::{self, Ctx};
use crate::{CompatibilityMode, Configuration, Result};
use brace::{Guid, Node};
use mdschema::{Codec, ConfigPath, ConfigPropSpec, PropSpec, Schema};

type R<T> = std::result::Result<T, String>;

type Contained = Vec<(Guid, Option<Guid>)>;

const PART2: [usize; 5] = [4, 1, 1, 1, 1];

fn first_object_id(node: &Node) -> Option<&str> {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        let Some(l) = n.as_list() else { continue };
        if let [a, b, c] = l
            && a.text() == Some("1")
            && b.text() == Some("0")
            && c.text().is_some_and(|s| s.chars().count() == 36)
        {
            return c.text();
        }
        stack.extend(l.iter().filter(|x| x.is_list()).rev());
    }
    None
}

fn contained_objects(t: &Node) -> R<Contained> {
    let bad = || "internal_info".to_string();
    let l = t.as_list().ok_or_else(bad)?;
    let n = uint(l.get(2).ok_or_else(bad)?).ok_or_else(bad)?;
    slice(l, 3, n)
        .iter()
        .map(|part| {
            let p = part.as_list().ok_or_else(bad)?;
            let class = p
                .first()
                .and_then(Node::text)
                .and_then(Guid::parse)
                .ok_or_else(bad)?;
            let object = match first_object_id(p.get(1).ok_or_else(bad)?) {
                Some(s) => Some(Guid::parse(s).ok_or_else(bad)?),
                None => None,
            };
            Ok((class, object))
        })
        .collect()
}

fn prop(name: &str, codec: Codec, value: Value) -> Prop {
    Prop {
        name: name.to_string(),
        codec,
        value,
    }
}

fn simple_codec(name: &str) -> Option<Codec> {
    Some(match name {
        "bool" => Codec::Bool,
        "int" => Codec::Int,
        "str" => Codec::Str,
        "lstr" => Codec::LStr,
        "ref" => Codec::Ref,
        "mdlist" => Codec::MdList,
        "purposes" => Codec::Purposes,
        _ => return None,
    })
}

fn simple(ctx: &Ctx, tag: &str, codec: Codec, v: Option<&Node>) -> Option<Value> {
    let spec = PropSpec {
        codec,
        ..Default::default()
    };
    codecs::decode(ctx, tag, &spec, v?, None, None)
}

fn functionalities(spec: &ConfigPropSpec, v: Option<&Node>) -> R<Value> {
    let name = spec.name.as_str();
    let bad = |why: &str| format!("{name}:{why}");
    let l = v.and_then(Node::as_list).ok_or_else(|| bad("value"))?;
    let n = l.get(1).and_then(uint).ok_or_else(|| bad("value"))?;
    let mut e = XmlElem::new(name);
    for it in slice(l, 2, n) {
        let it = it.as_list().ok_or_else(|| bad("value"))?;
        let id = it
            .first()
            .and_then(Node::text)
            .ok_or_else(|| bad("value"))?;
        let use_ = it.get(1).ok_or_else(|| bad("value"))?.text() == Some("1");
        let f = spec
            .extra
            .get("map")
            .and_then(|m| m.get(id))
            .and_then(|x| x.as_str())
            .ok_or_else(|| bad(&format!("functionality:{id}")))?;
        e = e.child(
            XmlElem::new("app:functionality")
                .child(XmlElem::leaf("app:functionality", f))
                .child(XmlElem::leaf(
                    "app:use",
                    if use_ { "true" } else { "false" },
                )),
        );
    }
    Ok(Value::Xml(e))
}

fn strlist(name: &str, v: Option<&Node>) -> R<Value> {
    let bad = || format!("{name}:value");
    let l = v.and_then(Node::as_list).ok_or_else(bad)?;
    let n = l.first().and_then(uint).ok_or_else(bad)?;
    let mut e = XmlElem::new(name);
    for x in slice(l, 1, n) {
        let s = match x {
            Node::List(_) => x.at(1).and_then(Node::text),
            _ => x.text(),
        }
        .ok_or_else(bad)?;
        e = e.child(XmlElem::leaf("v8:Value", s).with_attr("xsi:type", "xs:string"));
    }
    Ok(Value::Xml(e))
}

fn value(ctx: &Ctx, spec: &ConfigPropSpec, v: Option<&Node>) -> R<Option<Prop>> {
    let name = spec.name.as_str();
    let codec_miss = || format!("{name}:{}", spec.codec);
    if let Some(codec) = simple_codec(&spec.codec) {
        let val = simple(ctx, name, codec, v).ok_or_else(codec_miss)?;
        return Ok(Some(prop(name, codec, val)));
    }
    let p = match spec.codec.as_str() {
        "enum" => {
            let code = v.and_then(Node::text).ok_or_else(codec_miss)?;
            let lit = spec
                .extra
                .get("map")
                .and_then(|m| m.get(code))
                .and_then(|x| x.as_str())
                .ok_or_else(|| format!("{name}:enum:{code}"))?;
            if spec.extra.get("omit").and_then(|x| x.as_str()) == Some(code) {
                return Ok(None);
            }
            prop(name, Codec::Enum, Value::Text(lit.to_string()))
        }
        "const" => {
            let lit = spec
                .extra
                .get("value")
                .and_then(|x| x.as_str())
                .ok_or_else(codec_miss)?;
            prop(name, Codec::Enum, Value::Text(lit.to_string()))
        }
        "compat" => {
            let n = v
                .and_then(int)
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(codec_miss)?;
            prop(
                name,
                Codec::Enum,
                Value::Text(CompatibilityMode(n).xml_name()),
            )
        }
        "functionalities" => prop(name, Codec::Unknown, functionalities(spec, v)?),
        "empty" => prop(name, Codec::Unknown, Value::Xml(XmlElem::new(name))),
        "strlist" => prop(name, Codec::Unknown, strlist(name, v)?),
        other => return Err(format!("{name}:codec:{other}")),
    };
    Ok(Some(p))
}

fn decode(
    schema: &'static Schema,
    cfg: &Configuration,
    t: &Node,
    misses: &mut Vec<String>,
) -> R<(Contained, Vec<Prop>)> {
    let contained = contained_objects(t)?;
    let body = t.get(&[3, 1, 1]).ok_or("body")?;
    let hdr = body.get(&[1, 1]).ok_or("header")?;
    let at = |i: usize| hdr.at(i).ok_or_else(|| "header".to_string());
    let text_at = |i: usize| -> R<String> {
        Ok(at(i)?
            .text()
            .ok_or_else(|| "header".to_string())?
            .to_string())
    };
    let ctx = Ctx {
        schema,
        index: &cfg.index,
        predef: &cfg.predef,
    };
    let mut props = vec![prop("Name", Codec::Str, Value::Text(text_at(2)?))];
    match simple(&ctx, "Synonym", Codec::LStr, Some(at(3)?)) {
        Some(v) => props.push(prop("Synonym", Codec::LStr, v)),
        None => misses.push("Synonym:lstr".into()),
    }
    props.push(prop("Comment", Codec::Str, Value::Text(text_at(4)?)));
    for spec in schema.config_props() {
        let v = match &spec.path {
            Some(ConfigPath::Body(p)) => Some(body.get(p)),
            Some(ConfigPath::Part2) => Some(t.get(&PART2)),
            None => None,
        };
        let v = match v {
            Some(None) => return Err(format!("{}:no_value", spec.name)),
            Some(v) => v,
            None => None,
        };
        if let Some(p) = value(&ctx, spec, v)? {
            props.push(p);
        }
    }
    Ok((contained, props))
}

pub(crate) fn decode_configuration(cx: &LoadCtx, cfg: &mut Configuration) -> Result<()> {
    let name = cfg.id.to_string();
    let data = fetch(cx, &name)?;
    let t = match data.as_deref() {
        Some(EntryData::Brace(doc)) => &doc.root,
        _ if !cx.entries.contains(&name) => {
            miss(cfg, &["configuration", "no_entry"]);
            return Ok(());
        }
        _ => {
            miss(cfg, &["configuration", "not_brace"]);
            return Ok(());
        }
    };
    let mut misses = Vec::new();
    match decode(cx.schema, cfg, t, &mut misses) {
        Ok((contained, props)) => {
            cfg.parts.contained_objects = contained;
            cfg.props = props;
        }
        Err(why) => {
            cfg.parts.contained_objects.clear();
            cfg.props.clear();
            misses.push(why);
        }
    }
    for m in misses {
        miss(cfg, &["configuration", &m]);
    }
    Ok(())
}
