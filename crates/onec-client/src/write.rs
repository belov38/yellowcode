use std::collections::BTreeSet;

use roxmltree::{Document, Node};

use crate::answer::{NS_MODULES, NS_WS, boolean};
use crate::body;
use crate::formdescr::{FieldType, FormData, FormField, FormTable, NS_UO, Slot};
use crate::metadata::{MdKind, MdType};
use crate::object::{
    Asked, Cell, ObjectData, ObjectError, form_key, implied_type, main_of, object_of, parse_ref,
    type_of_id, uo_children,
};
use crate::session::{RandomSource, Session, SessionError};
use crate::value::{Date, Decimal, EnumRef, NS_CONFIG, NS_CORE, NS_XS, Ref, TypeName, Uuid, Value};
use crate::xml::{self, escape_attr, escape_text};

const NS_XSI: &str = "http://www.w3.org/2001/XMLSchema-instance";
const NS_ENT: &str = "http://v8.1c.ru/8.1/data/enterprise";

#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    Text(String),
    Value(Value),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Assign {
    pub name: String,
    pub input: Input,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RowEdit {
    Set { line: usize, cells: Vec<Assign> },
    Delete { line: usize },
    Add { cells: Vec<Assign> },
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct TableEdit {
    pub name: String,
    pub clear: bool,
    pub rows: Vec<RowEdit>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ObjectChange {
    pub set: Vec<Assign>,
    pub tables: Vec<TableEdit>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WriteMode {
    #[default]
    Default,
    Post,
    Unpost,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    New(String),
    Existing(Ref),
}

#[derive(Debug, Clone, PartialEq)]
pub struct WriteRequest {
    pub target: Target,
    pub change: ObjectChange,
    pub mode: WriteMode,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserMessage {
    pub text: String,
    pub field: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Applied {
    pub path: String,
    pub old: Option<Cell>,
    pub new: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    DryRun {
        body: String,
    },
    Written {
        reference: Ref,
        code: Option<String>,
        data_version: Option<String>,
        presentation: String,
    },
    Refused {
        reason: Option<String>,
        text: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct WriteResult {
    pub form_id: String,
    pub title: Option<String>,
    pub document_mode: Option<&'static str>,
    pub changes: Vec<Applied>,
    pub outcome: Outcome,
    pub messages: Vec<UserMessage>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Plain,
    Document,
}

fn kind_of(r: &Ref) -> Kind {
    if r.type_name().local().starts_with("DocumentRef.") {
        Kind::Document
    } else {
        Kind::Plain
    }
}

fn zero() -> Uuid {
    Uuid::from_bytes([0; 16])
}

pub fn new_object_ref(name: &str) -> Result<Ref, String> {
    let bad = || {
        format!(
            "not an object name: {name:?} (<Вид>.<Имя> or <Kind>.<Name>, e.g. Справочник.Имя or Catalog.Name)"
        )
    };
    let local = match name.strip_prefix("cfg:") {
        Some(local) => local.to_string(),
        None => {
            let (kind, rest) = name.split_once('.').ok_or_else(bad)?;
            let kind = MdKind::of_ru(kind).unwrap_or_else(|| MdKind::of_stem(kind));
            if matches!(
                kind,
                MdKind::InformationRegister | MdKind::AccumulationRegister
            ) || matches!(&kind, MdKind::Other(k) if k.ends_with("Register") || k.starts_with("Регистр"))
            {
                return Err(format!(
                    "{name}: writing records of registers is not implemented (objects with a reference only)"
                ));
            }
            format!("{}Ref.{rest}", kind.en())
        }
    };
    parse_ref(&format!("cfg:{local}({})", zero())).map_err(|_| bad())
}

struct RawForm {
    namespaces: Vec<(String, String)>,
    fid: String,
    form_id: String,
    fover: Option<String>,
    values: Vec<String>,
    tables: Vec<Option<RawTable>>,
    prms: String,
    dd: String,
    attributes: usize,
    main_at: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct RawTable {
    pub(crate) last_id: i64,
    pub(crate) rows: Vec<RawRow>,
    pub(crate) columns: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct RawRow {
    pub(crate) id: String,
    pub(crate) cells: Vec<String>,
}

fn shape(what: impl Into<String>) -> ObjectError {
    ObjectError::Shape(what.into())
}

fn raw_table(text: &str, n: Node) -> Result<RawTable, ObjectError> {
    let num = |name: &str| -> Result<i64, ObjectError> {
        n.attribute(name)
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| shape(format!("a tabular section without a {name}")))
    };
    let count = num("count")?;
    let last_id = num("lastId")?;
    let mut rows = Vec::new();
    let mut columns = Vec::new();
    for c in uo_children(n) {
        match c.tag_name().name() {
            "item" => {
                let id = c
                    .attribute("id")
                    .filter(|v| v.parse::<i64>().is_ok())
                    .ok_or_else(|| shape("a row without its id"))?;
                rows.push(RawRow {
                    id: id.to_string(),
                    cells: uo_children(c)
                        .map(|v| text[v.range()].to_string())
                        .collect(),
                });
            }
            "column" => columns.push(text[c.range()].to_string()),
            other => return Err(shape(format!("unexpected uo:{other} in a tabular section"))),
        }
    }
    if i64::try_from(rows.len()).ok() != Some(count) {
        return Err(shape(format!("{} rows for count {count}", rows.len())));
    }
    Ok(RawTable {
        last_id,
        rows,
        columns,
    })
}

fn raw_form(body: &[u8], data: &FormData) -> Result<RawForm, ObjectError> {
    let text = xml::value_text(body);
    let doc = Document::parse(&text).map_err(|e| shape(format!("logForm/query: {e}")))?;
    let root = doc.root_element();
    let child = |local: &str| {
        root.children().find(|c| {
            c.is_element()
                && c.tag_name().namespace() == Some(NS_WS)
                && c.tag_name().name() == local
        })
    };
    let text_of = |local: &str| child(local).and_then(|n| n.text()).map(str::to_string);
    let fid = text_of("fid").ok_or_else(|| shape("no fid"))?;
    let form_id = text_of("id").ok_or_else(|| shape("no id"))?;
    let fover = text_of("fover");
    let properties: Vec<Node> = root
        .children()
        .filter(|c| {
            c.is_element()
                && c.tag_name().namespace() == Some(NS_WS)
                && c.tag_name().name() == "property"
        })
        .collect();
    let props = child("props").ok_or_else(|| shape("no props"))?;
    let main = main_of(&properties, props)?;
    let main_at = properties
        .iter()
        .position(|p| p.attribute("main").and_then(boolean) == Some(true))
        .unwrap_or(0);
    let nodes: Vec<Node> = uo_children(main).collect();
    if nodes.len() != data.layout.len() {
        return Err(shape(format!(
            "{} values for {} fields of the description",
            nodes.len(),
            data.layout.len()
        )));
    }
    let mut tables = Vec::with_capacity(nodes.len());
    for (n, slot) in nodes.iter().zip(&data.layout) {
        tables.push(match slot {
            Slot::Table(_) => Some(raw_table(&text, *n)?),
            _ => None,
        });
    }
    let prms = child("prms").ok_or_else(|| shape("no prms (the parameters of the form)"))?;
    let dd = child("dd").ok_or_else(|| shape("no dd (the window data of the form)"))?;
    let mut namespaces: Vec<(String, String)> = Vec::new();
    for n in [main, prms, dd] {
        for ns in n.namespaces() {
            let Some(prefix) = ns.name() else { continue };
            match namespaces.iter().find(|(p, _)| p == prefix) {
                Some((_, uri)) if uri != ns.uri() => {
                    return Err(shape(format!("the prefix {prefix} names two namespaces")));
                }
                Some(_) => {}
                None => namespaces.push((prefix.to_string(), ns.uri().to_string())),
            }
        }
    }
    for (p, uri) in [
        ("cfg", NS_CONFIG),
        ("uo", NS_UO),
        ("v8", NS_CORE),
        ("xs", NS_XS),
        ("xsi", NS_XSI),
    ] {
        match namespaces.iter().find(|(x, _)| x == p) {
            Some((_, u)) if u != uri => {
                return Err(shape(format!("the prefix {p} names {u}, not {uri}")));
            }
            Some(_) => {}
            None => namespaces.push((p.to_string(), uri.to_string())),
        }
    }
    Ok(RawForm {
        namespaces,
        fid,
        form_id,
        fover,
        values: nodes.iter().map(|n| text[n.range()].to_string()).collect(),
        tables,
        prms: text[prms.range()].to_string(),
        dd: text[dd.range()].to_string(),
        attributes: properties.len(),
        main_at,
    })
}

pub(crate) fn same_name(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

pub(crate) fn named(f: &FormField, name: &str) -> bool {
    f.name.as_deref().is_some_and(|n| same_name(n, name))
        || f.name_ru.as_deref().is_some_and(|n| same_name(n, name))
}

fn allowed(ty: &FieldType, types: &[MdType]) -> Vec<Option<TypeName>> {
    ty.ids.iter().map(|id| type_of_id(id, types)).collect()
}

fn same_type(a: &TypeName, b: &TypeName) -> bool {
    a.ns() == b.ns() && same_name(a.local(), b.local())
}

fn is_enum(t: &TypeName) -> bool {
    t.ns() == NS_CONFIG && t.local().starts_with("EnumRef.")
}

fn date_text(t: &str) -> Result<Date, String> {
    let full = match t.len() {
        10 => format!("{t}T00:00:00"),
        8 => format!("0001-01-01T{t}"),
        _ => t.to_string(),
    };
    Date::parse(&full).map_err(|e| e.to_string())
}

fn empty_date() -> Result<Value, String> {
    Date::new(1, 1, 1, 0, 0, 0)
        .map(Value::Date)
        .ok_or_else(|| "the empty date".to_string())
}

fn single_text(t: &str, ty: &TypeName) -> Result<Value, String> {
    let empty = t.is_empty();
    if ty.ns() == NS_XS {
        return match ty.local() {
            "string" => Ok(Value::String(t.to_string())),
            "decimal" if empty => Ok(Value::Number(Decimal::from_i64(0))),
            "decimal" => Decimal::parse(t)
                .map(Value::Number)
                .map_err(|e| e.to_string()),
            "boolean" => match t {
                "" | "false" => Ok(Value::Bool(false)),
                "true" => Ok(Value::Bool(true)),
                _ => Err(format!("not a boolean: {t:?} (true or false)")),
            },
            "dateTime" if empty => empty_date(),
            "dateTime" => date_text(t).map(Value::Date),
            other => Err(format!(
                "a value of xs:{other} is not written by the client"
            )),
        };
    }
    if ty.ns() == NS_CORE && ty.local() == "UUID" {
        return if empty {
            Ok(Value::Uuid(zero()))
        } else {
            Uuid::parse(t).map(Value::Uuid).map_err(|e| e.to_string())
        };
    }
    if ty.ns() == NS_CORE {
        return Err(format!(
            "a value of v8:{} is not read from a text",
            ty.local()
        ));
    }
    if is_enum(ty) {
        return EnumRef::new(ty.clone(), t)
            .map(Value::EnumRef)
            .map_err(|e| e.to_string());
    }
    let uuid = if empty {
        zero()
    } else if let Ok(u) = Uuid::parse(t) {
        u
    } else {
        let r = parse_ref(t)?;
        if !same_type(r.type_name(), ty) {
            return Err(format!("a reference of {}, not of {ty}", r.type_name()));
        }
        r.uuid()
    };
    Ref::new(ty.clone(), uuid)
        .map(Value::Ref)
        .map_err(|e| e.to_string())
}

fn prefixed_text(t: &str) -> Result<Value, String> {
    if t == "undef" {
        return Ok(Value::Undefined);
    }
    match t.split_once(':') {
        Some(("s", v)) => Ok(Value::String(v.to_string())),
        Some(("n", v)) => Decimal::parse(v)
            .map(Value::Number)
            .map_err(|e| e.to_string()),
        Some(("b", "true")) => Ok(Value::Bool(true)),
        Some(("b", "false")) => Ok(Value::Bool(false)),
        Some(("b", v)) => Err(format!("not a boolean: {v:?} (true or false)")),
        Some(("d", v)) => date_text(v).map(Value::Date),
        Some(("u", v)) => Uuid::parse(v).map(Value::Uuid).map_err(|e| e.to_string()),
        _ if t.ends_with(')') && t.contains('(') => parse_ref(t).map(Value::Ref),
        _ => Err(format!(
            "{t:?}: a field of several types takes a typed value (s:…, n:…, d:…, b:…, u:…, undef or \
             <Вид>.<Имя>(<uuid>))"
        )),
    }
}

fn of_type(v: &Value, ty: &TypeName) -> bool {
    v.type_name().is_some_and(|t| same_type(&t, ty))
}

const PLATFORM_ENUMS: &[(&str, &[(&str, &str)])] = &[(
    "872f7198-7083-4e3e-b57e-a2a9802c769e",
    &[
        ("Active", "Активный"),
        ("Passive", "Пассивный"),
        ("ActivePassive", "АктивноПассивный"),
    ],
)];

fn platform_enum_value(
    input: &Input,
    field: &FormField,
) -> Option<Result<(Value, String), String>> {
    let [id] = field.ty.ids.as_slice() else {
        return None;
    };
    let (_, names) = PLATFORM_ENUMS
        .iter()
        .find(|(t, _)| t.eq_ignore_ascii_case(id))?;
    let Input::Text(t) = input else {
        return Some(Err(
            "the value of a platform enumeration is given by its name".to_string(),
        ));
    };
    Some(
        names
            .iter()
            .find(|(en, ru)| same_name(en, t) || same_name(ru, t))
            .map(|(en, _)| {
                (
                    Value::String((*en).to_string()),
                    format!("<uo:v>{en}</uo:v>"),
                )
            })
            .ok_or_else(|| {
                let all: Vec<&str> = names.iter().map(|(en, _)| *en).collect();
                format!(
                    "{t:?} is not a value of the enumeration ({})",
                    all.join(", ")
                )
            }),
    )
}

pub(crate) fn field_value(
    input: &Input,
    field: &FormField,
    types: &[MdType],
) -> Result<Value, String> {
    let single = implied_type(&field.ty, types);
    let all = allowed(&field.ty, types);
    if single.is_none() && all.len() == 1 {
        return Err("a value of this type is not written by the client".to_string());
    }
    let v = match (input, &single) {
        (Input::Text(t), Some(ty)) => single_text(t, ty)?,
        (Input::Text(t), None) => prefixed_text(t)?,
        (Input::Value(v), _) => v.clone(),
    };
    match &single {
        Some(ty) if !of_type(&v, ty) => {
            return Err(format!("a value of {}, not of {ty}", type_text(&v)));
        }
        Some(_) => {}
        None if matches!(v, Value::Undefined) => {}
        None if all.is_empty() => {}
        None if !all.iter().flatten().any(|ty| of_type(&v, ty)) => {
            return Err(format!(
                "a value of {}, not one of the types of the field",
                type_text(&v)
            ));
        }
        None => {}
    }
    if !v.kind().is_primitive() {
        return Err(format!(
            "a value of {} is not written into a field",
            type_text(&v)
        ));
    }
    qualifiers(&v, &field.ty)?;
    Ok(v)
}

fn type_text(v: &Value) -> String {
    v.type_name()
        .map_or_else(|| "Неопределено".to_string(), |t| t.to_string())
}

fn qualifiers(v: &Value, ty: &FieldType) -> Result<(), String> {
    match v {
        Value::String(s) => {
            if let Some(n) = ty.length.filter(|n| *n > 0) {
                let len = s.chars().count();
                if len > n as usize {
                    return Err(format!("{len} characters for a string of length {n}"));
                }
            }
        }
        Value::Number(d) => {
            let t = d.as_str();
            let negative = t.starts_with('-');
            if negative && ty.nonnegative {
                return Err(format!("{t}: the field takes a nonnegative number"));
            }
            let body = t.trim_start_matches('-');
            let (int, frac) = body.split_once('.').unwrap_or((body, ""));
            let int_digits = if int == "0" { 0 } else { int.len() };
            let fraction = ty.fraction.unwrap_or(0) as usize;
            if ty.digits.is_some_and(|d| d > 0) && frac.len() > fraction {
                return Err(format!(
                    "{t}: the field takes {fraction} digits after the point"
                ));
            }
            if let Some(digits) = ty.digits.filter(|d| *d > 0) {
                let whole = (digits as usize).saturating_sub(fraction);
                if int_digits > whole {
                    return Err(format!(
                        "{t}: the field takes {whole} digits before the point"
                    ));
                }
            }
        }
        Value::Date(d) => {
            let (y, mo, day, h, mi, s) = d.parts();
            match ty.date_fraction.as_deref() {
                Some("Date") if (h, mi, s) != (0, 0, 0) => {
                    return Err(format!("{d}: the field takes a date without a time"));
                }
                Some("Time") if (y, mo, day) != (1, 1, 1) => {
                    return Err(format!(
                        "{d}: the field takes a time without a date (0001-01-01)"
                    ));
                }
                _ => {}
            }
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn value_xml(v: &Value, typed: bool) -> Result<String, ObjectError> {
    let text = match v.lexical() {
        Some(t) => escape_text(&t),
        None if typed && matches!(v, Value::Undefined) => {
            return Ok("<uo:v xsi:nil=\"true\"/>".to_string());
        }
        None => {
            return Err(ObjectError::Change(format!(
                "a value of {} has no text to write",
                type_text(v)
            )));
        }
    };
    if !typed {
        return Ok(if text.is_empty() {
            "<uo:v/>".to_string()
        } else {
            format!("<uo:v>{text}</uo:v>")
        });
    }
    let ty = v
        .type_name()
        .filter(|t| t.known_prefix().is_some())
        .ok_or_else(|| {
            ObjectError::Change(format!("a value of {} is not written", type_text(v)))
        })?;
    let ty = escape_attr(&ty.to_string());
    Ok(if text.is_empty() {
        format!("<uo:v xsi:type=\"{ty}\"/>")
    } else {
        format!("<uo:v xsi:type=\"{ty}\">{text}</uo:v>")
    })
}

fn empty_xml(field: &FormField, types: &[MdType]) -> Result<String, ObjectError> {
    match implied_type(&field.ty, types) {
        Some(ty) => {
            let v = single_text("", &ty).map_err(ObjectError::Change)?;
            value_xml(&v, false)
        }
        None => Ok("<uo:v xsi:nil=\"true\"/>".to_string()),
    }
}

pub(crate) fn change_error(path: &str, e: impl std::fmt::Display) -> ObjectError {
    ObjectError::Change(format!("{path}: {e}"))
}

pub(crate) fn settable(f: &FormField, path: &str) -> Result<(), ObjectError> {
    if f.read_only || f.no_edit {
        return Err(change_error(path, "the field is read only"));
    }
    Ok(())
}

fn is_line_number(c: &FormField) -> bool {
    c.name.as_deref() == Some("LineNumber") && c.id.starts_with('-')
}

pub(crate) fn table_xml(
    raw: &RawTable,
    table: &FormTable,
    read: Option<&crate::object::ObjectTable>,
    edit: Option<&TableEdit>,
    types: &[MdType],
) -> Result<(String, usize, Vec<Applied>), ObjectError> {
    struct Row {
        orig: Option<usize>,
        id: String,
        cells: Vec<String>,
        changed: bool,
        added: bool,
    }
    let name = table.field.display_name();
    let mut rows: Vec<Row> = raw
        .rows
        .iter()
        .enumerate()
        .map(|(i, r)| Row {
            orig: Some(i),
            id: r.id.clone(),
            cells: r.cells.clone(),
            changed: false,
            added: false,
        })
        .collect();
    if rows.iter().any(|r| r.cells.len() != table.columns.len()) {
        return Err(shape(format!(
            "tabular section {name}: a row of another width"
        )));
    }
    let mut ops = Vec::new();
    let mut applied = Vec::new();
    let mut last_id = raw.last_id;
    let mut removed = false;
    let old_cell = |line: usize, col: usize| -> Option<Cell> {
        read.and_then(|t| t.rows.get(line - 1))
            .and_then(|r| r.get(col))
            .cloned()
    };
    let column = |cell: &Assign, path: &str| -> Result<(usize, Value), ObjectError> {
        let (ci, col) = table
            .columns
            .iter()
            .enumerate()
            .find(|(_, c)| named(c, &cell.name))
            .ok_or_else(|| change_error(path, format!("no column {:?}", cell.name)))?;
        if is_line_number(col) {
            return Err(change_error(
                path,
                "the line number is given by the order of the rows",
            ));
        }
        settable(col, path)?;
        let v = field_value(&cell.input, col, types).map_err(|e| change_error(path, e))?;
        Ok((ci, v))
    };
    if let Some(edit) = edit {
        if edit.clear {
            ops.push("<uo:clear/>".to_string());
            rows.clear();
            removed = true;
            applied.push(Applied {
                path: name.to_string(),
                old: None,
                new: None,
            });
        }
        let count = raw.rows.len();
        let mut deleted = BTreeSet::new();
        let mut set = BTreeSet::new();
        for r in &edit.rows {
            let (line, del) = match r {
                RowEdit::Delete { line } => (*line, true),
                RowEdit::Set { line, .. } => (*line, false),
                RowEdit::Add { .. } => continue,
            };
            let path = format!("{name}[{line}]");
            if edit.clear {
                return Err(change_error(&path, "the tabular section is cleared"));
            }
            if line == 0 || line > count {
                return Err(change_error(&path, format!("no such row ({count} rows)")));
            }
            let fresh = if del {
                deleted.insert(line)
            } else {
                set.insert(line)
            };
            if !fresh || deleted.contains(&line) && set.contains(&line) {
                return Err(change_error(&path, "the row is changed twice"));
            }
        }
        for line in deleted.iter().rev() {
            let pos = rows
                .iter()
                .position(|r| r.orig == Some(line - 1))
                .ok_or_else(|| change_error(&format!("{name}[{line}]"), "no such row"))?;
            ops.push(format!("<uo:remove index=\"{pos}\"/>"));
            rows.remove(pos);
            removed = true;
            applied.push(Applied {
                path: format!("{name}[{line}]"),
                old: None,
                new: None,
            });
        }
        for r in &edit.rows {
            let RowEdit::Set { line, cells } = r else {
                continue;
            };
            let row = rows
                .iter_mut()
                .find(|x| x.orig == Some(line - 1))
                .ok_or_else(|| change_error(&format!("{name}[{line}]"), "no such row"))?;
            let mut seen_cells = BTreeSet::new();
            for cell in cells {
                let path = format!("{name}[{line}].{}", cell.name);
                let (ci, v) = column(cell, &path)?;
                if !seen_cells.insert(ci) {
                    return Err(change_error(&path, "the cell is set twice"));
                }
                row.cells[ci] =
                    value_xml(&v, implied_type(&table.columns[ci].ty, types).is_none())?;
                row.changed = true;
                applied.push(Applied {
                    path,
                    old: old_cell(*line, ci),
                    new: Some(v),
                });
            }
        }
        let mut added = 0;
        for r in &edit.rows {
            let RowEdit::Add { cells } = r else { continue };
            added += 1;
            let mut values: Vec<String> = table
                .columns
                .iter()
                .map(|c| empty_xml(c, types))
                .collect::<Result<_, _>>()?;
            let mut seen_cells = BTreeSet::new();
            for cell in cells {
                let path = format!("{name}[+{added}].{}", cell.name);
                let (ci, v) = column(cell, &path)?;
                if !seen_cells.insert(ci) {
                    return Err(change_error(&path, "the cell is set twice"));
                }
                values[ci] = value_xml(&v, implied_type(&table.columns[ci].ty, types).is_none())?;
                applied.push(Applied {
                    path,
                    old: None,
                    new: Some(v),
                });
            }
            if cells.is_empty() {
                applied.push(Applied {
                    path: format!("{name}[+{added}]"),
                    old: None,
                    new: None,
                });
            }
            last_id += 1;
            ops.push(format!(
                "<uo:add index=\"{}\" id=\"{last_id}\"/>",
                rows.len()
            ));
            rows.push(Row {
                orig: None,
                id: last_id.to_string(),
                cells: values,
                changed: false,
                added: true,
            });
        }
    }
    let full = removed || rows.iter().any(|r| r.changed);
    let first_added = if full {
        -1
    } else {
        rows.iter()
            .position(|r| r.added)
            .and_then(|p| i64::try_from(p).ok())
            .unwrap_or(-1)
    };
    let line_col = table.columns.iter().position(is_line_number);
    let mut items = String::new();
    for (i, r) in rows.iter_mut().enumerate() {
        if !(r.changed || r.added) {
            continue;
        }
        if let Some(lc) = line_col {
            r.cells[lc] = format!("<uo:v>{}</uo:v>", i + 1);
        }
        items.push_str(&format!(
            "<uo:item index=\"{i}\" id=\"{}\"><uo:structure>{}</uo:structure></uo:item>",
            r.id,
            r.cells.concat()
        ));
    }
    let mut x = format!(
        "<uo:v count=\"{}\" lastId=\"{last_id}\" fullChanged=\"{full}\" firstAddedLine=\"{first_added}\">",
        rows.len()
    );
    if !ops.is_empty() {
        x.push_str(&format!("<uo:operations>{}</uo:operations>", ops.concat()));
    }
    if !items.is_empty() {
        x.push_str(&format!("<uo:items>{items}</uo:items>"));
    }
    let cells: Vec<&[String]> = rows.iter().map(|r| r.cells.as_slice()).collect();
    x.push_str(&column_totals(&raw.columns, table, &cells));
    x.push_str("</uo:v>");
    Ok((x, ops.len(), applied))
}

fn number_of(cell: &str) -> Option<&str> {
    let inner = cell.strip_prefix("<uo:v")?;
    if inner.ends_with("/>") {
        return Some("0");
    }
    let start = inner.find('>')? + 1;
    let text = inner[start..].strip_suffix("</uo:v>")?;
    Decimal::parse(text).ok().map(|_| text)
}

fn decimal_sum<'a>(texts: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let texts: Vec<&str> = texts.into_iter().collect();
    let scale = texts
        .iter()
        .map(|t| t.split_once('.').map_or(0, |(_, f)| f.len()))
        .max()
        .unwrap_or(0);
    let mut sum: i128 = 0;
    for t in &texts {
        let (negative, body) = match t.strip_prefix('-') {
            Some(b) => (true, b),
            None => (false, *t),
        };
        let (int, frac) = body.split_once('.').unwrap_or((body, ""));
        let digits = format!("{int}{frac}{}", "0".repeat(scale - frac.len()));
        let v: i128 = digits.parse().ok()?;
        sum = if negative {
            sum.checked_sub(v)?
        } else {
            sum.checked_add(v)?
        };
    }
    let negative = sum < 0;
    let digits = sum.unsigned_abs().to_string();
    let digits = format!(
        "{}{digits}",
        "0".repeat((scale + 1).saturating_sub(digits.len()))
    );
    let (int, frac) = digits.split_at(digits.len() - scale);
    let frac = frac.trim_end_matches('0');
    let text = if frac.is_empty() {
        int.to_string()
    } else {
        format!("{int}.{frac}")
    };
    Some(if negative && text != "0" {
        format!("-{text}")
    } else {
        text
    })
}

const NUMBER: &str = "b0be78f2-0ee6-4d31-a3bb-77dd32ba5bec";

fn column_id(raw: &str) -> Option<&str> {
    raw.split_once("id=\"")
        .and_then(|(_, r)| r.split_once('"'))
        .map(|(id, _)| id)
}

fn column_totals(given: &[String], table: &FormTable, rows: &[&[String]]) -> String {
    let mut out = String::new();
    for (ci, c) in table.columns.iter().enumerate() {
        if !matches!(c.ty.ids.as_slice(), [one] if one.eq_ignore_ascii_case(NUMBER)) {
            continue;
        }
        let id = xml::escape_attr(&c.id);
        let old = given.iter().find(|g| column_id(g) == Some(id.as_str()));
        let service = matches!(c.name.as_deref(), Some("LineNumber" | "SourceLineNumber"));
        let sum = if service {
            None
        } else {
            rows.iter()
                .map(|r| r.get(ci).and_then(|v| number_of(v)))
                .collect::<Option<Vec<&str>>>()
                .and_then(decimal_sum)
        };
        match (sum, old) {
            (Some(total), _) => {
                out.push_str(&format!("<uo:column id=\"{id}\" total=\"{total}\"/>"))
            }
            (None, Some(old)) => out.push_str(old),
            (None, None) => out.push_str(&format!("<uo:column id=\"{id}\" total=\"0\"/>")),
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Plan {
    pub(crate) body: String,
    pub(crate) changes: Vec<Applied>,
    pub(crate) document_mode: Option<&'static str>,
}

fn params_xml(mode: Option<&str>) -> String {
    match mode {
        None => {
            "<m:param xsi:type=\"v8:Structure\"/><m:param xsi:type=\"xs:boolean\">false</m:param>\
                 <m:param xsi:type=\"xs:string\"/>"
                .to_string()
        }
        Some(m) => format!(
            "<m:param xsi:type=\"v8:Structure\"><v8:Property name=\"РежимЗаписи\"><v8:Value \
             xmlns:d5p1=\"{NS_ENT}\" xsi:type=\"d5p1:DocumentWriteMode\">{m}</v8:Value></v8:Property></m:param>\
             <m:param xsi:type=\"xs:boolean\">{}</m:param><m:param xsi:type=\"xs:boolean\">false</m:param>\
             <m:param xsi:type=\"xs:string\"/>",
            m == "Posting"
        ),
    }
}

fn field_slot(data: &FormData, name: &str) -> Option<usize> {
    data.layout.iter().position(|s| match s {
        Slot::Field(i) => data.fields[*i].name.as_deref() == Some(name),
        _ => false,
    })
}

fn document_mode(
    kind: Kind,
    mode: WriteMode,
    object: &ObjectData,
    marked: bool,
) -> Result<Option<&'static str>, ObjectError> {
    let posted = object.fields.iter().any(|f| {
        f.field.name.as_deref() == Some("Posted") && f.value == Cell::Value(Value::Bool(true))
    });
    Ok(match (kind, mode) {
        (Kind::Plain, WriteMode::Default) => None,
        (Kind::Plain, _) => {
            return Err(ObjectError::Change(
                "only a document is posted or unposted".to_string(),
            ));
        }
        (Kind::Document, WriteMode::Post) if marked => {
            return Err(ObjectError::Change(
                "a document marked for deletion is not posted: --post with the deletion mark"
                    .to_string(),
            ));
        }
        (Kind::Document, WriteMode::Post) => Some("Posting"),
        (Kind::Document, WriteMode::Unpost) => Some("UndoPosting"),
        (Kind::Document, WriteMode::Default) if posted && marked => Some("UndoPosting"),
        (Kind::Document, WriteMode::Default) if posted => Some("Posting"),
        (Kind::Document, WriteMode::Default) => Some("Write"),
    })
}

#[allow(clippy::too_many_arguments)]
fn plan(
    raw: &RawForm,
    data: &FormData,
    object: &ObjectData,
    types: &[MdType],
    change: &ObjectChange,
    new: bool,
    kind: Kind,
    mode: WriteMode,
) -> Result<Plan, ObjectError> {
    let mut values: Vec<Option<String>> = vec![None; data.layout.len()];
    let mut changes = Vec::new();
    let mut marked = false;
    for a in &change.set {
        let at = data
            .layout
            .iter()
            .position(|s| matches!(s, Slot::Field(i) if named(&data.fields[*i], &a.name)))
            .ok_or_else(|| {
                if data.tables.iter().any(|t| named(&t.field, &a.name)) {
                    change_error(&a.name, "a tabular section is changed by its rows")
                } else {
                    change_error(&a.name, "no such field")
                }
            })?;
        let Slot::Field(fi) = data.layout[at] else {
            return Err(change_error(&a.name, "no such field"));
        };
        let f = &data.fields[fi];
        settable(f, &a.name)?;
        if values[at].is_some() {
            return Err(change_error(&a.name, "the field is set twice"));
        }
        let (v, x) = match platform_enum_value(&a.input, f) {
            Some(r) => r.map_err(|e| change_error(&a.name, e))?,
            None => {
                let v = field_value(&a.input, f, types).map_err(|e| change_error(&a.name, e))?;
                let x = value_xml(&v, implied_type(&f.ty, types).is_none())?;
                (v, x)
            }
        };
        marked |= f.name.as_deref() == Some("DeletionMark") && v == Value::Bool(true);
        values[at] = Some(x);
        changes.push(Applied {
            path: f.display_name().to_string(),
            old: if new {
                None
            } else {
                object
                    .fields
                    .iter()
                    .find(|x| x.field.id == f.id)
                    .map(|x| x.value.clone())
            },
            new: Some(v),
        });
    }
    let mut ops = 0;
    let mut seen = BTreeSet::new();
    for t in &change.tables {
        let at = data
            .layout
            .iter()
            .position(|s| matches!(s, Slot::Table(i) if named(&data.tables[*i].field, &t.name)))
            .ok_or_else(|| change_error(&t.name, "no such tabular section"))?;
        if !seen.insert(at) {
            return Err(change_error(&t.name, "the tabular section is given twice"));
        }
        let Slot::Table(ti) = data.layout[at] else {
            return Err(change_error(&t.name, "no such tabular section"));
        };
        let table = &data.tables[ti];
        settable(&table.field, &t.name)?;
        let raw_t = raw.tables[at]
            .as_ref()
            .ok_or_else(|| shape("no rows of a tabular section"))?;
        let read = object.tables.iter().find(|x| x.field.id == table.field.id);
        let (x, n, applied) = table_xml(raw_t, table, read, Some(t), types)?;
        ops += n;
        values[at] = Some(x);
        changes.extend(applied);
    }
    let document_mode = document_mode(kind, mode, object, marked)?;
    if changes.is_empty() && !new && mode == WriteMode::Default {
        return Err(ObjectError::Change(
            "nothing to change (a document is posted or unposted alone only with --post or --unpost)".to_string(),
        ));
    }
    let version = field_slot(data, "DataVersion");
    let fields = if new {
        let mut all = Vec::with_capacity(values.len());
        for (i, v) in values.iter().enumerate() {
            all.push(match (v, &data.layout[i]) {
                (Some(x), _) => x.clone(),
                (None, Slot::Table(ti)) => {
                    let raw_t = raw.tables[i].as_ref().ok_or_else(|| shape("no rows"))?;
                    table_xml(raw_t, &data.tables[*ti], None, None, types)?.0
                }
                (None, _) => raw.values[i].clone(),
            });
        }
        format!("<uo:v>{}</uo:v>", all.concat())
    } else {
        let version = version.ok_or_else(|| {
            ObjectError::Change(
                "the form has no DataVersion: a write without the check of the version is not done"
                    .to_string(),
            )
        })?;
        let shown = object
            .fields
            .iter()
            .find(|f| f.field.name.as_deref() == Some("DataVersion"))
            .map(|f| &f.value);
        if !matches!(shown, Some(Cell::Value(Value::String(t))) if !t.trim().is_empty()) {
            return Err(ObjectError::Change(
                "the form shows an empty DataVersion: a write without the check of the version is not done"
                    .to_string(),
            ));
        }
        let mut at: Vec<usize> = (0..values.len()).filter(|i| values[*i].is_some()).collect();
        at.push(version);
        at.sort_unstable();
        at.dedup();
        let body: String = at
            .iter()
            .map(|i| values[*i].clone().unwrap_or_else(|| raw.values[*i].clone()))
            .collect();
        let list: Vec<String> = at.iter().map(usize::to_string).collect();
        format!("<uo:v fields=\"{}\">{body}</uo:v>", list.join(" "))
    };
    let ns: String = raw
        .namespaces
        .iter()
        .map(|(p, u)| format!(" xmlns:{p}=\"{}\"", escape_attr(u)))
        .collect();
    let root = if raw.attributes > 1 {
        format!(" fields=\"{}\"", raw.main_at)
    } else {
        String::new()
    };
    let fover = raw.fover.as_ref().map_or(String::new(), |f| {
        format!("<fover>{}</fover>", escape_text(f))
    });
    let body = format!(
        "\u{feff}<?xml version=\"1.0\" encoding=\"UTF-8\"?><request xmlns=\"{NS_WS}\"{ns}><rKey>{}</rKey>\
         <id>{}</id><fid>{}</fid>{fover}<prm xmlns:m=\"{NS_MODULES}\" name=\"controller::write\">{}</prm>\
         <pchs sinDesc=\"0\" sin=\"{}\" seq=\"{}\"><uo:structure{root}>{fields}</uo:structure></pchs>{}{}</request>",
        escape_text(&raw.fid.to_uppercase()),
        escape_text(&raw.form_id),
        escape_text(&raw.fid),
        params_xml(document_mode),
        usize::from(ops > 0),
        ops,
        raw.prms,
        raw.dd,
    );
    Ok(Plan {
        body,
        changes,
        document_mode,
    })
}

#[derive(Debug, Clone, PartialEq)]
enum Answer {
    Done {
        ret: bool,
        presentation: String,
        values: Vec<(usize, String)>,
    },
    Exception {
        reason: Option<String>,
        text: String,
    },
}

fn child<'a, 'i>(n: Node<'a, 'i>, ns: &str, local: &str) -> Vec<Node<'a, 'i>> {
    n.children()
        .filter(|c| {
            c.is_element() && c.tag_name().namespace() == Some(ns) && c.tag_name().name() == local
        })
        .collect()
}

fn exception_text(root: Node) -> String {
    let mut out: Vec<String> = Vec::new();
    for n in root.descendants().filter(|n| n.is_element()) {
        if !matches!(n.tag_name().name(), "descr" | "string0") {
            continue;
        }
        let t = n.text().unwrap_or("").trim();
        if !t.is_empty() && !out.iter().any(|x| x == t) {
            out.push(t.to_string());
        }
    }
    out.join("; ")
}

fn read_answer(status: u16, body: &[u8]) -> Result<Answer, ObjectError> {
    let text = xml::value_text(body);
    let bad = |what: String| ObjectError::WriteAnswer(what);
    let doc = Document::parse(&text).map_err(|e| bad(format!("HTTP {status}: {e}")))?;
    let root = doc.root_element();
    if root.tag_name().name() == "exception" {
        return Ok(Answer::Exception {
            reason: root.attribute("reason").map(str::to_string),
            text: exception_text(root),
        });
    }
    if status != 200
        || root.tag_name().namespace() != Some(NS_WS)
        || root.tag_name().name() != "request"
    {
        return Err(bad(format!(
            "HTTP {status}: <{}> is not the answer of a write",
            root.tag_name().name()
        )));
    }
    let [prm] = child(root, NS_WS, "prm")[..] else {
        return Err(bad("not one prm".to_string()));
    };
    if prm.attribute("name") != Some("controller::write") {
        return Err(bad("prm is not controller::write".to_string()));
    }
    let [ret] = child(prm, NS_MODULES, "ret")[..] else {
        return Err(bad("not one ret".to_string()));
    };
    let ret = ret
        .text()
        .and_then(boolean)
        .ok_or_else(|| bad("ret is not a boolean".to_string()))?;
    let presentation = child(prm, NS_MODULES, "param")
        .last()
        .and_then(|p| p.text())
        .unwrap_or("")
        .to_string();
    let mut values = Vec::new();
    if let Some(v) = child(root, NS_WS, "pchs")
        .first()
        .and_then(|p| child(*p, NS_UO, "structure").first().copied())
        .and_then(|s| child(s, NS_UO, "v").first().copied())
    {
        let nodes = child(v, NS_UO, "v");
        let slots: Vec<usize> = match v.attribute("fields") {
            Some(f) => f
                .split_whitespace()
                .map(|x| x.parse().map_err(|_| bad(format!("bad fields {f:?}"))))
                .collect::<Result<_, _>>()?,
            None => (0..nodes.len()).collect(),
        };
        if slots.len() != nodes.len() {
            return Err(bad(format!(
                "{} values for {} fields",
                nodes.len(),
                slots.len()
            )));
        }
        for (slot, n) in slots.into_iter().zip(nodes) {
            values.push((slot, n.text().unwrap_or("").to_string()));
        }
    }
    Ok(Answer::Done {
        ret,
        presentation,
        values,
    })
}

fn messages_of(body: &[u8]) -> Result<Vec<UserMessage>, ObjectError> {
    let text = xml::value_text(body);
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let doc =
        Document::parse(&text).map_err(|e| ObjectError::WriteAnswer(format!("messages: {e}")))?;
    let mut out = Vec::new();
    for m in doc
        .root_element()
        .children()
        .filter(|c| c.is_element() && c.tag_name().name() == "message")
    {
        let text = m
            .children()
            .find(|c| {
                c.is_element()
                    && c.tag_name().name() == "Text"
                    && c.tag_name().namespace() == Some(NS_ENT)
            })
            .and_then(|c| c.text())
            .unwrap_or("")
            .to_string();
        let field = m
            .attribute("DataPath")
            .filter(|p| !p.is_empty())
            .map(str::to_string);
        out.push(UserMessage { text, field });
    }
    Ok(out)
}

fn outcome_of(answer: Answer, data: &FormData, r: &Ref) -> Result<Outcome, ObjectError> {
    let (ret, presentation, values) = match answer {
        Answer::Exception { reason, text } => return Ok(Outcome::Refused { reason, text }),
        Answer::Done {
            ret,
            presentation,
            values,
        } => (ret, presentation, values),
    };
    if !ret {
        return Ok(Outcome::Refused {
            reason: None,
            text: "the server did not write the object (see the messages)".to_string(),
        });
    }
    let at = |name: &str| {
        field_slot(data, name)
            .and_then(|s| values.iter().find(|(x, _)| *x == s).map(|(_, v)| v.clone()))
    };
    let uuid = match at("Ref") {
        Some(u) => Uuid::parse(&u).map_err(|_| ObjectError::WriteAnswer(format!("Ref {u:?}")))?,
        None if !r.is_empty() => r.uuid(),
        None => {
            return Err(ObjectError::WriteAnswer(
                "no Ref after the write".to_string(),
            ));
        }
    };
    if uuid == zero() {
        return Err(ObjectError::WriteAnswer(
            "an empty Ref after the write".to_string(),
        ));
    }
    let reference = Ref::new(r.type_name().clone(), uuid)
        .map_err(|e| ObjectError::WriteAnswer(e.to_string()))?;
    Ok(Outcome::Written {
        reference,
        code: at("Code")
            .or_else(|| at("Number"))
            .map(|c| c.trim_end().to_string()),
        data_version: at("DataVersion"),
        presentation,
    })
}

impl<R: RandomSource> Session<R> {
    pub fn write_object(&mut self, w: &WriteRequest) -> Result<WriteResult, SessionError> {
        let (r, new) = match &w.target {
            Target::New(name) => (new_object_ref(name).map_err(ObjectError::Change)?, true),
            Target::Existing(r) => (r.clone(), false),
        };
        let kind = kind_of(&r);
        if kind == Kind::Plain && w.mode != WriteMode::Default {
            return Err(
                ObjectError::Change("only a document is posted or unposted".to_string()).into(),
            );
        }
        let conf = self.prologue(true)?.conf;
        let types = self.session_types("the types of the values of the object")?;
        let key = form_key(&r)?;
        let (fid, answer) = self.open_form(&key, (!new).then_some(&r))?;
        let work = self.write_in_form(&conf, &answer, &types, &r, new, kind, w);
        match work {
            Ok(mut result) => {
                if let Err(e) = self.close_form(&fid) {
                    result
                        .warnings
                        .push(format!("the form was not closed: {e}"));
                }
                Ok(result)
            }
            Err(SessionError::Object(ObjectError::WriteSent(m))) => {
                let m = match self.close_form(&fid) {
                    Ok(()) => m,
                    Err(c) => format!("{m}; the form was not closed either: {c}"),
                };
                Err(ObjectError::WriteSent(m).into())
            }
            Err(e) => self.closing(&fid, Err(e)),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn write_in_form(
        &mut self,
        conf: &str,
        answer: &[u8],
        types: &[MdType],
        r: &Ref,
        new: bool,
        kind: Kind,
        w: &WriteRequest,
    ) -> Result<WriteResult, SessionError> {
        let (data, mut unrecognized) = self.describe_form(conf, answer)?;
        let asked = if new {
            Asked::New(r)
        } else {
            Asked::Existing(r)
        };
        let object = object_of(answer, &data, types, asked, &mut unrecognized)?;
        if object.read_only {
            return Err(ObjectError::Change(
                "the form is read only for the user: no right to change the object".to_string(),
            )
            .into());
        }
        let raw = raw_form(answer, &data)?;
        let p = plan(&raw, &data, &object, types, &w.change, new, kind, w.mode)?;
        let mut result = WriteResult {
            form_id: object.form_id.clone(),
            title: object.title.clone(),
            document_mode: p.document_mode,
            changes: p.changes,
            outcome: Outcome::DryRun {
                body: p.body.clone(),
            },
            messages: Vec::new(),
            warnings: Vec::new(),
        };
        if w.dry_run {
            return Ok(result);
        }
        let req = body::zstd_compress(p.body.as_bytes());
        let h = self.headers("application/xml", true, true);
        let sent = |e: String| SessionError::from(ObjectError::WriteSent(e));
        let resp = self
            .step_once(
                "logForm/call",
                "POST",
                "/e1cib/logForm?cmd=call",
                &h,
                &req,
                &[200, 400, 500],
            )
            .map_err(|e| sent(e.to_string()))?;
        let status = resp.status;
        if ![200, 400, 500].contains(&status) {
            return Err(sent(format!("HTTP {status}")));
        }
        let decoded = Self::decoded(&resp).map_err(|e| sent(e.to_string()))?;
        let answer = read_answer(status, &decoded).map_err(|e| sent(e.to_string()))?;
        let asked_messages = match &answer {
            Answer::Done { .. } => true,
            Answer::Exception { reason, .. } => reason.as_deref() == Some("-52"),
        };
        result.outcome = outcome_of(answer, &data, r).map_err(|e| sent(e.to_string()))?;
        if asked_messages {
            match self.user_messages() {
                Ok(m) => result.messages = m,
                Err(e) => result
                    .warnings
                    .push(format!("the messages to the user were not read: {e}")),
            }
        }
        Ok(result)
    }

    pub(crate) fn user_messages(&mut self) -> Result<Vec<UserMessage>, SessionError> {
        let mut h = self.headers("application/xml", true, false);
        h.push((
            "Content-Type".into(),
            "application/x-www-form-urlencoded".into(),
        ));
        let resp = self.step("messages", "POST", "/e1cib/messages", &h, &[], &[200, 204])?;
        Ok(messages_of(&Self::decoded(&resp)?)?)
    }
}
