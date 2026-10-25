use brace::{Guid, Node};
use mdschema::MdKind;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Column<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub pattern: &'a Node,
}

#[derive(Debug, Clone, Copy)]
pub enum PredefBody<'a> {
    Tree(&'a Node),
    Flat(&'a [Node]),
}

#[derive(Debug, Clone)]
pub struct PredefTable<'a> {
    pub kind_no: u32,
    pub columns: Vec<Column<'a>>,
    pub positions: Vec<&'a str>,
    pub body: PredefBody<'a>,
}

impl<'a> PredefTable<'a> {
    pub fn column(&self, id: &str) -> Option<&Column<'a>> {
        self.columns.iter().find(|c| c.id == id)
    }
}

pub fn table_suffix(kind: MdKind) -> Option<&'static str> {
    match kind {
        MdKind::Catalog => Some("1c"),
        MdKind::ChartOfCharacteristicTypes => Some("7"),
        MdKind::ChartOfAccounts => Some("9"),
        MdKind::ChartOfCalculationTypes => Some("2"),
        _ => None,
    }
}

pub fn ref_name_columns(kind_no: u32) -> Option<(&'static str, &'static str)> {
    match kind_no {
        0 => Some(("0", "3")),
        1 => Some(("1", "3")),
        2 => Some(("0", "1")),
        9 => Some(("1", "2")),
        _ => None,
    }
}

fn num(n: &Node) -> Option<usize> {
    n.text()?.trim().parse().ok()
}

fn parse_columns(cols: &Node) -> Option<Vec<Column<'_>>> {
    let l = cols.as_list()?;
    let n = num(l.first()?)?;
    l.get(1..)?
        .iter()
        .take(n)
        .map(|c| {
            Some(Column {
                id: c.at(0)?.text()?,
                name: c.at(1)?.text()?,
                pattern: c.at(2)?,
            })
        })
        .collect()
}

fn parse_positions(tb: &Node) -> Option<(Vec<&str>, &Node)> {
    let l = tb.as_list()?;
    let n = num(l.get(1)?)?;
    if n > l.len().saturating_sub(2) / 2 {
        return None;
    }
    let mut pos: Vec<Option<&str>> = vec![None; n];
    for i in 0..n {
        let p = num(l.get(2 + 2 * i)?)?;
        *pos.get_mut(p)? = Some(l.get(3 + 2 * i)?.text()?);
    }
    let positions = pos.into_iter().collect::<Option<Vec<_>>>()?;
    Some((positions, l.get(2 + 2 * n)?))
}

fn kind_no(t: &Node) -> Option<u32> {
    num(t.at(0)?)?.try_into().ok()
}

pub fn parse_table(t: &Node) -> Option<PredefTable<'_>> {
    let kind_no = kind_no(t)?;
    let body = t.at(1)?;
    let columns = parse_columns(body.at(1)?)?;
    let (positions, rows) = parse_positions(body.at(2)?)?;
    let root = rows.at(2)?;
    Some(PredefTable {
        kind_no,
        columns,
        positions,
        body: PredefBody::Tree(root),
    })
}

pub fn parse_vtable(t: &Node) -> Option<PredefTable<'_>> {
    let kind_no = kind_no(t)?;
    let columns = parse_columns(t.at(1)?)?;
    let (positions, rl) = parse_positions(t.at(2)?)?;
    let l = rl.as_list()?;
    let n = num(l.get(1)?)?;
    let rows = l.get(2..)?;
    Some(PredefTable {
        kind_no,
        columns,
        positions,
        body: PredefBody::Flat(rows.get(..n.min(rows.len()))?),
    })
}

pub fn iter_children(row: &Node) -> Option<&[Node]> {
    let l = row.as_list()?;
    let nv = num(l.get(2)?)?;
    let rest = l.get(3usize.checked_add(nv)?..).unwrap_or(&[]);
    match rest.first() {
        Some(f) if f.text() == Some("1") => {
            let ch = rest.get(1)?.as_list()?;
            let n = num(ch.get(1)?)?;
            let kids = ch.get(2..)?;
            kids.get(..n.min(kids.len()))
        }
        _ => Some(&[]),
    }
}

pub fn row_values<'a>(row: &'a Node, positions: &[&'a str]) -> Option<HashMap<&'a str, &'a Node>> {
    let l = row.as_list()?;
    let nv = num(l.get(2)?)?;
    let vals = l.get(3..)?.get(..nv)?;
    vals.iter()
        .enumerate()
        .map(|(i, v)| Some((*positions.get(i)?, v)))
        .collect()
}

pub fn ref_uuid(v: &Node) -> Option<Guid> {
    if v.at(0)?.text()? != "#" {
        return None;
    }
    Guid::parse(v.at(2)?.at(1)?.text()?)
}

pub fn scalar(v: &Node) -> Option<Option<String>> {
    match v.at(0)?.text()? {
        "S" => Some(Some(v.at(1).and_then(Node::text).unwrap_or("").to_string())),
        "N" => Some(Some(v.at(1)?.text()?.to_string())),
        "B" => Some(Some(
            if v.at(1)?.text()? == "1" {
                "true"
            } else {
                "false"
            }
            .to_string(),
        )),
        "U" => Some(None),
        _ => None,
    }
}

fn add_item(
    vals: &HashMap<&str, &Node>,
    (rid, nid): (&str, &str),
    out: &mut HashMap<Guid, String>,
) -> Option<()> {
    let id = ref_uuid(vals.get(rid)?)?;
    let name = scalar(vals.get(nid)?)??;
    let last = name.rsplit('.').next().unwrap_or("").to_string();
    out.insert(id, last);
    Some(())
}

pub fn index_table(kind: MdKind, t: &Node, out: &mut HashMap<Guid, String>) {
    if table_suffix(kind).is_none() {
        return;
    }
    let _ = index_table_inner(t, out);
}

fn index_table_inner(t: &Node, out: &mut HashMap<Guid, String>) -> Option<()> {
    let flat = kind_no(t)? == 9;
    let table = if flat {
        parse_vtable(t)?
    } else {
        parse_table(t)?
    };
    let cols = ref_name_columns(table.kind_no)?;
    match table.body {
        PredefBody::Flat(rows) => {
            for r in rows {
                add_item(&row_values(r, &table.positions)?, cols, out)?;
            }
        }
        PredefBody::Tree(root) => {
            let mut stack = vec![iter_children(root)?.iter()];
            while let Some(top) = stack.last_mut() {
                let Some(k) = top.next() else {
                    stack.pop();
                    continue;
                };
                add_item(&row_values(k, &table.positions)?, cols, out)?;
                stack.push(iter_children(k)?.iter());
            }
        }
    }
    Some(())
}
