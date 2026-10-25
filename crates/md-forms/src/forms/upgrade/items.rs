use brace::Node;
use mdschema::forms_upgrade::{Context, Domain, Op};

use super::{Cx, Refusal, at, created, fields, groups};
use crate::forms::decode::Item;
use crate::forms::variant::TABLE_BAG_AT;

pub(crate) struct Env<'a> {
    pub class: &'a str,
    pub body: &'a [Node],
    pub kind: &'a str,
}

fn context_name(it: &Item) -> Result<String, Refusal> {
    Ok(match it.class {
        Some(c) => format!("{c}/{}", it.version),
        None => match it.version.as_str() {
            "22" => "Nested22/22".to_string(),
            "11" | "12" => format!("Tooltip/{}", it.version),
            "5" | "6" => format!("Addition/{}", it.version),
            _ => {
                return Err(Refusal::NoRule {
                    class: "nested".into(),
                    version: it.version.clone(),
                });
            }
        },
    })
}

pub(crate) fn check_domain(op: &Op, v: &Node) -> Result<(), Refusal> {
    check_in(op.name.as_deref().unwrap_or(""), op.domain.as_ref(), v)
}

fn check_inputs(op: &Op, body: &[Node]) -> Result<(), Refusal> {
    for (i, p) in op.inputs.iter().enumerate() {
        check_in(
            op.name.as_deref().unwrap_or(""),
            op.domains.get(i),
            at(body, *p, "derived input")?,
        )?;
    }
    Ok(())
}

fn check_in(name: &str, domain: Option<&Domain>, v: &Node) -> Result<(), Refusal> {
    let refuse = || Refusal::Domain {
        op: name.to_string(),
        value: v.text().unwrap_or("{...}").to_string(),
    };
    match domain {
        None | Some(Domain::Atom) => {
            if v.is_list() {
                return Err(refuse());
            }
        }
        Some(Domain::Int) => {
            v.as_i64().ok_or_else(refuse)?;
        }
        Some(Domain::List) => {
            if !v.is_list() {
                return Err(refuse());
            }
        }
        Some(Domain::Set(set)) => {
            let n = v.as_i64().ok_or_else(refuse)?;
            if !set.contains(&n) {
                return Err(refuse());
            }
        }
    }
    Ok(())
}

pub(crate) fn xf(name: &str, v: &Node) -> Result<Node, Refusal> {
    let t = v.text().unwrap_or("");
    Ok(match name {
        "editmode26" => Node::atom(if t == "2" || t == "3" { "2" } else { t }),
        "editmode61" => Node::atom(if t == "2" { "3" } else { t }),
        "clamp3" => Node::atom(if v.as_i64().is_some_and(|n| n < 3) {
            t
        } else {
            "0"
        }),
        "cpl5006" => fields::cpl5006(v)?,
        "button_type" => Node::atom(if t == "3" { "0" } else { t }),
        "location_v29" => Node::atom(match t {
            "0" => "1",
            "1" => "3",
            "2" => "0",
            _ => {
                return Err(Refusal::Domain {
                    op: name.into(),
                    value: t.into(),
                });
            }
        }),
        "table_bool" => Node::atom(if t == "0" { "0" } else { "1" }),
        "table_enum" => Node::atom(if t == "0" { "0" } else { "2" }),
        "cmdbar_location" => Node::atom(match t {
            "1" => "2",
            "0" => "0",
            _ => "1",
        }),
        _ => return Err(Refusal::Shape("xf")),
    })
}

fn derived(op: &Op, env: &Env<'_>) -> Result<Node, Refusal> {
    let name = op.name.as_deref().unwrap_or("");
    check_inputs(op, env.body)?;
    Ok(match name {
        "automark" => {
            if env.kind == "2" {
                let v = at(env.body, 39, "Field[39]")?
                    .at(31)
                    .ok_or(Refusal::Shape("Field[39][31]"))?;
                check_in(name, Some(&Domain::Set(vec![0, 1, 2])), v)?;
                v.clone()
            } else {
                Node::atom("2")
            }
        }
        "hyperlink" => at(env.body, op.old.unwrap_or(22), "Field[22]")?.clone(),
        "cpl5007" => fields::cpl5007(at(env.body, op.old.unwrap_or(26), "InputField[26]")?)?,
        "importance" => {
            let default_button = at(env.body, 11, "Button[11]")?;
            let shape = at(env.body, 45, "Button[45]")?;
            Node::atom(if default_button.text() == Some("1") {
                "0"
            } else if shape.text() == Some("3") {
                "2"
            } else {
                "1"
            })
        }
        _ => return Err(Refusal::Shape("derived")),
    })
}

fn sublist(env: &Env<'_>, old: &Node, cx: &mut Cx<'_>) -> Result<Node, Refusal> {
    match env.class {
        "Field" => fields::sublist(env.kind, old, cx),
        "Group" | "Nested22" => groups::group_kind(env.kind, old, cx),
        "Decoration" | "Tooltip" => {
            let owner_color =
                cx.upgrade_value("Color", at(env.body, 14, "Decoration[14]")?, env.class)?;
            groups::decoration_kind(env.kind, old, &owner_color, cx)
        }
        "Addition" => addition_sublist(env.kind, old, cx),
        _ => Err(Refusal::Shape("sublist class")),
    }
}

fn addition_sublist(kind: &str, old: &Node, cx: &mut Cx<'_>) -> Result<Node, Refusal> {
    let a = cx
        .tables
        .addition_sublists
        .get(kind)
        .ok_or_else(|| Refusal::NoRuleKind {
            what: "addition-kind",
            kind: kind.into(),
            version: old.at(0).and_then(Node::text).unwrap_or("?").into(),
        })?;
    let l = old.as_list().ok_or(Refusal::Shape("Addition[13]"))?;
    if l.len() != a.len {
        return Err(Refusal::NoRuleKind {
            what: "addition-kind",
            kind: kind.into(),
            version: l.first().and_then(Node::text).unwrap_or("?").into(),
        });
    }
    let mut out = Vec::with_capacity(l.len());
    for (i, x) in l.iter().enumerate() {
        out.push(match a.types.get(&i) {
            Some(ty) => cx.upgrade_value(ty, x, "Addition")?,
            None => x.clone(),
        });
    }
    Ok(Node::list(out))
}

pub(crate) fn apply(c: &Context, env: &Env<'_>, cx: &mut Cx<'_>) -> Result<Vec<Node>, Refusal> {
    let mut out = Vec::with_capacity(c.new_len);
    for op in &c.ops {
        let old = |what: &'static str| -> Result<&Node, Refusal> {
            at(env.body, op.old.ok_or(Refusal::Shape(what))?, what)
        };
        let n = match op.op.as_str() {
            "ver" => Node::atom(c.new_version.clone()),
            "copy" => {
                let v = old("copy")?;
                cx.upgrade_value(op.ty.as_deref().unwrap_or("Plain"), v, env.class)?
            }
            "default" => cx.parse_default(op.value.as_deref().unwrap_or("0"))?,
            "xf" => {
                let v = old("xf")?;
                check_domain(op, v)?;
                xf(op.name.as_deref().unwrap_or(""), v)?
            }
            "derived" => derived(op, env)?,
            "created" => created::for_item(op.name.as_deref().unwrap_or(""), env, cx)?,
            "nested" => old("nested")?.clone(),
            "sublist" => {
                let v = old("sublist")?;
                let announced = match op.flag {
                    Some(f) => at(env.body, f, "sublist flag")?.text() == Some("1"),
                    None => true,
                };
                if announced {
                    sublist(env, v, cx)?
                } else {
                    v.clone()
                }
            }
            "bagcount" => Node::atom("0"),
            _ => return Err(Refusal::Shape("op")),
        };
        out.push(n);
    }
    Ok(out)
}

pub(crate) fn translate_item(
    it: &Item,
    owner: Option<&str>,
    cx: &mut Cx<'_>,
) -> Result<Item, Refusal> {
    let _ = owner;
    let ctx_name = context_name(it)?;
    let c = cx
        .tables
        .items
        .get(&ctx_name)
        .ok_or_else(|| Refusal::NoRule {
            class: it.class.unwrap_or("nested").into(),
            version: it.version.clone(),
        })?;
    if it.body.len() != c.old_len {
        return Err(Refusal::NoRule {
            class: it.class.unwrap_or("nested").into(),
            version: it.version.clone(),
        });
    }
    let class = ctx_name.split('/').next().unwrap_or("");
    let kind = it
        .body
        .get(5)
        .and_then(Node::text)
        .unwrap_or("")
        .to_string();
    let env = Env {
        class,
        body: &it.body,
        kind: &kind,
    };
    let mut body = apply(c, &env, cx)?;
    let mut nested = Vec::with_capacity(it.nested.len());
    for (p, n) in &it.nested {
        nested.push((*p, translate_item(n, Some(class), cx)?));
    }
    let mut children = Vec::with_capacity(it.children.len());
    for ch in &it.children {
        children.push(translate_item(ch, Some(class), cx)?);
    }
    let mut bag = Vec::new();
    let bag_at = if it.class == Some("Table") {
        for (k, v) in &it.bag {
            let key: i64 = k.parse().map_err(|_| Refusal::Shape("table bag key"))?;
            if matches!(key, 2 | 17 | 18) || key >= 22 {
                cx.stats.hit("table bag key dropped by reader");
                continue;
            }
            bag.push((k.clone(), v.clone()));
        }
        bag.sort_by_key(|(k, _)| k.parse::<i64>().unwrap_or(i64::MAX));
        *body
            .get_mut(TABLE_BAG_AT)
            .ok_or(Refusal::Shape("Table[54]"))? = Node::int(i64::try_from(bag.len()).unwrap_or(0));
        Some(TABLE_BAG_AT)
    } else {
        None
    };
    cx.stats.hit_owned(format!(
        "item {} v{}->v{}",
        it.class.unwrap_or("nested"),
        it.version,
        c.new_version
    ));
    Ok(Item {
        id_text: it.id_text.clone(),
        class: it.class,
        version: c.new_version.clone(),
        body,
        children,
        nested,
        children_at: it.children_at,
        opt: it.opt.clone(),
        bag,
        bag_at,
    })
}
