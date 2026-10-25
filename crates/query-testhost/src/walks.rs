use std::sync::Arc;

use bsl_prim::Collation;
use query_exec::QueryResult;
use query_exec::totals::{RecordType, RowRef};
use query_exec::walk::{self, Selection, TreeRow, Unloaded, Walk, WalkError};
use query_ir::QValue;
use query_syntax::Lang;
use serde_json::{Value, json};

use crate::host::TestBase;
use crate::repr::repr_lang;

fn msg(key: &str, lang: Lang) -> String {
    let l = match lang {
        Lang::Ru => platform_catalog::messages::Lang::Ru,
        Lang::En => platform_catalog::messages::Lang::En,
    };
    platform_catalog::messages::text_key(key, l)
        .unwrap_or(key)
        .to_string()
}

pub fn record_type_text(t: RecordType, lang: Lang) -> String {
    let key = match t {
        RecordType::Overall => "entext:IDS_VALUESSELECTRESULTRECORDTYPE_OVERALL",
        RecordType::GroupTotal => "entext:IDS_VALUESSELECTRESULTRECORDTYPE_GROUPSUBTOTAL",
        RecordType::HierarchyTotal => {
            "entext:IDS_VALUESSELECTRESULTRECORDTYPE_HIERARCHICALSUBTOTAL"
        }
        RecordType::Detail => "entext:IDS_VALUESSELECTRESULTRECORDTYPE_DETAIL",
    };
    msg(key, lang)
}

pub fn walk_error(e: &WalkError, method: &str, lang: Lang) -> Vec<String> {
    let _ = method;
    let cause = match e {
        WalkError::DivisionByZero => format!(
            "{} \"{}\"",
            msg("entext:IDS_SELECT_ERROR_ARITHMETICERROR", lang),
            msg("entext:IDS_SELECT_DIVIDEBYZERO", lang)
        ),
        WalkError::DimensionNotFound(n) => {
            format!(
                "{} \"{n}\"",
                msg("entext:IDS_SELECT_ERROR_DIMENSIONNOTFINDED", lang)
            )
        }
        WalkError::DimensionNotFoundInParents(n) => format!(
            "{} \"{n}\"",
            msg(
                "entext:IDS_SELECT_ERROR_DIMENSIONNOTFINDEDINPARENTITERATORS",
                lang
            )
        ),
        WalkError::DifferentDimensionCount => msg(
            "entext:IDS_SELECT_ERROR_DIFFERENTCOUNTINDIMENSIONSOURCES",
            lang,
        ),
        other => format!("{other:?}"),
    };
    vec![cause]
}

fn values(s: &Selection, base: &TestBase, lang: Lang) -> Result<Vec<Value>, WalkError> {
    (0..s.width())
        .map(|i| Ok(Value::String(repr_lang(s.value(i)?, base, lang))))
        .collect()
}

fn meta(
    s: &Selection,
    base: &TestBase,
    lang: Lang,
) -> Result<serde_json::Map<String, Value>, WalkError> {
    let mut m = serde_json::Map::new();
    m.insert(
        "t".into(),
        Value::String(record_type_text(s.record_type()?, lang)),
    );
    m.insert("l".into(), json!(s.level()?));
    m.insert("g".into(), Value::String(s.grouping()?.to_string()));
    m.insert("v".into(), Value::Array(values(s, base, lang)?));
    Ok(m)
}

fn tree(
    mut s: Selection,
    w: Walk,
    base: &TestBase,
    lang: Lang,
    coll: &dyn Collation,
) -> Result<Vec<Value>, WalkError> {
    let mut out = Vec::new();
    while s.next() {
        let mut m = meta(&s, base, lang)?;
        let nested = if s.record_type()? == RecordType::HierarchyTotal {
            let g = s.grouping()?.to_string();
            s.select(w, &g, "", coll)?
        } else {
            s.select(w, "", "", coll)?
        };
        let kids = tree(nested, w, base, lang, coll)?;
        if !kids.is_empty() {
            m.insert("c".into(), Value::Array(kids));
        }
        out.push(Value::Object(m));
    }
    Ok(out)
}

pub(crate) fn rows_json(
    res: &Arc<QueryResult>,
    walk_kind: &str,
    base: &TestBase,
    lang: Lang,
    coll: &dyn Collation,
) -> Result<Vec<Value>, WalkError> {
    match walk_kind {
        "linear" => {
            let mut s = walk::select(res, Walk::Direct, "", "", coll)?;
            let mut out = Vec::new();
            while s.next() {
                out.push(Value::Object(meta(&s, base, lang)?));
            }
            Ok(out)
        }
        "groups" => tree(
            walk::select(res, Walk::ByGroups, "", "", coll)?,
            Walk::ByGroups,
            base,
            lang,
            coll,
        ),
        "hier" => tree(
            walk::select(res, Walk::ByGroupsWithHierarchy, "", "", coll)?,
            Walk::ByGroupsWithHierarchy,
            base,
            lang,
            coll,
        ),
        _ => {
            let mut s = walk::select(res, Walk::Direct, "", "", coll)?;
            let mut out = Vec::new();
            while s.next() {
                out.push(Value::Array(values(&s, base, lang)?));
            }
            Ok(out)
        }
    }
}

fn row_of(res: &QueryResult, r: RowRef) -> Option<&Vec<QValue>> {
    match r {
        RowRef::Detail(i) => res.rows.get(i as usize),
        RowRef::Total(i) => res.totals.as_ref()?.totals.get(i as usize),
    }
}

fn cells(res: &QueryResult, r: RowRef, width: usize, base: &TestBase, lang: Lang) -> String {
    let vals: Vec<String> = (0..width)
        .map(|i| {
            row_of(res, r)
                .and_then(|row| row.get(i))
                .map_or_else(|| "NULL".to_string(), |v| repr_lang(v, base, lang))
        })
        .collect();
    format!("({})", vals.join(", "))
}

fn tree_text(
    res: &QueryResult,
    rows: &[TreeRow],
    width: usize,
    base: &TestBase,
    lang: Lang,
) -> String {
    let parts: Vec<String> = rows
        .iter()
        .map(|r| {
            let mut t = cells(res, r.row, width, base, lang);
            if !r.children.is_empty() {
                t.push_str(&tree_text(res, &r.children, width, base, lang));
            }
            t
        })
        .collect();
    format!("[{}]", parts.join(", "))
}

pub(crate) fn unload_json(
    res: &QueryResult,
    kind: &str,
    columns: &[Value],
    base: &TestBase,
    lang: Lang,
) -> Result<(Value, Value, Value), WalkError> {
    let w = match kind {
        "vt" => Walk::Direct,
        "groups" => Walk::ByGroups,
        _ => Walk::ByGroupsWithHierarchy,
    };
    let width = res.columns.len();
    let names: Vec<String> = res.columns.iter().map(|c| c.name.clone()).collect();
    let header = names.join(", ");
    let (type_name, text) = match walk::unload(res, w)? {
        Unloaded::Table(rows) => {
            let parts: Vec<String> = rows
                .iter()
                .map(|r| cells(res, *r, width, base, lang))
                .collect();
            (
                query_testkit::repr::platform("ValueTable", lang),
                format!("ТаблицаЗначений<{header}>[{}]", parts.join(", ")),
            )
        }
        Unloaded::Tree(rows) => (
            query_testkit::repr::platform("ValueTree", lang),
            format!(
                "ДеревоЗначений<{header}>{}",
                tree_text(res, &rows, width, base, lang)
            ),
        ),
    };
    let cols: Vec<Value> = columns
        .iter()
        .map(|c| json!({ "name": c["name"], "type": c["type"] }))
        .collect();
    Ok((
        Value::String(type_name),
        Value::Array(cols),
        Value::String(text),
    ))
}

fn untieable(row: &Value, i: usize, mask: Option<&[bool]>) -> bool {
    match row {
        Value::Object(m) => m.get("g").and_then(Value::as_str) == Some("") && !m.contains_key("c"),
        _ => mask.and_then(|m| m.get(i)).copied().unwrap_or(false),
    }
}

fn tie_key(row: &Value, cols: &[usize]) -> (Option<Value>, Vec<Value>) {
    let vals = row.get("v").unwrap_or(row);
    (
        row.get("l").cloned(),
        cols.iter()
            .map(|c| vals.get(*c).cloned().unwrap_or(Value::Null))
            .collect(),
    )
}

pub(crate) fn untie(rows: &[Value], cols: &[usize], mask: Option<&[bool]>) -> Vec<Value> {
    let rows: Vec<Value> = rows
        .iter()
        .map(|r| match r {
            Value::Object(m) if m.contains_key("c") => {
                let mut m = m.clone();
                if let Some(Value::Array(c)) = m.get("c") {
                    let c = untie(c, cols, None);
                    m.insert("c".into(), Value::Array(c));
                }
                Value::Object(m)
            }
            x => x.clone(),
        })
        .collect();
    let mut out = Vec::with_capacity(rows.len());
    let mut i = 0;
    while i < rows.len() {
        let Some(first) = rows.get(i) else { break };
        if !untieable(first, i, mask) {
            out.push(first.clone());
            i += 1;
            continue;
        }
        let k = tie_key(first, cols);
        let mut j = i + 1;
        while rows
            .get(j)
            .is_some_and(|r| untieable(r, j, mask) && tie_key(r, cols) == k)
        {
            j += 1;
        }
        let mut run: Vec<Value> = rows.get(i..j).unwrap_or_default().to_vec();
        run.sort_by_key(crate::run::canonical);
        out.extend(run);
        i = j;
    }
    out
}

struct TextNode {
    cells: Vec<String>,
    children: Vec<TextNode>,
}

fn parse_rows(s: &[char], at: &mut usize) -> Vec<TextNode> {
    let mut out = Vec::new();
    if s.get(*at) != Some(&'[') {
        return out;
    }
    *at += 1;
    loop {
        while s.get(*at).is_some_and(|c| *c == ' ' || *c == ',') {
            *at += 1;
        }
        match s.get(*at) {
            Some('(') => {
                *at += 1;
                let mut cells = Vec::new();
                let mut cur = String::new();
                let mut depth = 0usize;
                let mut quoted = false;
                while let Some(&c) = s.get(*at) {
                    *at += 1;
                    if quoted {
                        cur.push(c);
                        if c == '"' {
                            if s.get(*at) == Some(&'"') {
                                cur.push('"');
                                *at += 1;
                            } else {
                                quoted = false;
                            }
                        }
                        continue;
                    }
                    match c {
                        '"' => {
                            quoted = true;
                            cur.push(c);
                        }
                        '(' | '[' => {
                            depth += 1;
                            cur.push(c);
                        }
                        ')' | ']' if depth > 0 => {
                            depth -= 1;
                            cur.push(c);
                        }
                        ')' => break,
                        ',' if depth == 0 => {
                            cells.push(cur.trim().to_string());
                            cur.clear();
                        }
                        _ => cur.push(c),
                    }
                }
                cells.push(cur.trim().to_string());
                let children = parse_rows(s, at);
                out.push(TextNode { cells, children });
            }
            Some(']') => {
                *at += 1;
                return out;
            }
            _ => return out,
        }
    }
}

fn node_text(nodes: &[TextNode], cols: &[usize]) -> String {
    let mut parts: Vec<(Vec<String>, String)> = nodes
        .iter()
        .map(|n| {
            let mut t = format!("({})", n.cells.join(", "));
            if !n.children.is_empty() {
                t.push_str(&node_text(&n.children, cols));
            }
            let key = cols
                .iter()
                .map(|c| n.cells.get(*c).cloned().unwrap_or_default())
                .collect();
            (key, t)
        })
        .collect();
    let leaf = |k: usize| nodes.get(k).is_some_and(|n| n.children.is_empty());
    let mut i = 0;
    while i < parts.len() {
        if !leaf(i) {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < parts.len() && leaf(j) && parts.get(j).map(|p| &p.0) == parts.get(i).map(|p| &p.0)
        {
            j += 1;
        }
        if let Some(run) = parts.get_mut(i..j) {
            run.sort_by(|a, b| a.1.cmp(&b.1));
        }
        i = j;
    }
    format!(
        "[{}]",
        parts
            .into_iter()
            .map(|(_, t)| t)
            .collect::<Vec<_>>()
            .join(", ")
    )
}

pub(crate) fn untie_text(text: &str, cols: &[usize]) -> String {
    if !text.starts_with("ДеревоЗначений") {
        return text.to_string();
    }
    let Some(open) = text.find('[') else {
        return text.to_string();
    };
    let chars: Vec<char> = text.get(open..).unwrap_or_default().chars().collect();
    let mut at = 0;
    let nodes = parse_rows(&chars, &mut at);
    format!(
        "{}{}",
        text.get(..open).unwrap_or_default(),
        node_text(&nodes, cols)
    )
}

pub(crate) fn detail_mask(
    res: &Arc<QueryResult>,
    coll: &dyn Collation,
) -> Result<Vec<bool>, WalkError> {
    let mut s = walk::select(res, Walk::Direct, "", "", coll)?;
    let mut out = Vec::new();
    while s.next() {
        out.push(s.record_type()? == RecordType::Detail);
    }
    Ok(out)
}
