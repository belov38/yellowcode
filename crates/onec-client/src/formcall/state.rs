use roxmltree::{Document, Node};

use super::{
    AttrValue, Described, FormAttr, FormError, Period, Slot, field_cell, main_value, own_value,
    positions, raw_of, shape, ws_children,
};
use crate::answer::{NS_WS, attr};
use crate::formdescr::{FormField, NS_UO};
use crate::metadata::MdType;
use crate::object::{Cell, implied_type, uo_children};
use crate::value::{NS_CONFIG, NS_CORE, NS_XS};
use crate::write::{RawRow, RawTable, value_xml};

const NS_XSI: &str = "http://www.w3.org/2001/XMLSchema-instance";

pub(super) const PERIOD: &str = "2fdc88ec-7c9b-43cd-8ba5-873f043bdd88";

#[derive(Debug, Clone, PartialEq)]
pub(super) enum Kind {
    Main,
    Sheet,
    Rows(Vec<FormField>),
    Period,
    Value,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Row {
    pub(super) id: String,
    pub(super) cells: Vec<Cell>,
    pub(super) raw: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub(super) struct Rows {
    pub(super) last_id: i64,
    pub(super) rows: Vec<Row>,
    pub(super) columns: Vec<String>,
}

impl Rows {
    pub(super) fn raw_table(&self, columns: &[FormField], types: &[MdType]) -> RawTable {
        RawTable {
            last_id: self.last_id,
            rows: self
                .rows
                .iter()
                .map(|r| RawRow {
                    id: r.id.clone(),
                    cells: r
                        .cells
                        .iter()
                        .zip(columns)
                        .zip(&r.raw)
                        .map(|((c, col), raw)| match c {
                            Cell::Value(v) => value_xml(v, implied_type(&col.ty, types).is_none())
                                .unwrap_or_else(|_| raw.clone()),
                            Cell::Raw(_) => raw.clone(),
                        })
                        .collect(),
                })
                .collect(),
            columns: self.columns.clone(),
        }
    }

    pub(super) fn cells(&self) -> Vec<Vec<Cell>> {
        self.rows.iter().map(|r| r.cells.clone()).collect()
    }
}

pub(super) type Place = (usize, Option<usize>);

#[derive(Debug, Clone)]
pub(super) struct FormState {
    pub(super) attrs: Vec<FormAttr>,
    pub(super) kinds: Vec<Kind>,
    pub(super) tables: Vec<(Place, Option<Rows>)>,
    pub(super) sin: u64,
    pub(super) seq: u64,
    pub(super) dd: Option<String>,
}

fn op_index(n: Node, name: &str) -> Result<usize, String> {
    n.attribute(name)
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| format!("uo:{} without its {name}", n.tag_name().name()))
}

fn op_id(n: Node, name: &str) -> Result<i64, String> {
    n.attribute(name)
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| format!("uo:{} without its {name}", n.tag_name().name()))
}

fn row_cells(
    item: Node,
    columns: &[FormField],
    types: &[MdType],
) -> Result<(Vec<Cell>, Vec<String>), String> {
    let holder = uo_children(item)
        .find(|c| c.tag_name().name() == "structure")
        .unwrap_or(item);
    let values: Vec<Node> = uo_children(holder)
        .filter(|v| v.tag_name().name() == "v")
        .collect();
    if values.len() != columns.len() {
        return Err(format!(
            "a row of {} values for {} columns",
            values.len(),
            columns.len()
        ));
    }
    Ok((
        values
            .iter()
            .zip(columns)
            .map(|(v, c)| field_cell(*v, c, types))
            .collect(),
        values.iter().map(|v| raw_of(*v)).collect(),
    ))
}

pub(super) fn apply_rows(
    rows: &mut Rows,
    n: Node,
    columns: &[FormField],
    types: &[MdType],
) -> Result<(), String> {
    let mut given_columns = Vec::new();
    let set_item = |rows: &mut Rows, item: Node| -> Result<(), String> {
        let index = op_index(item, "index")?;
        let id = attr(item, "id").ok_or("uo:item without its id")?;
        let (cells, raw) = row_cells(item, columns, types)?;
        let row = Row { id, cells, raw };
        match index.cmp(&rows.rows.len()) {
            std::cmp::Ordering::Less if rows.rows[index].id != row.id => {
                return Err(format!(
                    "uo:item {index} has the id {} of a row {}",
                    row.id, rows.rows[index].id
                ));
            }
            std::cmp::Ordering::Less => rows.rows[index] = row,
            std::cmp::Ordering::Equal => rows.rows.push(row),
            std::cmp::Ordering::Greater => {
                return Err(format!("uo:item {index} after {} rows", rows.rows.len()));
            }
        }
        Ok(())
    };
    let placeholder = |id: String| Row {
        id,
        cells: Vec::new(),
        raw: Vec::new(),
    };
    for c in uo_children(n) {
        match c.tag_name().name() {
            "operations" => {
                for op in uo_children(c) {
                    let len = rows.rows.len();
                    match op.tag_name().name() {
                        "add" => {
                            let at = op_index(op, "index")?;
                            if at > len {
                                return Err(format!("uo:add at {at} of {len} rows"));
                            }
                            rows.rows
                                .insert(at, placeholder(op_id(op, "id")?.to_string()));
                        }
                        "addRange" => {
                            let (from, to) = (op_index(op, "index1")?, op_index(op, "index2")?);
                            let start = op_id(op, "startId")?;
                            if from > len || to < from {
                                return Err(format!("uo:addRange {from}..{to} of {len} rows"));
                            }
                            for (k, at) in (from..=to).enumerate() {
                                let id = start + i64::try_from(k).map_err(|e| e.to_string())?;
                                rows.rows.insert(at, placeholder(id.to_string()));
                            }
                        }
                        "remove" => {
                            let at = op_index(op, "index")?;
                            if at >= len {
                                return Err(format!("uo:remove at {at} of {len} rows"));
                            }
                            rows.rows.remove(at);
                        }
                        "clear" => rows.rows.clear(),
                        other => return Err(format!("unknown operation uo:{other}")),
                    }
                }
            }
            "items" => {
                for item in uo_children(c).filter(|i| i.tag_name().name() == "item") {
                    set_item(rows, item)?;
                }
            }
            "item" => set_item(rows, c)?,
            "column" => given_columns.push(raw_of(c)),
            other => return Err(format!("unexpected uo:{other} in a collection")),
        }
    }
    if let Some(count) = n.attribute("count") {
        let count: usize = count.parse().map_err(|_| format!("bad count {count:?}"))?;
        if count != rows.rows.len() {
            return Err(format!("{} rows for count {count}", rows.rows.len()));
        }
    }
    if let Some(last) = n.attribute("lastId") {
        rows.last_id = last.parse().map_err(|_| format!("bad lastId {last:?}"))?;
    }
    if rows.rows.iter().any(|r| r.cells.len() != columns.len()) {
        return Err("an added row without its values".to_string());
    }
    if !given_columns.is_empty() {
        rows.columns = given_columns;
    }
    Ok(())
}

pub(super) fn period_of(n: Node) -> Option<Period> {
    let core = |local: &str| {
        n.children()
            .find(|c| {
                c.is_element()
                    && c.tag_name().namespace() == Some(NS_CORE)
                    && c.tag_name().name() == local
            })
            .map(|c| c.text().unwrap_or("").to_string())
    };
    let variant = core("variant")?;
    let custom = variant == "Custom";
    Some(Period {
        variant,
        start: core("startDate").filter(|_| custom),
        end: core("endDate").filter(|_| custom),
    })
}

pub(super) fn kinds_of(data: &Described, attrs: &[FormAttr]) -> Vec<Kind> {
    attrs
        .iter()
        .map(|f| {
            let a = &f.attribute;
            if f.main && data.data.is_some() {
                Kind::Main
            } else if a.is_spreadsheet() {
                Kind::Sheet
            } else if let Some(columns) = a.object.as_ref().and_then(|s| data.collections.get(s)) {
                Kind::Rows(columns.clone())
            } else if a.field.ty.ids.len() == 1 && a.field.ty.ids[0].eq_ignore_ascii_case(PERIOD) {
                Kind::Period
            } else {
                Kind::Value
            }
        })
        .collect()
}

fn full_value(
    n: Node,
    place: usize,
    a: &FormAttr,
    k: &Kind,
    data: &Described,
    types: &[MdType],
    tables: &mut Vec<(Place, Option<Rows>)>,
) -> AttrValue {
    match (k, &data.data) {
        (Kind::Main, Some(d)) => {
            let v = main_value(n, d, types);
            let values: Vec<Node> = uo_children(n)
                .filter(|v| v.tag_name().name() == "v")
                .collect();
            for (x, slot) in values.iter().zip(&d.layout) {
                if let Slot::Table(ti) = slot {
                    let mut rows = Rows::default();
                    let ok = apply_rows(&mut rows, *x, &d.tables[*ti].columns, types).is_ok();
                    tables.push(((place, Some(*ti)), ok.then_some(rows)));
                }
            }
            v
        }
        (Kind::Rows(columns), _) => {
            let mut rows = Rows::default();
            match apply_rows(&mut rows, n, columns, types) {
                Ok(()) => {
                    let v = AttrValue::Rows {
                        columns: columns.clone(),
                        rows: rows.cells(),
                    };
                    tables.push(((place, None), Some(rows)));
                    v
                }
                Err(_) => {
                    tables.push(((place, None), None));
                    AttrValue::Other(raw_of(n))
                }
            }
        }
        (Kind::Period, _) => {
            period_of(n).map_or_else(|| AttrValue::Other(raw_of(n)), AttrValue::Period)
        }
        _ => own_value(n, &a.attribute, types),
    }
}

impl FormState {
    pub(super) fn from_values(
        mut attrs: Vec<FormAttr>,
        values: &[Option<Node>],
        data: &Described,
        types: &[MdType],
    ) -> FormState {
        let kinds = kinds_of(data, &attrs);
        let mut tables = Vec::new();
        for (place, (a, v)) in attrs.iter_mut().zip(values).enumerate() {
            a.value = match v {
                Some(n) => full_value(*n, place, a, &kinds[place], data, types, &mut tables),
                None => AttrValue::Other(String::new()),
            };
        }
        FormState {
            attrs,
            kinds,
            tables,
            sin: 0,
            seq: 0,
            dd: None,
        }
    }

    pub(super) fn rows(&self, place: Place) -> Option<&Rows> {
        self.tables
            .iter()
            .find(|(p, _)| *p == place)
            .and_then(|(_, r)| r.as_ref())
    }

    pub(super) fn apply(
        &mut self,
        body: &str,
        data: &Described,
        types: &[MdType],
        warnings: &mut Vec<String>,
    ) -> Result<(), FormError> {
        const WHAT: &str = "logForm/call";
        let doc = Document::parse(body).map_err(|e| shape(WHAT, e.to_string()))?;
        let root = doc.root_element();
        if let Some(dd) = ws_children(root, "dd").next() {
            self.dd = Some(raw_of(dd));
        }
        let Some(pchs) = ws_children(root, "pchs").next() else {
            return Ok(());
        };
        let counter = |name: &str| pchs.attribute(name).and_then(|v| v.parse::<u64>().ok());
        if let (Some(sin), Some(seq)) = (counter("sin"), counter("seq")) {
            (self.sin, self.seq) = (sin, seq);
        }
        let Some(structure) = uo_children(pchs).find(|c| c.tag_name().name() == "structure") else {
            return Ok(());
        };
        let values: Vec<Node> = uo_children(structure)
            .filter(|v| v.tag_name().name() == "v")
            .collect();
        let places = positions(structure, values.len(), WHAT)?;
        for (place, v) in places.into_iter().zip(values) {
            if place >= self.attrs.len() {
                return Err(shape(
                    WHAT,
                    format!("pchs names the attribute {place} of {}", self.attrs.len()),
                ));
            }
            let name = self.attrs[place].attribute.field.display_name().to_string();
            match (self.kinds[place].clone(), &data.data) {
                (Kind::Main, Some(d)) => {
                    let inner: Vec<Node> = uo_children(v)
                        .filter(|x| x.tag_name().name() == "v")
                        .collect();
                    let slots = positions(v, inner.len(), WHAT)?;
                    for (p, x) in slots.into_iter().zip(inner) {
                        match d.layout.get(p) {
                            Some(Slot::Field(i)) => {
                                let f = &d.fields[*i];
                                let new = field_cell(x, f, types);
                                if let AttrValue::Object { fields, .. } =
                                    &mut self.attrs[place].value
                                    && let Some((_, c)) =
                                        fields.iter_mut().find(|(g, _)| g.id == f.id)
                                {
                                    *c = new;
                                }
                            }
                            Some(Slot::Table(ti)) => {
                                let t = &d.tables[*ti];
                                let path = format!("{name}.{}", t.field.display_name());
                                let rows = self.apply_table(
                                    (place, Some(*ti)),
                                    x,
                                    &t.columns,
                                    types,
                                    warnings,
                                    &path,
                                );
                                if let (Some(rows), AttrValue::Object { tables, .. }) =
                                    (rows, &mut self.attrs[place].value)
                                    && let Some(table) = tables.get_mut(*ti)
                                {
                                    table.2 = rows;
                                }
                            }
                            Some(Slot::Other) => {}
                            None => {
                                return Err(shape(
                                    WHAT,
                                    format!(
                                        "pchs names the field {p} of {} of {name}",
                                        d.layout.len()
                                    ),
                                ));
                            }
                        }
                    }
                }
                (Kind::Rows(columns), _) => {
                    if let Some(rows) =
                        self.apply_table((place, None), v, &columns, types, warnings, &name)
                    {
                        self.attrs[place].value = AttrValue::Rows { columns, rows };
                    }
                }
                (Kind::Sheet, _) => {}
                (Kind::Period, _) => {
                    self.attrs[place].value =
                        period_of(v).map_or_else(|| AttrValue::Other(raw_of(v)), AttrValue::Period);
                }
                _ => self.attrs[place].value = own_value(v, &self.attrs[place].attribute, types),
            }
        }
        Ok(())
    }

    fn apply_table(
        &mut self,
        place: Place,
        n: Node,
        columns: &[FormField],
        types: &[MdType],
        warnings: &mut Vec<String>,
        name: &str,
    ) -> Option<Vec<Vec<Cell>>> {
        let entry = self.tables.iter_mut().find(|(p, _)| *p == place)?;
        let rows = entry.1.as_mut()?;
        match apply_rows(rows, n, columns, types) {
            Ok(()) => Some(rows.cells()),
            Err(e) => {
                warnings.push(format!(
                    "the rows of {name} are not followed after this answer: {e}"
                ));
                entry.1 = None;
                None
            }
        }
    }
}

pub(super) fn request_doc(pchs: &str) -> String {
    format!(
        "<request xmlns=\"{NS_WS}\" xmlns:uo=\"{NS_UO}\" xmlns:v8=\"{NS_CORE}\" xmlns:xs=\"{NS_XS}\" \
         xmlns:xsi=\"{NS_XSI}\" xmlns:cfg=\"{NS_CONFIG}\">{pchs}</request>"
    )
}
