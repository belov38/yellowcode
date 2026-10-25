use super::{
    ObjectRights, Restriction, Right, RightTarget, RightValue, Rights, fetch, int, miss, slice,
    uint,
};
use crate::entries::EntryData;
use crate::load::LoadCtx;
use crate::{Configuration, MdKind, Result};
use brace::{Guid, Node};
use mdschema::Schema;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Miss {
    Unresolved,
    UnknownRight(String),
    UnresolvedField,
    Malformed,
}

const NOT_BRACE: &str = "rights:not_brace";

impl Miss {
    fn key(&self) -> String {
        match self {
            Miss::Unresolved => "rights:unresolved_object".into(),
            Miss::UnknownRight(g) => format!("rights:unknown_right:{g}"),
            Miss::UnresolvedField => "rights:unresolved_field".into(),
            Miss::Malformed => "rights:malformed".into(),
        }
    }
}

type Decoded<T> = std::result::Result<T, Miss>;

fn list(n: Option<&Node>) -> Decoded<&[Node]> {
    n.and_then(Node::as_list).ok_or(Miss::Malformed)
}

fn counted<'a>(l: &'a [Node], from: usize, count: Option<&Node>) -> Decoded<&'a [Node]> {
    let n = count.and_then(uint).ok_or(Miss::Malformed)?;
    Ok(slice(l, from, n))
}

fn bool3(n: Option<&Node>) -> Decoded<bool> {
    match n.and_then(Node::text) {
        Some("1") => Ok(true),
        Some("-1" | "4294967295" | "0") => Ok(false),
        _ => Err(Miss::Malformed),
    }
}

fn brace_text(n: Option<&Node>) -> Decoded<String> {
    Ok(n.and_then(Node::text)
        .ok_or(Miss::Malformed)?
        .replace("\r\n", "\n"))
}

struct Resolved {
    target: RightTarget,
    full_name: String,
}

fn resolve(
    cfg: &Configuration,
    schema: &Schema,
    cfg_full: &str,
    objref: Option<&Node>,
) -> Decoded<Resolved> {
    let r = list(objref)?;
    let id = r
        .get(1)
        .ok_or(Miss::Malformed)?
        .text()
        .and_then(Guid::parse);
    let sa = r.get(2).ok_or(Miss::Malformed)?.text() == Some("1");
    let id = id.ok_or(Miss::Unresolved)?;
    let own = if id == cfg.id {
        cfg_full
    } else {
        cfg.full_name(id).ok_or(Miss::Unresolved)?
    };
    if sa {
        let kind = if own.split('.').count() >= 4 {
            own.rsplit('.').nth(1)
        } else {
            own.split('.').next()
        };
        let sid = r
            .get(3)
            .and_then(|n| n.at(0))
            .and_then(Node::text)
            .ok_or(Miss::Malformed)?;
        let nm = kind
            .and_then(|k| schema.std_attr(k, sid))
            .ok_or(Miss::Unresolved)?;
        let sid: i32 = sid.trim().parse().map_err(|_| Miss::Malformed)?;
        return Ok(Resolved {
            target: RightTarget::StandardAttribute { owner: id, id: sid },
            full_name: format!("{own}.StandardAttribute.{nm}"),
        });
    }
    let target = if id == cfg.id {
        RightTarget::Configuration
    } else if cfg.index.objs.contains_key(&id) {
        RightTarget::Object(id)
    } else {
        RightTarget::Child {
            id,
            field: r.get(3).and_then(Node::text) == Some("1"),
        }
    };
    Ok(Resolved {
        target,
        full_name: own.to_string(),
    })
}

struct RawRls<'a> {
    cond: &'a str,
    fields: &'a [Node],
}

fn pairs(prs: &[Node]) -> Vec<(&Node, &Node)> {
    prs.as_chunks::<2>()
        .0
        .iter()
        .filter_map(|c| Some((c.first()?, c.get(1)?)))
        .collect()
}

type RightList<'a> = (Vec<(&'a Node, &'a Node)>, HashMap<&'a str, Vec<RawRls<'a>>>);

fn parse_rightlist(rl: Option<&Node>) -> Decoded<RightList<'_>> {
    let l = list(rl)?;
    if l.first().ok_or(Miss::Malformed)?.text() == Some("0") {
        return Ok((pairs(l.get(1..).unwrap_or_default()), HashMap::new()));
    }
    let n = l
        .get(1)
        .and_then(int)
        .and_then(|n| usize::try_from(n).ok())
        .ok_or(Miss::Malformed)?;
    let end = n
        .checked_mul(2)
        .and_then(|x| x.checked_add(2))
        .ok_or(Miss::Malformed)?;
    let prs = l.get(2..end).ok_or(Miss::Malformed)?;
    let mut restr = HashMap::new();
    let rest = l.get(end..).unwrap_or_default();
    if !rest.is_empty() {
        for r in counted(rest, 1, rest.first())? {
            let r = list(Some(r))?;
            let key = r.first().and_then(Node::text).ok_or(Miss::Malformed)?;
            let lst = list(r.get(1))?;
            let mut items = Vec::new();
            let listed = counted(lst, 1, lst.first())?;
            if listed.is_empty() {
                items.push(RawRls {
                    cond: "",
                    fields: &[],
                });
            }
            for rls in listed {
                let rls = list(Some(rls))?;
                let nf = match rls.get(2) {
                    Some(n) => uint(n).ok_or(Miss::Malformed)?,
                    None => 0,
                };
                let cond = rls.get(1).and_then(Node::text).ok_or(Miss::Malformed)?;
                items.push(RawRls {
                    cond,
                    fields: slice(rls, 3, nf),
                });
            }
            restr.insert(key, items);
        }
    }
    Ok((pairs(prs), restr))
}

fn rls_field(cfg: &Configuration, schema: &Schema, f: &Node, objname: &str) -> Decoded<String> {
    let r = match f.at(1) {
        Some(n) if n.is_list() => n,
        _ => f,
    };
    let first = r.at(0).and_then(Node::text).ok_or(Miss::Malformed)?;
    if let (true, Some(u)) = (first == "0", r.at(1)) {
        return u
            .text()
            .and_then(Guid::parse)
            .and_then(|id| cfg.full_name(id))
            .map(str::to_string)
            .ok_or(Miss::UnresolvedField);
    }
    let kind = objname.split('.').next().unwrap_or_default();
    let nm = schema.std_attr(kind, first).ok_or(Miss::UnresolvedField)?;
    Ok(format!("{objname}.StandardAttribute.{nm}"))
}

fn restriction(
    cfg: &Configuration,
    schema: &Schema,
    objname: &str,
    it: &RawRls,
) -> Decoded<Restriction> {
    let condition = it.cond.replace("\r\n", "\n");
    if it.cond.is_empty() && it.fields.is_empty() {
        return Ok(Restriction {
            condition,
            fields: Vec::new(),
        });
    }
    let mut regular = Vec::new();
    let mut standard = Vec::new();
    for f in it.fields {
        match f.get(&[1, 0]).and_then(Node::text) {
            Some("0") => regular.push(f),
            Some(_) => standard.push(f),
            None => return Err(Miss::Malformed),
        }
    }
    let fields = regular
        .into_iter()
        .chain(standard)
        .map(|f| rls_field(cfg, schema, f, objname))
        .collect::<Decoded<Vec<_>>>()?;
    Ok(Restriction { condition, fields })
}

fn object_rights(
    cfg: &Configuration,
    schema: &Schema,
    res: &Resolved,
    rl: &Node,
    default: bool,
) -> Decoded<Vec<RightValue>> {
    let (pairs, restr) = parse_rightlist(Some(rl))?;
    let mut keyed = Vec::with_capacity(pairs.len());
    for (g, v) in pairs {
        keyed.push((g.text().ok_or(Miss::Malformed)?, v.text() == Some("1")));
    }
    let dropped: HashSet<&str> = keyed
        .iter()
        .filter(|(g, v)| *v == default && !restr.contains_key(g))
        .map(|(g, _)| *g)
        .collect();
    let mut out = Vec::with_capacity(keyed.len());
    for (g, value) in keyed {
        let right = Guid::parse(g).and_then(Right::from_guid);
        if dropped.contains(g) {
            if let Some(right) = right {
                out.push(RightValue {
                    right,
                    value,
                    restrictions: Vec::new(),
                });
            }
            continue;
        }
        let right = right.ok_or_else(|| Miss::UnknownRight(g.to_string()))?;
        let restrictions = match restr.get(g) {
            Some(items) => items
                .iter()
                .map(|it| restriction(cfg, schema, &res.full_name, it))
                .collect::<Decoded<Vec<_>>>()?,
            None => Vec::new(),
        };
        out.push(RightValue {
            right,
            value,
            restrictions,
        });
    }
    Ok(out)
}

fn decode(cfg: &Configuration, t: &Node, keep_source: bool) -> Decoded<Rights> {
    let schema = mdschema::schema().map_err(|_| Miss::Malformed)?;
    let cfg_full = format!("Configuration.{}", cfg.settings.name);
    let set_for_new_objects = bool3(t.at(3))?;
    let set_for_attributes_by_default = bool3(t.at(4))?;
    let independent_rights_of_child_objects = bool3(t.at(5))?;
    let lst = list(t.at(1))?;
    let mut resolved = Vec::new();
    for o in counted(lst, 1, lst.first())? {
        let o = list(Some(o))?;
        let res = resolve(cfg, schema, &cfg_full, o.first())?;
        resolved.push((res, o.get(1).ok_or(Miss::Malformed)?));
    }
    let mut order: Vec<((Guid, u8, i32), usize)> = resolved
        .iter()
        .enumerate()
        .map(|(i, (res, _))| (res.target.sort_key(cfg.id), i))
        .collect();
    order.sort_by_key(|(k, _)| *k);
    let mut rights: Vec<Vec<RightValue>> = vec![Vec::new(); resolved.len()];
    for (_, i) in order {
        let (Some((res, rl)), Some(slot)) = (resolved.get(i), rights.get_mut(i)) else {
            continue;
        };
        let default = if res.target.is_field() {
            set_for_attributes_by_default
        } else {
            set_for_new_objects
        };
        *slot = object_rights(cfg, schema, res, rl, default)?;
    }
    let objects = resolved
        .into_iter()
        .zip(rights)
        .map(|((res, _), rights)| ObjectRights {
            target: res.target,
            full_name: res.full_name,
            rights,
        })
        .collect();
    let tl = list(t.at(2))?;
    let mut templates = Vec::new();
    for it in counted(tl, 1, tl.first())? {
        let it = list(Some(it))?;
        templates.push((brace_text(it.first())?, brace_text(it.get(1))?));
    }
    Ok(Rights {
        set_for_new_objects,
        set_for_attributes_by_default,
        independent_rights_of_child_objects,
        objects,
        templates,
        source: if keep_source {
            t.clone()
        } else {
            Node::empty()
        },
    })
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn decode_rights(cfg: &Configuration, entry: &Node) -> Option<Rights> {
    decode(cfg, entry, true).ok()
}

pub(crate) fn load(cx: &LoadCtx, cfg: &mut Configuration) -> Result<()> {
    for i in 0..cfg.objects.len() {
        let Some(role) = cfg.objects.get(i).filter(|o| o.kind == MdKind::Role) else {
            continue;
        };
        let name = format!("{}.0", role.id);
        let data = fetch(cx, &name)?;
        let res = match data.as_deref() {
            Some(EntryData::Brace(doc)) => decode(cfg, &doc.root, cx.opts.keep_source),
            _ if !cx.entries.contains(&name) => continue,
            _ => {
                miss(cfg, &[NOT_BRACE]);
                continue;
            }
        };
        match res {
            Ok(r) => {
                if let Some(o) = cfg.objects.get_mut(i) {
                    o.parts.rights = Some(r);
                }
            }
            Err(m) => miss(cfg, &[&m.key()]),
        }
    }
    Ok(())
}
