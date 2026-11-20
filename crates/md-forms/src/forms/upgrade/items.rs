use brace::Node;
use mdschema::forms_upgrade::{Context, Domain, Op};

use super::{Cx, Refusal, at, created, fields, groups};
use crate::forms::decode::{self, Item};
use crate::forms::variant::{self, TABLE_BAG_AT};

pub(crate) struct Env<'a> {
    pub class: &'a str,
    pub body: &'a [Node],
    pub kind: &'a str,
}

fn context_name(it: &Item) -> Result<String, Refusal> {
    Ok(match it.class {
        Some(c) => format!("{c}/{}", it.version),
        None => match it.version.as_str() {
            "22" if it.body.len() == 30 => "Group/22".to_owned(),
            "12" if it.body.len() == 36 => "Decoration/12".to_owned(),
            "19" | "21" | "22" => format!("Nested22/{}", it.version),
            "8" | "10" | "11" | "12" => format!("Tooltip/{}", it.version),
            "3" | "4" | "5" | "6" => format!("Addition/{}", it.version),
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
        "clamp1" => Node::atom(if v.as_i64().is_some_and(|n| n > 1) {
            "1"
        } else {
            t
        }),
        "le1_else0" => Node::atom(if v.as_i64().is_some_and(|n| n > 1) {
            "0"
        } else {
            t
        }),
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
        "enum_bool" => Node::int(i64::from(matches!(v.as_i64(), Some(1 | 2)))),
        "enum_one" => Node::int(i64::from(v.as_i64() == Some(1))),
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
            let shape = if op.inputs.contains(&45) {
                at(env.body, 45, "Button[45]")?.text()
            } else {
                Some("0")
            };
            Node::atom(if default_button.text() == Some("1") {
                "0"
            } else if shape == Some("3") {
                "2"
            } else {
                "1"
            })
        }
        "modern_cmdbar" => {
            let old = at(env.body, 8, "Table[8]")?
                .as_i64()
                .ok_or(Refusal::Shape("modern command bar"))?;
            let flag = at(env.body, 110, "Table[110]")?
                .as_bool()
                .ok_or(Refusal::Shape("modern command bar"))?;
            let mode = at(env.body, 111, "Table[111]")?
                .as_i64()
                .ok_or(Refusal::Shape("modern command bar"))?;
            Node::int(if !flag {
                old
            } else {
                match mode {
                    2 => 1,
                    0 => 0,
                    _ if matches!(old, 2 | 3) => old,
                    _ => 2,
                }
            })
        }
        "modern_default_button" => Node::int(i64::from(
            at(env.body, 11, "Button[11]")?.as_bool() == Some(true)
                || at(env.body, 57, "Button[57]")?.as_i64() == Some(0),
        )),
        "modern_importance" => {
            if at(env.body, 11, "Button[11]")?.as_bool() == Some(true) {
                Node::int(0)
            } else {
                at(env.body, 57, "Button[57]")?.clone()
            }
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
        "Addition" => groups::addition_kind(env.kind, old, cx),
        _ => Err(Refusal::Shape("sublist class")),
    }
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
    let normalized;
    let it = if it.class == Some("Table") && it.version == "64" {
        normalized = without_transient_table_objects(it)?;
        &normalized
    } else {
        it
    };
    let ctx_name = context_name(it)?;
    let c = cx
        .tables
        .items
        .get(&ctx_name)
        .ok_or_else(|| Refusal::NoRule {
            class: it.class.unwrap_or("nested").into(),
            version: it.version.clone(),
        })?;
    if it.body.len() != c.old_len && Some(it.body.len()) != c.read_len {
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
    for (p, x) in body.iter_mut().enumerate() {
        if decode::is_item(x) {
            let created = decode::Item::parse_in(x, None, &variant::V4_59)
                .ok_or(Refusal::Shape("created item"))?;
            *x = Node::atom(decode::NESTED);
            nested.push((p, created));
        }
    }
    nested.sort_by_key(|(p, _)| *p);
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

fn without_transient_table_objects(it: &Item) -> Result<Item, Refusal> {
    let mut out = it.clone();
    for _ in 0..2 {
        let flag = out
            .body
            .get(109)
            .and_then(Node::as_bool)
            .ok_or(Refusal::Shape("Table transient flag"))?;
        let end = 110 + usize::from(flag);
        if end > out.body.len() {
            return Err(Refusal::Shape("Table transient object"));
        }
        if flag && !out.nested.iter().any(|(p, _)| *p == 110) {
            return Err(Refusal::Shape("Table transient object"));
        }
        out.body.drain(109..end);
        out.nested.retain(|(p, _)| *p < 109 || *p >= end);
        for (p, _) in &mut out.nested {
            if *p >= end {
                *p -= end - 109;
            }
        }
    }
    Ok(out)
}
