use brace::Node;

use super::items::Env;
use super::{Cx, Refusal, at};

fn quoted(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

fn term<'a>(cx: &'a Cx<'_>, name: &str) -> Result<&'a str, Refusal> {
    cx.tables
        .terms
        .get(name)
        .map(String::as_str)
        .ok_or_else(|| Refusal::Table(format!("term {name}")))
}

fn template<'a>(cx: &'a Cx<'_>, name: &str) -> Result<&'a str, Refusal> {
    cx.tables
        .created
        .get(name)
        .map(String::as_str)
        .ok_or_else(|| Refusal::Table(format!("template {name}")))
}

fn group22(cx: &Cx<'_>, which: &str, kind: &str, name: &str) -> Result<String, Refusal> {
    Ok(template(cx, which)?
        .replace("$KIND", kind)
        .replace("$NAME", &quoted(name)))
}

fn tooltip12(cx: &Cx<'_>, name: &str) -> Result<String, Refusal> {
    Ok(template(cx, "tooltip12")?.replace("$NAME", &quoted(name)))
}

fn addition6(
    cx: &Cx<'_>,
    kind: &str,
    name: &str,
    table_id: &str,
    owner: &str,
) -> Result<String, Refusal> {
    let (sub, src, t) = match kind {
        "0" => ("search_string_sub", "0", "SearchString"),
        _ => ("search_control_sub", "2", "SearchControl"),
    };
    let menu = group22(
        cx,
        "menu22",
        "8",
        &format!("{name}{}", term(cx, "ContextMenu")?),
    )?;
    let tooltip = tooltip12(cx, &format!("{name}{}", term(cx, "ExtendedTooltip")?))?;
    Ok(template(cx, "addition6")?
        .replace("$KIND", kind)
        .replace("$NAME", &quoted(name))
        .replace("$SUB", template(cx, sub)?)
        .replace("$MENU", &menu)
        .replace("$TOOLTIP", &tooltip)
        .replace("$TID", table_id)
        .replace("$SRC", src)
        .replace("$TAIL", &quoted(&format!("{owner}{}", term(cx, t)?))))
}

pub(crate) fn for_item(name: &str, env: &Env<'_>, cx: &mut Cx<'_>) -> Result<Node, Refusal> {
    let owner_name = |p: usize| -> Result<String, Refusal> {
        Ok(at(env.body, p, "item name")?
            .as_str()
            .ok_or(Refusal::Shape("item name"))?
            .to_string())
    };
    let text = match (env.class, name) {
        ("Field", "SelectedRowsActionsPanel") => {
            cx.stats.hit("created:Field SelectedRowsActionsPanel");
            let n = format!("{}{}", owner_name(6)?, term(cx, name)?);
            group22(cx, "panel22", "10", &n)?
        }
        ("Table", _) => {
            let owner = owner_name(5)?;
            let table_id = at(env.body, 1, "table id")?
                .at(0)
                .and_then(Node::text)
                .ok_or(Refusal::Shape("table id"))?
                .to_string();
            let panel = format!("{owner}{}", term(cx, "SelectedRowsActionsPanel")?);
            match name {
                "SelectedRowsActionsPanel" => group22(cx, "panel22", "10", &panel)?,
                "RowActions" => group22(
                    cx,
                    "panel22",
                    "11",
                    &format!("{owner}{}", term(cx, "RowActions")?),
                )?,
                "SearchStringAddition" => {
                    cx.stats.hit("created:Table service items");
                    let n = format!("{panel}{}", term(cx, "SearchString")?);
                    addition6(cx, "0", &n, &table_id, &owner)?
                }
                "SearchControlAddition" => {
                    let n = format!("{panel}{}", term(cx, "SearchControl")?);
                    addition6(cx, "2", &n, &table_id, &owner)?
                }
                _ => return Err(Refusal::Shape("created")),
            }
        }
        _ => return Err(Refusal::Shape("created")),
    };
    brace::parse(&text).map_err(|_| Refusal::Shape("created template"))
}
