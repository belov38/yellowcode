use std::collections::{BTreeMap, HashMap};

use base64::Engine as _;
use ibschema::fields::FieldValue;
use ibschema::names::{StdField, role};
use ibschema::storage::{FieldId, StorageMap, TableId, TableInfo};
use ibschema::store::{ReadTxn, Row};
use ibschema::values::RefId;
use md5::{Digest, Md5};
use mdconf::{Child, ChildTag, Configuration, MdKind, MdObject, Value};
use serde_json::{Map, Value as J, json};

use crate::{Error, Infobase, Result};

pub const B64MAX: usize = 1 << 20;

const INFLATE_LIMIT: u64 = 256 << 20;

const ZERO_UUID: &str = "00000000-0000-0000-0000-000000000000";

fn any(why: &str) -> J {
    json!({ "$any": why })
}

fn bad(what: impl std::fmt::Display) -> Error {
    Error::Inconsistent(format!("dumpall: {what}"))
}

pub fn dumpall(ib: &Infobase) -> Result<J> {
    let Some(cfg) = ib.configuration() else {
        return Err(Error::Unsupported("dumpall needs a configuration".into()));
    };
    let store = ib.store();
    let txn = store.read()?;
    let cx = Cx::new(&cfg, ib.storage(), txn.as_ref());

    let mut enums = Map::new();
    let mut constants = Map::new();
    let mut objects = Map::new();
    let mut registers = Map::new();
    let mut skipped = Vec::new();
    for o in &cfg.objects {
        let name = full_name(o);
        match o.kind {
            MdKind::Enum => {
                enums.insert(name, cx.decode_enum(o)?);
            }
            MdKind::Constant => {
                constants.insert(name, cx.decode_constant(o)?);
            }
            MdKind::Catalog
            | MdKind::Document
            | MdKind::ChartOfCharacteristicTypes
            | MdKind::ExchangePlan => {
                objects.insert(name, cx.decode_objects(o)?);
            }
            MdKind::InformationRegister | MdKind::AccumulationRegister => {
                registers.insert(name, cx.decode_register(o)?);
            }
            MdKind::AccountingRegister
            | MdKind::CalculationRegister
            | MdKind::ChartOfAccounts
            | MdKind::ChartOfCalculationTypes
            | MdKind::BusinessProcess
            | MdKind::Task
            | MdKind::DocumentJournal
            | MdKind::Sequence => skipped.push(J::String(name)),
            _ => {}
        }
    }
    let mut changes = Map::new();
    for plan in cfg.of_kind(MdKind::ExchangePlan) {
        for item in plan.exchange_content().unwrap_or_default() {
            let Some(o) = item.object.id.and_then(|id| cfg.object(id)) else {
                continue;
            };
            if let Some(t) = cx.changes_table(o) {
                changes.insert(
                    format!("{}.Changes", full_name(o)),
                    cx.decode_changes(o, t)?,
                );
            }
        }
    }

    let cfg_name = cfg
        .props
        .iter()
        .find(|p| p.name == "Name")
        .and_then(|p| match &p.value {
            Value::Text(t) => Some(t.clone()),
            _ => None,
        })
        .unwrap_or_default();
    Ok(json!({
        "service": "onec",
        "format": "1",
        "platform": "8.5.1.1529",
        "config": {"name": cfg_name},
        "b64max": B64MAX.to_string(),
        "enums": enums,
        "constants": constants,
        "objects": objects,
        "registers": registers,
        "changes": changes,
        "errors": [],
        "skipped": skipped,
    }))
}

fn full_name(o: &MdObject) -> String {
    format!("{}.{}", o.kind.as_str(), o.name)
}

struct RefType {
    name: String,
    kind: MdKind,
    enum_values: Vec<(RefId, String)>,
}

struct Cx<'a> {
    map: &'a StorageMap,
    txn: &'a (dyn ReadTxn + Send),
    refs: HashMap<u32, RefType>,
}

fn row_field<'r>(t: &TableInfo, row: &'r Row, s: StdField) -> Option<&'r FieldValue> {
    let f = t.field_by_std(s)?;
    row.get(usize::from(f.0))
}

fn uuid_text(v: &FieldValue) -> Option<String> {
    match v {
        FieldValue::Ref { id, .. } => Some(id.to_uuid().to_string()),
        FieldValue::Uuid(u) => Some(u.to_string()),
        _ => None,
    }
}

pub fn num_json(s: &str) -> String {
    let (neg, body) = match s.strip_prefix('-') {
        Some(b) => (true, b),
        None => (false, s),
    };
    let (int, frac) = body.split_once('.').unwrap_or((body, ""));
    let frac = frac.trim_end_matches('0');
    let int = int.trim_start_matches('0');
    let int = if int.is_empty() { "0" } else { int };
    if int == "0" && frac.is_empty() {
        return "0".into();
    }
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    out.push_str(int);
    if !frac.is_empty() {
        out.push('.');
        out.push_str(frac);
    }
    out
}

fn date_json(d: &onecd::DateTime) -> String {
    if *d == onecd::DateTime::EMPTY {
        "0001-01-01T00:00:00".into()
    } else {
        d.to_string()
    }
}

fn md5_hex(b: &[u8]) -> String {
    Md5::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}

fn prim_json(v: Option<&FieldValue>) -> J {
    match v {
        Some(FieldValue::Null) => J::Null,
        Some(FieldValue::Bool(b)) => J::Bool(*b),
        Some(FieldValue::Number(n)) => J::String(num_json(&n.to_string())),
        Some(FieldValue::Date(d)) => J::String(date_json(d)),
        Some(FieldValue::String(s)) => J::String(s.clone()),
        Some(v @ (FieldValue::Ref { .. } | FieldValue::Uuid(_))) => {
            uuid_text(v).map_or(J::Null, J::String)
        }
        other => J::String(format!("!unexpected-standard-value:{other:?}")),
    }
}

fn bool_of(v: Option<&FieldValue>, what: &str) -> Result<bool> {
    match v {
        Some(FieldValue::Bool(b)) => Ok(*b),
        other => Err(bad(format!("{what}: expected a boolean, got {other:?}"))),
    }
}

const VS_BINARY: &str = "87126200-3e98-44e0-b931-ccb1d7edc497";
const VS_STRUCTURE: &str = "4238019d-7e49-4fc9-91db-b6b951d5cf8e";
const BOM: &[u8] = b"\xef\xbb\xbf";

pub fn storage_json(raw: &[u8], b64max: usize) -> J {
    let mut o = Map::new();
    let (body, compressed) = match raw {
        [1, 1, rest @ ..] => (rest.to_vec(), false),
        [2, 1, rest @ ..] if rest.len() >= 16 => {
            match zraw::inflate(rest.get(16..).unwrap_or_default(), INFLATE_LIMIT) {
                Ok(out) => (out, true),
                Err(_) => return json!({"payloadType": "!bad-deflate"}),
            }
        }
        _ => {
            let head: String = raw.iter().take(2).map(|b| format!("{b:02x}")).collect();
            return json!({ "payloadType": format!("!unknown-storage:{head}") });
        }
    };
    let parsed = (|| {
        let tl = usize::try_from(u64::from_le_bytes(body.get(..8)?.try_into().ok()?)).ok()?;
        let end = 8usize.checked_add(tl)?;
        let text = body.get(8..end)?;
        let tail = body.get(end..)?;
        let text = text.strip_prefix(BOM).unwrap_or(text);
        let text = std::str::from_utf8(text).ok()?;
        Some((text.to_string(), tail.to_vec()))
    })();
    match parsed {
        Some((text, _)) if text == "{\"U\"}" => {
            o.insert("payloadType".into(), J::Null);
        }
        Some((text, tail)) if text.starts_with(&format!("{{\"#\",{VS_BINARY},")) => {
            let payload = (|| {
                let n =
                    usize::try_from(u64::from_le_bytes(tail.get(4..12)?.try_into().ok()?)).ok()?;
                let p = tail.get(12..)?;
                (p.len() == n).then(|| p.to_vec())
            })();
            match payload {
                Some(p) => {
                    o.insert("payloadType".into(), J::String("base64Binary".into()));
                    o.insert("size".into(), J::String(p.len().to_string()));
                    o.insert("md5".into(), J::String(md5_hex(&p)));
                    if p.len() <= b64max {
                        o.insert(
                            "base64".into(),
                            J::String(base64::engine::general_purpose::STANDARD.encode(&p)),
                        );
                    }
                }
                None => {
                    o.insert("payloadType".into(), J::String("!bad-binary-size".into()));
                }
            }
        }
        Some((text, _)) if text.starts_with(&format!("{{\"#\",{VS_STRUCTURE},")) => {
            o.insert("payloadType".into(), J::String("Структура".into()));
            o.insert("internal".into(), J::String(text.replace("\r\n", "\n")));
        }
        _ => {
            o.insert("payloadType".into(), J::String("!unknown-payload".into()));
        }
    }
    if compressed {
        let why = "compressed storage: 1C serializes it again";
        o.insert("rawSize".into(), any(why));
        o.insert("rawMd5".into(), any(why));
    } else {
        o.insert("rawSize".into(), J::String(raw.len().to_string()));
        o.insert("rawMd5".into(), J::String(md5_hex(raw)));
    }
    J::Object(o)
}

struct Attr<'m> {
    child: &'m Child,
    field: FieldId,
    composite: bool,
}

impl<'a> Cx<'a> {
    fn new(cfg: &Configuration, map: &'a StorageMap, txn: &'a (dyn ReadTxn + Send)) -> Cx<'a> {
        let mut refs = HashMap::new();
        for o in &cfg.objects {
            let Some(st) = map.object(o.id) else { continue };
            let number = map.table(st.main).number;
            if number == 0 {
                continue;
            }
            let enum_values = o
                .enum_values()
                .map(|v| (RefId::from_uuid(v.id), v.name.clone()))
                .collect();
            refs.insert(
                number,
                RefType {
                    name: format!("{}Ref.{}", o.kind.as_str(), o.name),
                    kind: o.kind,
                    enum_values,
                },
            );
        }
        Cx { map, txn, refs }
    }

    fn rows(&self, t: TableId) -> Result<Vec<Row>> {
        let mut out = Vec::new();
        for r in self.txn.scan(t)? {
            out.push(r?.1);
        }
        Ok(out)
    }

    fn storage_of(&self, o: &MdObject) -> Result<&'a ibschema::storage::ObjectStorage> {
        self.map
            .object(o.id)
            .ok_or_else(|| bad(format!("{} has no tables", full_name(o))))
    }

    fn attrs<'m>(
        &self,
        owner: &str,
        children: impl Iterator<Item = &'m Child>,
        table: TableId,
    ) -> Result<Vec<Attr<'m>>> {
        let mut out = Vec::new();
        for child in children {
            let (t, field) = self
                .map
                .field(child.id)
                .ok_or_else(|| bad(format!("{owner}.{}: no field in the storage", child.name)))?;
            if t != table {
                return Err(bad(format!(
                    "{owner}.{}: the field is in another table",
                    child.name
                )));
            }
            out.push(Attr {
                child,
                field,
                composite: self.composite(self.map.table(t), field, child),
            });
        }
        Ok(out)
    }

    fn composite(&self, t: &TableInfo, f: FieldId, child: &Child) -> bool {
        let several = child.type_desc().is_some_and(|d| d.types.len() > 1);
        let stored = t.field(f).is_some_and(|fi| {
            t.columns.get(fi.columns.clone()).is_some_and(|cols| {
                cols.first().is_some_and(|c| {
                    matches!(
                        c.part,
                        ibschema::columns::Part::Type | ibschema::columns::Part::TRef
                    )
                })
            })
        });
        several || stored
    }

    fn type_name(&self, v: &FieldValue) -> String {
        match v {
            FieldValue::Null => "Null".into(),
            FieldValue::Undefined => "Undefined".into(),
            FieldValue::String(_) => "string".into(),
            FieldValue::Number(_) => "decimal".into(),
            FieldValue::Date(_) => "dateTime".into(),
            FieldValue::Bool(_) => "boolean".into(),
            FieldValue::Uuid(_) => "UUID".into(),
            FieldValue::Storage(_) => "ValueStorage".into(),
            FieldValue::Ref { table, .. } => self
                .refs
                .get(table)
                .map_or_else(|| format!("!unknown-ref-table:{table}"), |r| r.name.clone()),
            FieldValue::Binary(_) | FieldValue::RowVersion(_) => "!unexpected".into(),
        }
    }

    fn plain(&self, v: &FieldValue) -> J {
        match v {
            FieldValue::Null | FieldValue::Undefined => J::Null,
            FieldValue::String(s) => J::String(s.clone()),
            FieldValue::Number(n) => J::String(num_json(&n.to_string())),
            FieldValue::Date(d) => J::String(date_json(d)),
            FieldValue::Bool(b) => J::Bool(*b),
            FieldValue::Uuid(u) => J::String(u.to_string()),
            FieldValue::Ref { table, id } => match self.refs.get(table) {
                Some(r) if r.kind == MdKind::Enum => J::String(if id.is_nil() {
                    String::new()
                } else {
                    r.enum_values.iter().find(|(b, _)| b == id).map_or_else(
                        || format!("!unknown-enum-value:{}", id.to_uuid()),
                        |(_, n)| n.clone(),
                    )
                }),
                _ => J::String(id.to_uuid().to_string()),
            },
            FieldValue::Storage(raw) => storage_json(raw, B64MAX),
            FieldValue::Binary(_) | FieldValue::RowVersion(_) => J::String("!unexpected".into()),
        }
    }

    fn value_json(&self, v: &FieldValue, composite: bool) -> J {
        if composite {
            return match v {
                FieldValue::Undefined => J::Null,
                FieldValue::Null => json!({"type": "Null", "value": null}),
                _ => json!({"type": self.type_name(v), "value": self.plain(v)}),
            };
        }
        self.plain(v)
    }

    fn attrs_json(&self, out: &mut Map<String, J>, attrs: &[Attr], row: &Row) {
        for a in attrs {
            let j = match row.get(usize::from(a.field.0)) {
                Some(v) => self.value_json(v, a.composite),
                None => J::String("!short-row".into()),
            };
            out.insert(a.child.name.clone(), j);
        }
    }

    fn decode_enum(&self, o: &MdObject) -> Result<J> {
        let st = self.storage_of(o)?;
        let t = self.map.table(st.main);
        let mut by_order: BTreeMap<i64, RefId> = BTreeMap::new();
        for row in self.rows(st.main)? {
            let order = match row_field(t, &row, StdField::EnumOrder) {
                Some(FieldValue::Number(n)) => n.to_string().parse::<i64>().unwrap_or(-1),
                _ => -1,
            };
            if let Some(FieldValue::Ref { id, .. }) = row_field(t, &row, StdField::Id) {
                by_order.insert(order, *id);
            }
        }
        let tid = o.type_id("Ref").map(|g| g.to_string()).unwrap_or_default();
        let mut a = Vec::new();
        for (k, v) in o.enum_values().enumerate() {
            let hex: String = i64::try_from(k)
                .ok()
                .and_then(|k| by_order.get(&k))
                .map(|id| id.0.iter().map(|x| format!("{x:02x}")).collect())
                .unwrap_or_else(|| "!missing-enum-order".into());
            a.push(json!({
                "name": v.name,
                "order": k.to_string(),
                "ref": format!("{{\"#\",{tid},{}:{hex}}}", t.number),
            }));
        }
        Ok(J::Array(a))
    }

    fn decode_constant(&self, o: &MdObject) -> Result<J> {
        let st = self.storage_of(o)?;
        let (t, f) = self
            .map
            .field(o.id)
            .ok_or_else(|| bad(format!("{}: no value field", full_name(o))))?;
        let composite = match o.prop("Type") {
            Some(Value::Type(d)) => d.types.len() > 1,
            _ => false,
        } || self.stored_composite(t, f);
        let rows = self.rows(st.main)?;
        match rows.as_slice() {
            [row] => Ok(match row.get(usize::from(f.0)) {
                Some(v) => self.value_json(v, composite),
                None => J::String("!short-row".into()),
            }),
            rows => Ok(J::String(format!("!constant-rows:{}", rows.len()))),
        }
    }

    fn stored_composite(&self, t: TableId, f: FieldId) -> bool {
        let ti = self.map.table(t);
        ti.field(f)
            .and_then(|fi| ti.columns.get(fi.columns.clone()))
            .and_then(|cols| cols.first())
            .is_some_and(|c| {
                matches!(
                    c.part,
                    ibschema::columns::Part::Type | ibschema::columns::Part::TRef
                )
            })
    }

    fn decode_objects(&self, o: &MdObject) -> Result<J> {
        let name = full_name(o);
        let st = self.storage_of(o)?;
        let t = self.map.table(st.main);
        let rows = self.rows(st.main)?;
        let attrs = self.attrs(&name, o.attributes(), st.main)?;
        let mut tabs = Vec::new();
        for ts in o.tabular_sections() {
            let tid = st
                .tabular
                .iter()
                .find(|(md, _)| *md == ts.id)
                .map(|(_, t)| *t)
                .ok_or_else(|| bad(format!("{name}.{}: no table", ts.name)))?;
            let tattrs = self.attrs(
                &format!("{name}.{}", ts.name),
                ts.children.iter().filter(|c| c.tag == ChildTag::Attribute),
                tid,
            )?;
            let tinfo = self.map.table(tid);
            let mut by_owner: HashMap<RefId, Vec<(f64, Row)>> = HashMap::new();
            for row in self.rows(tid)? {
                let Some(FieldValue::Ref { id, .. }) = row_field(tinfo, &row, StdField::Id) else {
                    return Err(bad(format!("{}: no owner in a row", tinfo.logical)));
                };
                let ln = match row_field(tinfo, &row, StdField::LineNo) {
                    Some(FieldValue::Number(n)) => n.to_string(),
                    other => {
                        return Err(bad(format!("{}: line number {other:?}", tinfo.logical)));
                    }
                };
                let key = ln.parse::<f64>().unwrap_or(0.0);
                by_owner.entry(*id).or_default().push((key, row));
            }
            tabs.push((ts, tinfo, tattrs, by_owner));
        }
        let mut items = Vec::new();
        for row in &rows {
            let item = self.decode_item(o, t, row, &attrs, &tabs)?;
            let key = item
                .get("Ref")
                .and_then(J::as_str)
                .unwrap_or("")
                .to_string();
            items.push((key, item));
        }
        items.sort_by(|a, b| a.0.cmp(&b.0));
        let tid = o.type_id("Ref").map(|g| g.to_string()).unwrap_or_default();
        let count = items.len();
        Ok(json!({
            "refType": format!("{{\"#\",{tid},{}:{}}}", t.number, "0".repeat(32)),
            "items": items.into_iter().map(|x| x.1).collect::<Vec<_>>(),
            "count": count,
        }))
    }

    #[allow(clippy::type_complexity)]
    fn decode_item(
        &self,
        o: &MdObject,
        t: &TableInfo,
        row: &Row,
        attrs: &[Attr],
        tabs: &[(
            &Child,
            &TableInfo,
            Vec<Attr>,
            HashMap<RefId, Vec<(f64, Row)>>,
        )],
    ) -> Result<J> {
        let name = full_name(o);
        let has = |s: StdField| t.field_by_std(s).is_some();
        let std = |s: StdField| row_field(t, row, s);
        let mut m = Map::new();
        let idr = match std(StdField::Id) {
            Some(FieldValue::Ref { id, .. }) => *id,
            other => return Err(bad(format!("{name}: ID holds {other:?}"))),
        };
        m.insert("Ref".into(), J::String(idr.to_uuid().to_string()));
        if has(StdField::PredefinedId) {
            let p = std(StdField::PredefinedId)
                .and_then(uuid_text)
                .ok_or_else(|| bad(format!("{name}: PredefinedID is not a reference")))?;
            let pname = if p == ZERO_UUID {
                String::new()
            } else {
                o.predefined()
                    .and_then(|pd| pd.iter().find(|i| i.id.to_string() == p))
                    .map_or_else(|| format!("!unknown-predefined:{p}"), |i| i.name.clone())
            };
            if o.kind == MdKind::ExchangePlan {
                m.insert("ThisNode".into(), J::Bool(p != ZERO_UUID));
            } else {
                m.insert("PredefinedDataName".into(), J::String(pname));
                m.insert("Predefined".into(), J::Bool(p != ZERO_UUID));
            }
        }
        m.insert(
            "DeletionMark".into(),
            J::Bool(bool_of(std(StdField::Marked), "Marked")?),
        );
        match o.kind {
            MdKind::Document => {
                m.insert(
                    "Posted".into(),
                    J::Bool(bool_of(std(StdField::Posted), "Posted")?),
                );
                m.insert("Date".into(), prim_json(std(StdField::DateTime)));
                m.insert("Number".into(), prim_json(std(StdField::Number)));
            }
            MdKind::ExchangePlan => {
                m.insert("ExchangeDate".into(), prim_json(std(StdField::LastUpdate)));
                m.insert("ReceivedNo".into(), prim_json(std(StdField::ReceivedNo)));
                m.insert("SentNo".into(), prim_json(std(StdField::SentNo)));
            }
            MdKind::ChartOfCharacteristicTypes => {
                m.insert("ValueType".into(), any("type description is not decoded"));
            }
            _ => {}
        }
        if has(StdField::Folder) {
            m.insert(
                "IsFolder".into(),
                J::Bool(!bool_of(std(StdField::Folder), "Folder")?),
            );
        }
        if has(StdField::ParentId) {
            m.insert("Parent".into(), prim_json(std(StdField::ParentId)));
        }
        if has(StdField::OwnerId) {
            m.insert("Owner".into(), prim_json(std(StdField::OwnerId)));
        }
        if has(StdField::Description) {
            m.insert("Description".into(), prim_json(std(StdField::Description)));
        }
        if has(StdField::Code) {
            m.insert("Code".into(), prim_json(std(StdField::Code)));
        }
        self.attrs_json(&mut m, attrs, row);
        for (ts, tinfo, tattrs, by_owner) in tabs {
            let mut lines: Vec<(f64, &Row)> = by_owner
                .get(&idr)
                .map(|v| v.iter().map(|(k, r)| (*k, r)).collect())
                .unwrap_or_default();
            lines.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut out = Vec::new();
            for (_, tr) in lines {
                let mut lm = Map::new();
                let ln = match row_field(tinfo, tr, StdField::LineNo) {
                    Some(FieldValue::Number(n)) => n.to_string(),
                    _ => String::new(),
                };
                lm.insert("LineNumber".into(), J::String(num_json(&ln)));
                self.attrs_json(&mut lm, tattrs, tr);
                out.push(J::Object(lm));
            }
            m.insert(ts.name.clone(), J::Array(out));
        }
        Ok(J::Object(m))
    }

    fn decode_register(&self, o: &MdObject) -> Result<J> {
        let name = full_name(o);
        let st = self.storage_of(o)?;
        let t = self.map.table(st.main);
        let accum = o.kind == MdKind::AccumulationRegister;
        let balance = accum && t.field_by_std(StdField::RecordKind).is_some();
        let dims = self.attrs(&name, o.dimensions(), st.main)?;
        let resources = self.attrs(&name, o.resources(), st.main)?;
        let attrs = self.attrs(&name, o.attributes(), st.main)?;
        let mut records = Vec::new();
        for row in self.rows(st.main)? {
            let std = |s: StdField| row_field(t, &row, s);
            let has = |s: StdField| t.field_by_std(s).is_some();
            let mut m = Map::new();
            if accum || has(StdField::Recorder) {
                m.insert("Recorder".into(), prim_json(std(StdField::Recorder)));
                m.insert("LineNumber".into(), prim_json(std(StdField::LineNo)));
                if !accum {
                    m.insert("Active".into(), prim_json(std(StdField::Active)));
                }
            }
            if accum || has(StdField::Period) {
                m.insert("Period".into(), prim_json(std(StdField::Period)));
            }
            if accum {
                if balance {
                    let kind = match std(StdField::RecordKind) {
                        Some(FieldValue::Number(n)) => match n.to_string().as_str() {
                            "0" => "Receipt".to_string(),
                            "1" => "Expense".to_string(),
                            other => format!("!bad-recordkind:{other}"),
                        },
                        v => format!("!bad-recordkind:{v:?}"),
                    };
                    m.insert("RecordType".into(), J::String(kind));
                }
                m.insert("Active".into(), prim_json(std(StdField::Active)));
            }
            self.attrs_json(&mut m, &dims, &row);
            self.attrs_json(&mut m, &resources, &row);
            self.attrs_json(&mut m, &attrs, &row);
            records.push(J::Object(m));
        }
        let mut sec = Map::new();
        sec.insert("count".into(), json!(records.len()));
        if balance {
            sec.insert(
                "balance".into(),
                balance_json(&name, &dims, &resources, &records)?,
            );
        }
        sec.insert("records".into(), J::Array(records));
        Ok(J::Object(sec))
    }

    fn changes_table(&self, o: &MdObject) -> Option<TableId> {
        let st = self.map.object(o.id)?;
        st.service
            .iter()
            .find(|(r, _)| r == role::CHANGE_RECORDS || r == role::CONSTANT_CHANGE_RECORDS)
            .map(|(_, t)| *t)
    }

    fn decode_changes(&self, o: &MdObject, table: TableId) -> Result<J> {
        let t = self.map.table(table);
        let mut out = Vec::new();
        for row in self.rows(table)? {
            let mut m = Map::new();
            m.insert("Node".into(), prim_json(row_field(t, &row, StdField::Node)));
            m.insert(
                "MessageNo".into(),
                prim_json(row_field(t, &row, StdField::MessageNo)),
            );
            if o.kind != MdKind::Constant && t.field_by_std(StdField::Id).is_some() {
                m.insert("Ref".into(), prim_json(row_field(t, &row, StdField::Id)));
            }
            out.push(J::Object(m));
        }
        Ok(J::Array(out))
    }
}

fn fixed(x: f64, fraction: usize) -> String {
    format!("{:.*}", fraction.min(38), x)
}

fn balance_json(name: &str, dims: &[Attr], resources: &[Attr], records: &[J]) -> Result<J> {
    let mut sums: BTreeMap<String, (Map<String, J>, Vec<f64>)> = BTreeMap::new();
    let mut scale = Vec::new();
    for res in resources {
        match res.child.type_desc().and_then(|d| d.number) {
            Some(q) => scale.push(q.fraction),
            None => return Err(bad(format!("{name}: resource is not a number"))),
        }
    }
    for rec in records {
        if rec.get("Active") != Some(&J::Bool(true)) {
            continue;
        }
        let sign = if rec.get("RecordType") == Some(&J::String("Expense".into())) {
            -1.0
        } else {
            1.0
        };
        let mut dm = Map::new();
        for d in dims {
            dm.insert(
                d.child.name.clone(),
                rec.get(&d.child.name).cloned().unwrap_or(J::Null),
            );
        }
        let key = J::Object(dm.clone()).to_string();
        let e = sums
            .entry(key)
            .or_insert_with(|| (dm, vec![0.0; resources.len()]));
        for (k, res) in resources.iter().enumerate() {
            let x: f64 = rec
                .get(&res.child.name)
                .and_then(J::as_str)
                .unwrap_or("0")
                .parse()
                .unwrap_or(0.0);
            if let Some(s) = e.1.get_mut(k) {
                *s += sign * x;
            }
        }
    }
    let mut out = Vec::new();
    for (_, (mut dm, s)) in sums {
        let rounded: Vec<String> = s
            .iter()
            .zip(&scale)
            .map(|(x, f)| num_json(&fixed(*x, *f as usize)))
            .collect();
        if rounded.iter().all(|x| x == "0") {
            continue;
        }
        for (res, x) in resources.iter().zip(rounded) {
            dm.insert(format!("{}Balance", res.child.name), J::String(x));
        }
        out.push(J::Object(dm));
    }
    Ok(J::Array(out))
}
