use md_xmlout::codec::parts::predefined::*;

use super::{
    AccountData, AccountType, CalcTypeData, ExtDimensionType, Predefined, PredefinedCode,
    PredefinedItem, fetch, miss,
};
use crate::entries::EntryData;
use crate::load::LoadCtx;
use crate::model::MdRef;
use crate::objects::codecs::Ctx;
use crate::objects::types::type_desc;
use crate::predefined::{
    Column, PredefBody, index_table, iter_children, parse_table, parse_vtable,
    scalar as raw_scalar, table_suffix,
};
use crate::{Configuration, MdKind, MdObject, Result};
use brace::{Guid, Node};
use std::collections::HashMap;
use std::sync::Arc;

const ACCOUNT_TYPE: [(&str, AccountType); 3] = [
    ("0", AccountType::Active),
    ("1", AccountType::Passive),
    ("2", AccountType::ActivePassive),
];

const T_VTABLE: &str = "acf6192e-81ca-46ef-93a6-5a6968b78663";

fn colinfo<'a>(cols: &[Column<'a>]) -> Vec<(&'a str, &'a str)> {
    dict(cols.iter().map(|c| (c.id, c.name)))
}

fn scalar(v: Option<&Node>) -> Decoded<Option<String>> {
    let v = v.ok_or(Miss::Malformed)?;
    raw_scalar(v).ok_or_else(|| match v.at(0).and_then(Node::text) {
        Some(c) if !matches!(c, "S" | "N" | "B" | "U") => bad(format!("scalar {c}")),
        _ => Miss::Malformed,
    })
}

fn text_of(v: Option<&Node>) -> Decoded<String> {
    Ok(scalar(v)?.unwrap_or_default())
}

fn bool_of(v: Option<&Node>) -> Decoded<bool> {
    match scalar(v)?.as_deref() {
        Some("true") => Ok(true),
        Some("false") => Ok(false),
        _ => Err(Miss::Unrepresentable),
    }
}

fn item_index<'a>(
    tables: impl IntoIterator<Item = (MdKind, &'a str, &'a Node)>,
) -> HashMap<Guid, String> {
    let mut out = HashMap::new();
    for (kind, name, t) in tables {
        let mut items = HashMap::new();
        index_table(kind, t, &mut items);
        for (id, item) in items {
            out.insert(id, format!("{}.{name}.{item}", kind.as_str()));
        }
    }
    out
}

struct Dec<'a> {
    cfg: &'a Configuration,
    ctx: Ctx<'a>,
    index: &'a HashMap<Guid, String>,
}

impl Dec<'_> {
    fn item_ref(&self, text: &str, miss: &str) -> Decoded<MdRef> {
        let id = Guid::parse(text);
        let name = id
            .and_then(|g| self.index.get(&g))
            .ok_or_else(|| bad(format!("{miss}{text}")))?;
        Ok(MdRef {
            id,
            name: name.clone(),
        })
    }

    fn flag_ref(&self, text: &str, miss: &str) -> Decoded<MdRef> {
        let id = Guid::parse(text);
        let name = id
            .and_then(|g| self.cfg.full_name(g))
            .ok_or_else(|| bad(format!("{miss}{text}")))?;
        Ok(MdRef {
            id,
            name: name.to_string(),
        })
    }
}

struct Tree<'a> {
    kind_no: u32,
    cols: Columns,
    colinfo: Vec<(&'a str, &'a str)>,
    positions: Vec<&'a str>,
    flags: Decoded<Vec<(&'a str, &'a str)>>,
}

fn flag_columns<'a>(ci: &[(&'a str, &'a str)], cols: Columns) -> Decoded<Vec<(&'a str, &'a str)>> {
    let mut v = ci
        .iter()
        .filter(|(c, n)| field(cols, c).is_none() && !n.is_empty())
        .map(|&(c, n)| {
            let k: i64 = c.trim().parse().map_err(|_| Miss::Malformed)?;
            Ok((k, c, n))
        })
        .collect::<Decoded<Vec<_>>>()?;
    v.sort_by_key(|x| x.0);
    Ok(v.into_iter().map(|(_, c, n)| (c, n)).collect())
}

fn ext_dims(d: &Dec, v: &Node) -> Decoded<Vec<ExtDimensionType>> {
    if v.at(0).ok_or(Miss::Malformed)?.text() != Some("#")
        || v.at(1).ok_or(Miss::Malformed)?.text() != Some(T_VTABLE)
    {
        return Err(bad("ext dims value".into()));
    }
    let t = parse_vtable(v.at(2).ok_or(Miss::Malformed)?).ok_or(Miss::Malformed)?;
    let PredefBody::Flat(rows) = t.body else {
        return Err(Miss::Malformed);
    };
    let ci = colinfo(&t.columns);
    let rows = rows
        .iter()
        .map(|r| cells(r, &t.positions))
        .collect::<Decoded<Vec<_>>>()?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        let kind = d.item_ref(ref_text(lookup(r, "0"))?, "ext dim type ")?;
        let turnover = bool_of(lookup(r, "1"))?;
        let stored = r
            .iter()
            .filter(|(c, _)| !matches!(*c, "0" | "1"))
            .map(|&(c, val)| Ok((lookup(&ci, c).ok_or(Miss::Malformed)?, val)))
            .collect::<Decoded<Vec<_>>>()?;
        let mut flags = Vec::with_capacity(stored.len());
        for (fu, val) in stored {
            let r = d.flag_ref(fu, "ext dim flag ")?;
            flags.push((r, bool_of(Some(val))?));
        }
        out.push(ExtDimensionType {
            kind,
            turnover,
            flags,
        });
    }
    Ok(out)
}

const ITEM_DEPTH: usize = 128;

fn tree_item(
    d: &Dec,
    tr: &Tree,
    row: &Node,
    states: &mut bool,
    depth: usize,
) -> Decoded<PredefinedItem> {
    if depth > ITEM_DEPTH {
        return Err(Miss::TooDeep);
    }
    let vals = cells(row, &tr.positions)?;
    let mut f: HashMap<&'static str, &Node> = HashMap::new();
    for &(cid, v) in &vals {
        match field(tr.cols, cid) {
            Some(fld) => {
                f.insert(fld, v);
            }
            None if tr.kind_no == 2 && lookup(&tr.colinfo, cid).is_some_and(|n| !n.is_empty()) => {}
            None => return Err(bad(format!("column {cid}"))),
        }
    }
    let flag_cols = if tr.kind_no == 2 {
        tr.flags.clone()?
    } else {
        Vec::new()
    };
    let get = |fld: &str| f.get(fld).copied();
    let id = Guid::parse(ref_text(get("Ref"))?).ok_or(Miss::Unrepresentable)?;
    let name = text_of(get("Name"))?;
    let code = match get("Code") {
        Some(c) if c.at(0).and_then(Node::text) == Some("N") => {
            PredefinedCode::Number(text_of(Some(c))?)
        }
        c => PredefinedCode::Str(text_of(c)?),
    };
    let description = text_of(get("Description"))?;
    let mut item = PredefinedItem {
        id,
        name,
        code,
        description,
        ..Default::default()
    };
    if tr.kind_no == 1 {
        let pat = get("Type").and_then(|t| t.at(2)).ok_or(Miss::Malformed)?;
        item.type_desc = Some(type_desc(&d.ctx, pat).ok_or_else(|| bad("type".into()))?);
    }
    if tr.kind_no != 2 {
        item.is_folder = bool_of(get("IsFolder"))?;
    } else {
        let account_type = get("AccountType")
            .and_then(|v| v.at(1))
            .and_then(Node::text)
            .and_then(|t| lookup(&ACCOUNT_TYPE, t))
            .ok_or(Miss::Malformed)?;
        let off_balance = bool_of(get("OffBalance"))?;
        let order = text_of(get("Order"))?;
        let mut flags = Vec::with_capacity(flag_cols.len());
        for (cid, nm) in flag_cols {
            let r = d.flag_ref(nm, "flag ")?;
            let v = match lookup(&vals, cid) {
                Some(v) => bool_of(Some(v))?,
                None => false,
            };
            flags.push((r, v));
        }
        let ext_dimension_types = match get("ExtDimensionTypes") {
            Some(e) if e.at(0).ok_or(Miss::Malformed)?.text() != Some("U") => Some(ext_dims(d, e)?),
            _ => None,
        };
        item.account = Some(AccountData {
            account_type,
            off_balance,
            order,
            flags,
            ext_dimension_types,
        });
    }
    if let Some(st) = get("ExtState")
        && st
            .at(1)
            .ok_or(Miss::Malformed)?
            .text()
            .and_then(ext_state)
            .is_some()
    {
        *states = true;
    }
    for k in iter_children(row).ok_or(Miss::Malformed)? {
        item.children.push(tree_item(d, tr, k, states, depth + 1)?);
    }
    Ok(item)
}

fn tree_items(d: &Dec, t: &Node) -> Decoded<(Vec<PredefinedItem>, bool)> {
    let tb = parse_table(t).ok_or(Miss::Malformed)?;
    let cols = columns(tb.kind_no).ok_or(Miss::Malformed)?;
    let PredefBody::Tree(root) = tb.body else {
        return Err(Miss::Malformed);
    };
    let ci = colinfo(&tb.columns);
    let flags = if tb.kind_no == 2 {
        flag_columns(&ci, cols)
    } else {
        Ok(Vec::new())
    };
    let tr = Tree {
        kind_no: tb.kind_no,
        cols,
        colinfo: ci,
        positions: tb.positions,
        flags,
    };
    let mut states = false;
    let mut items = Vec::new();
    for r in iter_children(root).ok_or(Miss::Malformed)? {
        items.push(tree_item(d, &tr, r, &mut states, 1)?);
    }
    Ok((items, states))
}

fn calc_refs(d: &Dec, val: Option<&Node>) -> Decoded<Vec<MdRef>> {
    let val = val.ok_or(Miss::Malformed)?;
    if val.at(0).ok_or(Miss::Malformed)?.text() != Some("#") {
        return Ok(Vec::new());
    }
    let t = parse_vtable(val.at(2).ok_or(Miss::Malformed)?).ok_or(Miss::Malformed)?;
    let PredefBody::Flat(rows) = t.body else {
        return Err(Miss::Malformed);
    };
    let ci = colinfo(&t.columns);
    let rows = rows
        .iter()
        .map(|r| cells(r, &t.positions))
        .collect::<Decoded<Vec<_>>>()?;
    if ci.is_empty() || rows.is_empty() {
        return Ok(Vec::new());
    }
    let mut last: Option<(i64, &str)> = None;
    for (c, _) in &ci {
        let k: i64 = c.trim().parse().map_err(|_| Miss::Malformed)?;
        if last.is_none_or(|(m, _)| k > m) {
            last = Some((k, c));
        }
    }
    let (_, last) = last.ok_or(Miss::Malformed)?;
    rows.iter()
        .map(|r| {
            let text = ref_text(Some(lookup(r, last).ok_or(Miss::Malformed)?))?;
            d.item_ref(text, "").map_err(|m| match m {
                Miss::Bad(_) => bad("calc type ref".into()),
                other => other,
            })
        })
        .collect()
}

fn calc_items(d: &Dec, t: &Node) -> Decoded<Vec<PredefinedItem>> {
    let tb = parse_vtable(t).ok_or(Miss::Malformed)?;
    let PredefBody::Flat(rows) = tb.body else {
        return Err(Miss::Malformed);
    };
    let cols = columns(9).ok_or(Miss::Malformed)?;
    let col = |fld: &str| column_of(cols, fld).unwrap_or_default();
    let rows = rows
        .iter()
        .map(|r| cells(r, &tb.positions))
        .collect::<Decoded<Vec<_>>>()?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        let get = |fld: &str| lookup(r, col(fld));
        let id = Guid::parse(ref_text(get("Ref"))?).ok_or(Miss::Unrepresentable)?;
        let name = text_of(get("Name"))?;
        let code = match get("Code") {
            Some(c) if c.at(0).and_then(Node::text) == Some("N") => {
                PredefinedCode::Number(text_of(Some(c))?)
            }
            c => PredefinedCode::Str(text_of(c)?),
        };
        let description = text_of(get("Description"))?;
        let action_period_is_base = bool_of(get("ActionPeriodIsBase"))?;
        let base = calc_refs(d, get("Base"))?;
        let leading = calc_refs(d, get("Leading"))?;
        let displaced = calc_refs(d, get("Displaced"))?;
        out.push(PredefinedItem {
            id,
            name,
            code,
            description,
            calc: Some(CalcTypeData {
                action_period_is_base,
                base,
                leading,
                displaced,
            }),
            ..Default::default()
        });
    }
    Ok(out)
}

fn decode(
    cfg: &Configuration,
    index: &HashMap<Guid, String>,
    t: &Node,
    keep_source: bool,
) -> Decoded<Predefined> {
    let d = Dec {
        cfg,
        ctx: Ctx {
            schema: mdschema::schema().map_err(|_| Miss::Malformed)?,
            index: &cfg.index,
            predef: &cfg.predef,
        },
        index,
    };
    let (items, states) = if t.at(0).ok_or(Miss::Malformed)?.text() == Some("9") {
        (calc_items(&d, t)?, false)
    } else {
        tree_items(&d, t)?
    };
    Ok(Predefined {
        items,
        source: Some(if keep_source || states {
            t.clone()
        } else {
            Node::empty()
        }),
    })
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn decode_predefined(
    cfg: &Configuration,
    owner: &MdObject,
    entry: &Node,
) -> Option<Predefined> {
    xsi_type(owner.kind)?;
    decode(cfg, &index_for(cfg, owner, entry), entry, true).ok()
}

fn index_for(cfg: &Configuration, owner: &MdObject, entry: &Node) -> HashMap<Guid, String> {
    let mut tables: Vec<(MdKind, &str, &Node)> = Vec::new();
    let mut seen = false;
    for o in &cfg.objects {
        if o.id == owner.id && o.kind == owner.kind {
            tables.push((o.kind, o.name.as_str(), entry));
            seen = true;
        } else if let Some(src) = o.predefined().and_then(|p| p.source.as_ref()) {
            tables.push((o.kind, o.name.as_str(), src));
        }
    }
    if !seen {
        tables.push((owner.kind, owner.name.as_str(), entry));
    }
    item_index(tables)
}

fn root_of(d: &EntryData) -> Option<&Node> {
    match d {
        EntryData::Brace(doc) => Some(&doc.root),
        _ => None,
    }
}

pub(crate) fn load(cx: &LoadCtx, cfg: &mut Configuration) -> Result<()> {
    let mut tables: Vec<(usize, Arc<EntryData>)> = Vec::new();
    for i in 0..cfg.objects.len() {
        let Some(name) = cfg
            .objects
            .get(i)
            .and_then(|o| Some(format!("{}.{}", o.id, table_suffix(o.kind)?)))
        else {
            continue;
        };
        if !cx.entries.contains(&name) {
            continue;
        }
        match fetch(cx, &name)? {
            Some(d) if root_of(&d).is_some() => tables.push((i, d)),
            _ => miss(cfg, &[NOT_BRACE]),
        }
    }
    let index = item_index(tables.iter().filter_map(|(i, d)| {
        let o = cfg.objects.get(*i)?;
        Some((o.kind, o.name.as_str(), root_of(d)?))
    }));
    let mut decoded = Vec::with_capacity(tables.len());
    for (i, d) in &tables {
        if let Some(t) = root_of(d) {
            decoded.push((*i, decode(cfg, &index, t, cx.opts.keep_source)));
        }
    }
    for (i, res) in decoded {
        match res {
            Ok(p) => {
                if let Some(o) = cfg.objects.get_mut(i) {
                    o.parts.predefined = Some(p);
                }
            }
            Err(m) => miss(cfg, &[&m.key()]),
        }
    }
    Ok(())
}

fn field(cols: Columns, id: &str) -> Option<&'static str> {
    cols.iter().find(|(c, _)| *c == id).map(|(_, f)| *f)
}
