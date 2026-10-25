use super::{XML_HEAD, bool_text};
use crate::codec::predefined::{PredefBody, iter_children, parse_table};
use crate::paths::object_dir;
use crate::{DUMP_VERSION, DumpSink, Part, PartStatus, Result, XmlWriter};
use brace::{Guid, Node};
use md_model::model::{DateFractions, MdRef, TypeDescription};
use md_model::parts::*;
use md_model::*;
use std::collections::HashMap;
use std::hash::Hash;

pub fn xsi_type(kind: MdKind) -> Option<&'static str> {
    match kind {
        MdKind::Catalog => Some("CatalogPredefinedItems"),
        MdKind::ChartOfCharacteristicTypes => Some("PlanOfCharacteristicKindPredefinedItems"),
        MdKind::ChartOfAccounts => Some("ChartOfAccountsPredefinedItems"),
        MdKind::ChartOfCalculationTypes => Some("CalculationTypePredefinedItems"),
        _ => None,
    }
}

pub type Columns = &'static [(&'static str, &'static str)];

pub const COLUMNS: [(u32, Columns); 4] = [
    (
        0,
        &[
            ("0", "Ref"),
            ("1", "IsFolder"),
            ("2", "Parent"),
            ("3", "Name"),
            ("4", "Code"),
            ("5", "Description"),
            ("6", "ExtState"),
        ],
    ),
    (
        1,
        &[
            ("1", "Ref"),
            ("2", "IsFolder"),
            ("3", "Name"),
            ("4", "Code"),
            ("5", "Description"),
            ("6", "Type"),
            ("7", "ExtState"),
        ],
    ),
    (
        2,
        &[
            ("0", "Ref"),
            ("1", "Name"),
            ("2", "Code"),
            ("3", "Description"),
            ("4", "AccountType"),
            ("5", "OffBalance"),
            ("6", "ExtDimensionTypes"),
            ("10000", "Order"),
            ("20000", "ExtState"),
        ],
    ),
    (
        9,
        &[
            ("1", "Ref"),
            ("2", "Name"),
            ("3", "Code"),
            ("4", "Description"),
            ("5", "ActionPeriodIsBase"),
            ("6", "Displaced"),
            ("7", "Base"),
            ("8", "Leading"),
            ("9", "ExtState"),
        ],
    ),
];

pub fn columns(kind_no: u32) -> Option<Columns> {
    COLUMNS.iter().find(|(k, _)| *k == kind_no).map(|(_, c)| *c)
}

pub fn column_of(cols: Columns, fld: &str) -> Option<&'static str> {
    cols.iter().find(|(_, f)| *f == fld).map(|(c, _)| *c)
}

pub const EXT_STATE: [(&str, &str); 2] = [("1", "AdoptedCheck"), ("2", "AdoptedNotify")];

pub fn ext_state(code: &str) -> Option<&'static str> {
    EXT_STATE.iter().find(|(c, _)| *c == code).map(|(_, s)| *s)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Miss {
    Bad(String),
    Unrepresentable,
    Malformed,
    TooDeep,
}

pub const NOT_BRACE: &str = "predef:not_brace";

impl Miss {
    pub fn key(&self) -> String {
        match self {
            Miss::Bad(t) => format!("predef:{t}"),
            Miss::Unrepresentable => "predef:unrepresentable".into(),
            Miss::Malformed => "predef:malformed".into(),
            Miss::TooDeep => "predef:too_deep".into(),
        }
    }
}

pub fn bad(text: String) -> Miss {
    Miss::Bad(text)
}

pub type Decoded<T> = std::result::Result<T, Miss>;

pub fn dict<K: Copy + Eq + Hash, V>(pairs: impl IntoIterator<Item = (K, V)>) -> Vec<(K, V)> {
    let mut out: Vec<(K, V)> = Vec::new();
    let mut at: HashMap<K, usize> = HashMap::new();
    for (k, v) in pairs {
        match at.get(&k).and_then(|&i| out.get_mut(i)) {
            Some(slot) => slot.1 = v,
            None => {
                at.insert(k, out.len());
                out.push((k, v));
            }
        }
    }
    out
}

pub fn lookup<V: Copy>(d: &[(&str, V)], key: &str) -> Option<V> {
    d.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

pub fn cells<'a>(row: &'a Node, positions: &[&'a str]) -> Decoded<Vec<(&'a str, &'a Node)>> {
    let l = row.as_list().ok_or(Miss::Malformed)?;
    let nv: usize = l
        .get(2)
        .and_then(Node::text)
        .and_then(|t| t.trim().parse().ok())
        .ok_or(Miss::Malformed)?;
    let vals = l
        .get(3..)
        .and_then(|v| v.get(..nv))
        .ok_or(Miss::Malformed)?;
    let keyed = vals
        .iter()
        .enumerate()
        .map(|(i, v)| Some((*positions.get(i)?, v)))
        .collect::<Option<Vec<_>>>()
        .ok_or(Miss::Malformed)?;
    Ok(dict(keyed))
}

pub fn ref_text(v: Option<&Node>) -> Decoded<&str> {
    let v = v.ok_or(Miss::Malformed)?;
    if v.at(0).ok_or(Miss::Malformed)?.text() != Some("#") {
        return Err(bad("ref".into()));
    }
    v.at(2)
        .and_then(|n| n.at(1))
        .and_then(Node::text)
        .ok_or(Miss::Malformed)
}

fn ext_states(src: &Node) -> Option<HashMap<Guid, &'static str>> {
    let mut out = HashMap::new();
    if src.is_empty_slot() || src.at(0)?.text() == Some("9") {
        return Some(out);
    }
    let tb = parse_table(src)?;
    let cols = columns(tb.kind_no)?;
    let (Some(rc), Some(sc)) = (column_of(cols, "Ref"), column_of(cols, "ExtState")) else {
        return None;
    };
    let PredefBody::Tree(root) = tb.body else {
        return None;
    };
    let mut stack = vec![iter_children(root)?.iter()];
    while let Some(top) = stack.last_mut() {
        let Some(row) = top.next() else {
            stack.pop();
            continue;
        };
        let vals = cells(row, &tb.positions).ok()?;
        let state = lookup(&vals, sc)
            .and_then(|st| st.at(1))
            .and_then(Node::text)
            .and_then(ext_state);
        if let Some(s) = state {
            let id = Guid::parse(ref_text(lookup(&vals, rc)).ok()?)?;
            out.entry(id).or_insert(s);
        }
        stack.push(iter_children(row)?.iter());
    }
    Some(out)
}

fn write_type(w: &mut XmlWriter, t: &TypeDescription) {
    if t.types.is_empty() {
        w.leaf("Type", None, &[]);
        return;
    }
    w.open("Type", &[]);
    let lvl = w.depth() + 1;
    for (set, tag) in [(false, "v8:Type"), (true, "v8:TypeSet")] {
        for r in t.types.iter().filter(|r| r.set == set) {
            match r.qname.strip_prefix("cfg:") {
                Some(n) => w.raw(&format!(
                    "<{tag} xmlns:d{lvl}p1=\"http://v8.1c.ru/8.1/data/enterprise/current-config\">d{lvl}p1:{n}</{tag}>"
                )),
                None => w.leaf(tag, Some(&r.qname), &[]),
            }
        }
    }
    let allowed = |fixed: bool| if fixed { "Fixed" } else { "Variable" };
    if let Some(n) = &t.number {
        w.open("v8:NumberQualifiers", &[]);
        w.leaf("v8:Digits", Some(&n.digits.to_string()), &[]);
        w.leaf("v8:FractionDigits", Some(&n.fraction.to_string()), &[]);
        let sign = if n.nonnegative { "Nonnegative" } else { "Any" };
        w.leaf("v8:AllowedSign", Some(sign), &[]);
        w.close("v8:NumberQualifiers");
    }
    if let Some(s) = &t.string {
        w.open("v8:StringQualifiers", &[]);
        w.leaf("v8:Length", Some(&s.length.to_string()), &[]);
        w.leaf("v8:AllowedLength", Some(allowed(s.fixed)), &[]);
        w.close("v8:StringQualifiers");
    }
    if let Some(d) = &t.date {
        let f = match d {
            DateFractions::Date => "Date",
            DateFractions::Time => "Time",
            DateFractions::DateTime => "DateTime",
        };
        w.open("v8:DateQualifiers", &[]);
        w.leaf("v8:DateFractions", Some(f), &[]);
        w.close("v8:DateQualifiers");
    }
    if let Some(b) = &t.binary {
        w.open("v8:BinaryDataQualifiers", &[]);
        w.leaf("v8:Length", Some(&b.length.to_string()), &[]);
        w.leaf("v8:AllowedLength", Some(allowed(b.fixed)), &[]);
        w.close("v8:BinaryDataQualifiers");
    }
    w.close("Type");
}

fn write_flags(w: &mut XmlWriter, flags: &[(MdRef, bool)]) {
    if flags.is_empty() {
        return;
    }
    w.open("AccountingFlags", &[]);
    for (r, v) in flags {
        w.leaf("Flag", Some(bool_text(*v)), &[("ref", &r.name)]);
    }
    w.close("AccountingFlags");
}

fn write_account(w: &mut XmlWriter, a: &AccountData) {
    let t = match a.account_type {
        AccountType::Active => "Active",
        AccountType::Passive => "Passive",
        AccountType::ActivePassive => "ActivePassive",
    };
    w.leaf("AccountType", Some(t), &[]);
    w.leaf("OffBalance", Some(bool_text(a.off_balance)), &[]);
    w.leaf("Order", Some(&a.order), &[]);
    write_flags(w, &a.flags);
    match a.ext_dimension_types.as_deref() {
        None => {}
        Some([]) => w.leaf("ExtDimensionTypes", None, &[]),
        Some(dims) => {
            w.open("ExtDimensionTypes", &[]);
            for e in dims {
                w.open("ExtDimensionType", &[("name", &e.kind.name)]);
                w.leaf("Turnover", Some(bool_text(e.turnover)), &[]);
                write_flags(w, &e.flags);
                w.close("ExtDimensionType");
            }
            w.close("ExtDimensionTypes");
        }
    }
}

fn write_calc(w: &mut XmlWriter, c: &CalcTypeData) {
    w.leaf(
        "ActionPeriodIsBase",
        Some(bool_text(c.action_period_is_base)),
        &[],
    );
    for (tag, list) in [
        ("Base", &c.base),
        ("Leading", &c.leading),
        ("Displaced", &c.displaced),
    ] {
        if list.is_empty() {
            continue;
        }
        w.open(tag, &[]);
        for r in list {
            w.leaf("CalculationType", Some(&r.name), &[]);
        }
        w.close(tag);
    }
}

fn write_item(w: &mut XmlWriter, it: &PredefinedItem, states: &HashMap<Guid, &str>) {
    w.open("Item", &[("id", &it.id.to_string())]);
    w.leaf("Name", Some(&it.name), &[]);
    match &it.code {
        PredefinedCode::Number(n) => w.leaf("Code", Some(n), &[("xsi:type", "xs:decimal")]),
        PredefinedCode::Str(s) => w.leaf("Code", Some(s), &[]),
    }
    w.leaf("Description", Some(&it.description), &[]);
    if let Some(c) = &it.calc {
        write_calc(w, c);
        w.close("Item");
        return;
    }
    if let Some(t) = &it.type_desc {
        write_type(w, t);
    }
    match &it.account {
        None => w.leaf("IsFolder", Some(bool_text(it.is_folder)), &[]),
        Some(a) => write_account(w, a),
    }
    if let Some(s) = states.get(&it.id) {
        w.leaf("ExtensionState", Some(s), &[]);
    }
    if !it.children.is_empty() {
        w.open("ChildItems", &[]);
        for k in &it.children {
            write_item(w, k, states);
        }
        w.close("ChildItems");
    }
    w.close("Item");
}

pub fn predefined_xml(_cfg: &Configuration, owner: &MdObject, p: &Predefined) -> Option<String> {
    let src = p.source.as_ref()?;
    let xsi = xsi_type(owner.kind)?;
    let states = ext_states(src)?;
    let mut w = XmlWriter::new(1);
    for it in &p.items {
        write_item(&mut w, it, &states);
    }
    let body = w.text();
    let sep = if body.is_empty() { "" } else { "\r\n" };
    Some(format!(
        "{XML_HEAD}\r\n<PredefinedData xmlns=\"http://v8.1c.ru/8.3/xcf/predef\" xmlns:v8=\"http://v8.1c.ru/8.1/data/core\" \
         xmlns:xr=\"http://v8.1c.ru/8.3/xcf/readable\" xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" \
         xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:type=\"{xsi}\" version=\"{DUMP_VERSION}\">{sep}{body}\r\n</PredefinedData>"
    ))
}

pub fn dump(cfg: &Configuration, sink: &mut DumpSink) -> Result<PartStatus> {
    for o in &cfg.objects {
        let Some(xml) = o.predefined().and_then(|p| predefined_xml(cfg, o, p)) else {
            continue;
        };
        let rel = format!("{}/Ext/Predefined.xml", object_dir(cfg, o));
        sink.put(Part::Predefined, &rel, xml.as_bytes())?;
    }
    Ok(PartStatus::Written)
}
