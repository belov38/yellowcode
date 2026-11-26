use std::collections::HashMap;

use base64::Engine;
use brace::binary::{Atom, BinNode};
use roxmltree::{Document, Node};

use crate::answer::{AnswerError, NS_WS, attr, boolean, is, unsigned};
use crate::enums::EnumPresentations;
use crate::metadata::{MdKind, MdType};
use crate::refs::{Presentation, Presentations};
use crate::session::{RandomSource, Session, SessionError};
use crate::value::{Date, Decimal, EnumRef, Ref, TypeName, UUID_REF_KINDS, Uuid, Value, fold_key};
use crate::xml::{escape_attr, escape_text};

const NS_UO: &str = "http://v8.1c.ru/8.2/uobjects";
const NS_DL: &str = "http://v8.1c.ru/8.2/managed-application/dynamic-list";
use crate::answer::NS_LF;
const NS_XSI: &str = "http://www.w3.org/2001/XMLSchema-instance";

pub const PAGE: u32 = 22;
const CHANGES_STEP: u64 = 10;
const MAX_PAGE: u32 = 1000;
const TICKS_PER_SECOND: i128 = 10_000;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ListError {
    #[error("not a filter (expected <field><op><value>, op one of = <> < <= > >= ~ !~): {0:?}")]
    Filter(String),
    #[error("not an order (expected <field> [asc|desc]): {0:?}")]
    Order(String),
    #[error("the list has no field {0:?} (fields: {1})")]
    NoField(String, String),
    #[error("field {field}: {reason}")]
    Value { field: String, reason: String },
    #[error("not a list object (expected <Вид>.<Имя>, e.g. Справочник.Номенклатура): {0:?}")]
    Object(String),
    #[error(
        "the list has more than {0} rows: read it by pages (--page, --page-size) or give a larger --limit"
    )]
    Limit(usize),
    #[error("the page ends at row {end}, past the limit of {limit} rows: give a larger --limit")]
    PageLimit { end: usize, limit: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldType {
    Ref(Uuid),
    Bool,
    String,
    Number,
    Date,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowField {
    pub id: u32,
    pub name: String,
    pub name_ru: String,
    pub ty: FieldType,
    pub service: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowDescr {
    pub fields: Vec<RowField>,
    pub digest: Option<String>,
}

impl RowDescr {
    pub fn field(&self, name: &str) -> Option<usize> {
        let k = fold_key(name);
        self.fields
            .iter()
            .position(|f| fold_key(&f.name_ru) == k || fold_key(&f.name) == k)
    }

    fn names(&self) -> String {
        self.fields
            .iter()
            .map(|f| f.name_ru.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cell {
    Value(Value),
    UnknownRef { clsid: Uuid, uuid: Uuid },
    RecordKey,
    Unsupported(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub cells: Vec<Cell>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Roles {
    pub key: Option<String>,
    pub folder: Option<String>,
    pub parent: Option<String>,
    pub period: Option<String>,
    pub posted: Option<String>,
    pub marked: Option<String>,
    pub main_field: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub rows: Vec<Row>,
    pub first: bool,
    pub last: bool,
    pub empty: bool,
    pub parent: Option<Ref>,
    pub descr: Option<RowDescr>,
    pub roles: Option<Roles>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Portion {
    pub forward: Option<Part>,
    pub backward: Option<Part>,
    pub refs: HashMap<(Uuid, Uuid), Presentation>,
    pub not_found: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListForm {
    pub form_id: String,
    pub fid: String,
    pub remote_key: String,
    pub object: Vec<String>,
    pub title: Option<String>,
    pub table_id: u32,
    pub data_path: u32,
    pub changes_version: u64,
    pub hierarchical: bool,
    pub descr: RowDescr,
    pub roles: Roles,
    pub ref_type: Option<TypeName>,
    pub first: Portion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    pub name: String,
    pub field_id: u32,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct TypeIndex {
    by_clsid: HashMap<Uuid, Indexed>,
}

#[derive(Debug, Clone)]
enum Indexed {
    Ref(TypeName),
    Enum {
        ty: TypeName,
        values: HashMap<Uuid, String>,
    },
}

impl TypeIndex {
    pub fn new(types: &[MdType]) -> TypeIndex {
        let mut by_clsid = HashMap::new();
        for t in types {
            let Ok(clsid) = Uuid::parse(&t.type_id) else {
                continue;
            };
            let entry = if t.kind == MdKind::Enum {
                let values = t
                    .values
                    .iter()
                    .filter_map(|v| Uuid::parse(&v.id).ok().map(|id| (id, v.name.clone())))
                    .collect();
                Indexed::Enum {
                    ty: TypeName::config(&format!("EnumRef.{}", t.name)),
                    values,
                }
            } else {
                let kind = format!("{}Ref", t.kind.en());
                if !UUID_REF_KINDS.contains(&kind.as_str()) {
                    continue;
                }
                Indexed::Ref(TypeName::config(&format!("{kind}.{}", t.name)))
            };
            by_clsid.insert(clsid, entry);
        }
        TypeIndex { by_clsid }
    }

    fn clsid_of(&self, ty: &TypeName) -> Option<Uuid> {
        self.by_clsid.iter().find_map(|(c, t)| match t {
            Indexed::Ref(n) | Indexed::Enum { ty: n, .. } if n == ty => Some(*c),
            _ => None,
        })
    }

    fn cell(&self, clsid: Uuid, uuid: Uuid) -> Cell {
        match self.by_clsid.get(&clsid) {
            Some(Indexed::Ref(ty)) => match Ref::new(ty.clone(), uuid) {
                Ok(r) => Cell::Value(Value::Ref(r)),
                Err(_) => Cell::UnknownRef { clsid, uuid },
            },
            Some(Indexed::Enum { ty, values }) => {
                let name = if uuid == Uuid::from_bytes([0; 16]) {
                    Some(String::new())
                } else {
                    values.get(&uuid).cloned()
                };
                match name.and_then(|n| EnumRef::new(ty.clone(), &n).ok()) {
                    Some(e) => Cell::Value(Value::EnumRef(e)),
                    None => Cell::UnknownRef { clsid, uuid },
                }
            }
            None => Cell::UnknownRef { clsid, uuid },
        }
    }
}

fn bad(answer: &'static str, field: &str, value: impl Into<String>) -> AnswerError {
    AnswerError::Bad {
        answer,
        field: field.to_string(),
        value: value.into(),
    }
}

fn blob(answer: &'static str, field: &str, text: &str) -> Result<Vec<BinNode>, AnswerError> {
    let clean: String = text.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(clean.as_bytes())
        .map_err(|e| bad(answer, field, format!("base64: {e}")))?;
    brace::binary::decode_all(&bytes).map_err(|e| bad(answer, field, format!("{e:?}")))
}

fn atom(n: &BinNode) -> Option<&Atom> {
    match n {
        BinNode::Atom(a) => Some(a),
        BinNode::List(_) => None,
    }
}

fn list(n: &BinNode) -> Option<&[BinNode]> {
    match n {
        BinNode::List(l) => Some(l),
        BinNode::Atom(_) => None,
    }
}

fn int(n: &BinNode) -> Option<i128> {
    atom(n)?.as_int()
}

fn text(n: &BinNode) -> Option<String> {
    atom(n)?.as_str()
}

fn guid(n: &BinNode) -> Option<Uuid> {
    atom(n)?.as_guid().map(|g| Uuid::from_bytes(*g.as_bytes()))
}

fn code(n: &BinNode) -> Option<char> {
    int(n)
        .and_then(|v| u8::try_from(v).ok())
        .filter(u8::is_ascii_graphic)
        .map(char::from)
}

fn show(n: &BinNode) -> String {
    match n {
        BinNode::List(l) => format!("{{{}}}", l.iter().map(show).collect::<Vec<_>>().join(",")),
        BinNode::Atom(a) => {
            if let Some(c) = a.as_int() {
                c.to_string()
            } else if let Some(g) = a.as_guid() {
                Uuid::from_bytes(*g.as_bytes()).to_string()
            } else if let Some(t) = a.as_decimal() {
                t
            } else if let Some(t) = a.as_str() {
                format!("{t:?}")
            } else {
                format!("<tag {:#x}>", a.tag())
            }
        }
    }
}

pub fn parse_descr(text: &str, digest: Option<String>) -> Result<RowDescr, AnswerError> {
    const A: &str = "dataDescrData";
    let top = blob(A, A, text)?;
    let shape = || bad(A, "shape", top.iter().map(show).collect::<String>());
    if top.len() != 1 {
        return Err(shape());
    }
    let root = top.first().and_then(list).ok_or_else(shape)?;
    let inner = root.get(2).and_then(list).ok_or_else(shape)?;
    let body = inner.get(1).and_then(list).ok_or_else(shape)?;
    let count = body.get(1).and_then(int).ok_or_else(shape)?;
    let items = body.get(2..).unwrap_or(&[]);
    if usize::try_from(count).ok() != Some(items.len()) {
        return Err(bad(
            A,
            "field count",
            format!("{count} for {}", items.len()),
        ));
    }
    let fields = items
        .iter()
        .map(|f| field(f).ok_or_else(|| bad(A, "field", show(f))))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(RowDescr { fields, digest })
}

fn field(n: &BinNode) -> Option<RowField> {
    let f = list(n)?;
    let id = list(f.first()?)?.first().and_then(int)?;
    let name = text(f.get(1)?)?;
    let name_ru = text(f.get(2)?)?;
    let pattern = list(f.get(3)?)?.get(2).and_then(list)?;
    if text(pattern.first()?)?.as_str() != "Pattern" {
        return None;
    }
    let types = pattern.get(1..).unwrap_or(&[]);
    let ty = match types {
        [one] => {
            let t = list(one)?;
            match code(t.first()?)? {
                '#' => FieldType::Ref(t.get(1).and_then(guid)?),
                'B' => FieldType::Bool,
                'S' => FieldType::String,
                'N' => FieldType::Number,
                'D' => FieldType::Date,
                _ => FieldType::Other(show(one)),
            }
        }
        _ => FieldType::Other(types.iter().map(show).collect::<Vec<_>>().join(",")),
    };
    let flag = int(f.get(4)?)?;
    Some(RowField {
        id: u32::try_from(id).ok()?,
        name,
        name_ru,
        ty,
        service: flag == 2,
    })
}

pub fn parse_rows(
    text: &str,
    descr: &RowDescr,
    types: &TypeIndex,
) -> Result<Vec<Row>, AnswerError> {
    const A: &str = "rowsData";
    let top = blob(A, A, text)?;
    let shape = || bad(A, "shape", top.iter().map(show).collect::<String>());
    let root = top.first().and_then(list).ok_or_else(shape)?;
    let count = root.get(1).and_then(int).ok_or_else(shape)?;
    let items = root.get(2..).unwrap_or(&[]);
    if top.len() != 1 || usize::try_from(count).ok() != Some(items.len()) {
        return Err(bad(A, "row count", format!("{count} for {}", items.len())));
    }
    let mut rows = Vec::with_capacity(items.len());
    for r in items {
        let cells = list(r).ok_or_else(|| bad(A, "row", show(r)))?;
        if cells.len() != descr.fields.len() {
            return Err(bad(
                A,
                "row width",
                format!("{} cells for {} fields", cells.len(), descr.fields.len()),
            ));
        }
        rows.push(Row {
            cells: cells.iter().map(|c| cell(c, types)).collect(),
        });
    }
    Ok(rows)
}

fn cell(n: &BinNode, types: &TypeIndex) -> Cell {
    let unsupported = || Cell::Unsupported(show(n));
    let Some(c) = list(n) else {
        return unsupported();
    };
    let Some(kind) = c.first().and_then(code) else {
        return unsupported();
    };
    match (kind, c.get(1..).unwrap_or(&[])) {
        ('#', [clsid, _table, id]) if atom(id).is_some() => match (guid(clsid), guid(id)) {
            (Some(clsid), Some(uuid)) => types.cell(clsid, uuid),
            _ => unsupported(),
        },
        ('#', [_, key]) if list(key).is_some() => Cell::RecordKey,
        ('B', [v]) => match atom(v).and_then(Atom::as_bool) {
            Some(b) => Cell::Value(Value::Bool(b)),
            None => unsupported(),
        },
        ('S', [v]) => match text(v) {
            Some(s) => Cell::Value(Value::String(s)),
            None => unsupported(),
        },
        ('N', [v]) => match atom(v).and_then(Atom::as_decimal) {
            Some(t) => match Decimal::parse(&t) {
                Ok(d) => Cell::Value(Value::Number(d)),
                Err(_) => unsupported(),
            },
            None => unsupported(),
        },
        ('D', [v]) => match int(v).and_then(date_of_ticks) {
            Some(d) => Cell::Value(Value::Date(d)),
            None => unsupported(),
        },
        _ => unsupported(),
    }
}

fn date_of_ticks(t: i128) -> Option<Date> {
    if t < 0 {
        return None;
    }
    let secs = i64::try_from(t / TICKS_PER_SECOND).ok()?;
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let z = days - 719_162 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let n = |v: i64| u32::try_from(v).ok();
    Date::new(
        n(y)?,
        n(m)?,
        n(d)?,
        n(rem / 3600)?,
        n(rem % 3600 / 60)?,
        n(rem % 60)?,
    )
}

fn parse_doc<'i>(answer: &'static str, text: &'i str) -> Result<Document<'i>, AnswerError> {
    Document::parse(text).map_err(|e| AnswerError::Parse {
        answer,
        message: e.to_string(),
    })
}

fn body_text(body: &[u8]) -> std::borrow::Cow<'_, str> {
    String::from_utf8_lossy(body.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(body))
}

fn response<'a, 'i>(
    answer: &'static str,
    doc: &'a Document<'i>,
) -> Result<Node<'a, 'i>, AnswerError> {
    let root = doc.root_element();
    if is(root, crate::answer::NS_VRS, "exception") {
        let descr = root
            .descendants()
            .find(|n| is(*n, crate::answer::NS_CORE, "descr"))
            .and_then(|n| n.text())
            .unwrap_or_default();
        return Err(AnswerError::Exception {
            answer,
            descr: descr.trim().to_string(),
        });
    }
    if !is(root, NS_WS, "response") {
        return Err(AnswerError::NotResponse {
            answer,
            root: root.tag_name().name().to_string(),
        });
    }
    Ok(root)
}

fn child<'a, 'i>(n: Node<'a, 'i>, ns: &str, local: &str) -> Option<Node<'a, 'i>> {
    n.children().find(|c| is(*c, ns, local))
}

fn child_text(n: Node, ns: &str, local: &str) -> Option<String> {
    child(n, ns, local).and_then(|c| c.text().map(str::to_string))
}

fn flag(n: Node, name: &str) -> bool {
    n.attribute(name).and_then(boolean).unwrap_or(false)
}

fn ref_of(answer: &'static str, n: Node) -> Result<Ref, AnswerError> {
    let ty = n
        .attribute((NS_XSI, "type"))
        .ok_or_else(|| bad(answer, n.tag_name().name(), "no xsi:type"))?;
    let (prefix, local) = ty.split_once(':').unwrap_or(("", ty));
    let ns = n.lookup_namespace_uri(Some(prefix)).unwrap_or_default();
    let tn = TypeName::new(ns, local);
    let text = n.text().unwrap_or_default();
    let uuid = Uuid::parse(text).map_err(|_| bad(answer, n.tag_name().name(), text))?;
    Ref::new(tn, uuid).map_err(|_| bad(answer, n.tag_name().name(), ty))
}

fn portion(
    answer: &'static str,
    n: Node,
    descr: Option<&RowDescr>,
    types: &TypeIndex,
) -> Result<Portion, AnswerError> {
    let mut out = Portion {
        not_found: flag(n, "notFound"),
        ..Portion::default()
    };
    let mut current = descr.cloned();
    for p in n.children().filter(|c| c.is_element()) {
        let name = p.tag_name().name();
        if p.tag_name().namespace() != Some(NS_WS) {
            continue;
        }
        match name {
            "forward" | "backward" => {
                let part = part(answer, p, current.as_ref(), types)?;
                if let Some(d) = &part.descr {
                    current = Some(d.clone());
                }
                let slot = if name == "forward" {
                    &mut out.forward
                } else {
                    &mut out.backward
                };
                if slot.is_some() {
                    return Err(AnswerError::Duplicate {
                        answer,
                        field: name.to_string(),
                    });
                }
                *slot = Some(part);
            }
            "refs" => refs_into(answer, p, &mut out.refs)?,
            _ => {}
        }
    }
    Ok(out)
}

fn part(
    answer: &'static str,
    p: Node,
    descr: Option<&RowDescr>,
    types: &TypeIndex,
) -> Result<Part, AnswerError> {
    let own = match child_text(p, NS_WS, "dataDescrData") {
        Some(t) => Some(parse_descr(&t, child_text(p, NS_WS, "dataDescrDigest"))?),
        None => None,
    };
    let roles = p.attribute("key").map(|_| Roles {
        key: attr(p, "key"),
        folder: attr(p, "folder"),
        parent: attr(p, "parent"),
        period: attr(p, "period"),
        posted: attr(p, "posted"),
        marked: attr(p, "marked"),
        main_field: attr(p, "mainField"),
    });
    let parent = match child(p, NS_WS, "rowsParent") {
        Some(n) if n.attribute((NS_XSI, "nil")) != Some("true") => Some(ref_of(answer, n)?),
        _ => None,
    };
    let rows = match child_text(p, NS_WS, "rowsData") {
        Some(t) => {
            let d = own
                .as_ref()
                .or(descr)
                .ok_or_else(|| bad(answer, "rowsData", "rows without a description"))?;
            parse_rows(&t, d, types)?
        }
        None => Vec::new(),
    };
    Ok(Part {
        rows,
        first: flag(p, "fif"),
        last: flag(p, "lil"),
        empty: flag(p, "intentionallyEmpty"),
        parent,
        descr: own,
        roles,
    })
}

fn refs_into(
    answer: &'static str,
    n: Node,
    out: &mut HashMap<(Uuid, Uuid), Presentation>,
) -> Result<(), AnswerError> {
    for item in n.children().filter(|c| is(*c, NS_WS, "item")) {
        let t = item.attribute("typeId").unwrap_or_default();
        let clsid = Uuid::parse(t).map_err(|_| bad(answer, "refs item typeId", t))?;
        for r in item.children().filter(|c| is(*c, NS_WS, "ref")) {
            let t = r.attribute("id").unwrap_or_default();
            let id = Uuid::parse(t).map_err(|_| bad(answer, "refs ref id", t))?;
            let text = r.text().unwrap_or_default().to_string();
            let p = match r.attribute("valid") {
                Some("true") => Presentation::Valid(text),
                Some(other) => return Err(bad(answer, "refs ref valid", other)),
                None if text.is_empty() => Presentation::Blank,
                None => Presentation::NotFound(text),
            };
            out.insert((clsid, id), p);
        }
    }
    Ok(())
}

pub fn parse_form(body: &[u8], types: &TypeIndex) -> Result<ListForm, AnswerError> {
    const A: &str = "logForm/query";
    let text = body_text(body);
    let doc = parse_doc(A, &text)?;
    let root = response(A, &doc)?;
    let need = |local: &str| {
        child_text(root, NS_WS, local).ok_or_else(|| AnswerError::Missing {
            answer: A,
            field: local.to_string(),
        })
    };
    let form_id = need("id")?;
    let fid = need("fid")?;
    let remote_key = need("remotekey")?;
    let object = root
        .children()
        .filter(|c| is(*c, NS_WS, "objname"))
        .filter_map(|c| c.text().map(str::to_string))
        .collect();
    let title = root
        .descendants()
        .find(|c| is(*c, NS_WS, "attl"))
        .and_then(|c| c.text().map(str::to_string));
    let data_path = root
        .children()
        .filter(|c| is(*c, NS_WS, "property") && flag(*c, "main"))
        .find_map(|c| c.attribute("id").and_then(unsigned))
        .ok_or_else(|| AnswerError::Missing {
            answer: A,
            field: "property main".into(),
        })?;
    let list = child(root, NS_WS, "props")
        .and_then(|p| child(p, NS_UO, "v"))
        .ok_or_else(|| AnswerError::Missing {
            answer: A,
            field: "props/v (not a list form?)".into(),
        })?;
    let changes_version = child_text(list, NS_DL, "ChangesVersion")
        .and_then(|t| t.parse::<u64>().ok())
        .ok_or_else(|| AnswerError::Missing {
            answer: A,
            field: "ChangesVersion".into(),
        })?;
    let hierarchical = flag(list, "HierAvailable");
    let dsfd = child(root, NS_WS, "dsfd").ok_or_else(|| AnswerError::Missing {
        answer: A,
        field: "dsfd (no rows of a dynamic list)".into(),
    })?;
    let table_id = dsfd
        .attribute("id")
        .and_then(unsigned)
        .ok_or_else(|| bad(A, "dsfd id", dsfd.attribute("id").unwrap_or_default()))?;
    let answer = child(dsfd, NS_WS, "answer").ok_or_else(|| AnswerError::Missing {
        answer: A,
        field: "dsfd/answer".into(),
    })?;
    let first = portion(A, answer, None, types)?;
    let head = first
        .forward
        .as_ref()
        .into_iter()
        .chain(first.backward.as_ref())
        .find(|p| p.descr.is_some());
    let (descr, roles) = match head {
        Some(p) => (p.descr.clone(), p.roles.clone().unwrap_or_default()),
        None => (None, Roles::default()),
    };
    let descr = descr.ok_or_else(|| AnswerError::Missing {
        answer: A,
        field: "dataDescrData".into(),
    })?;
    let ref_type = list_ref_type(&first, &descr, &roles);
    Ok(ListForm {
        form_id,
        fid,
        remote_key,
        object,
        title,
        table_id,
        data_path,
        changes_version,
        hierarchical,
        descr,
        roles,
        ref_type,
        first,
    })
}

fn list_ref_type(p: &Portion, descr: &RowDescr, roles: &Roles) -> Option<TypeName> {
    let parts = || p.forward.iter().chain(p.backward.iter());
    if let Some(r) = parts().find_map(|x| x.parent.as_ref()) {
        return Some(r.type_name().clone());
    }
    let key = descr.field(roles.key.as_deref()?)?;
    parts()
        .flat_map(|x| x.rows.iter())
        .find_map(|r| match r.cells.get(key) {
            Some(Cell::Value(Value::Ref(r))) => Some(r.type_name().clone()),
            _ => None,
        })
}

pub fn parse_dlist(
    body: &[u8],
    descr: &RowDescr,
    types: &TypeIndex,
) -> Result<Portion, AnswerError> {
    const A: &str = "dlist";
    let text = body_text(body);
    let doc = parse_doc(A, &text)?;
    let root = response(A, &doc)?;
    portion(A, root, Some(descr), types)
}

pub fn parse_columns(
    body: &[u8],
    table_id: u32,
    data_path: u32,
) -> Result<Vec<Column>, AnswerError> {
    const A: &str = "logForm/elements";
    let text = body_text(body);
    let doc = parse_doc(A, &text)?;
    let root = response(A, &doc)?;
    let table = root
        .descendants()
        .find(|n| is(*n, NS_LF, "table") && n.attribute("id").and_then(unsigned) == Some(table_id))
        .ok_or_else(|| AnswerError::Missing {
            answer: A,
            field: format!("table {table_id}"),
        })?;
    let mut out = Vec::new();
    for n in table_fields(table) {
        if n.attribute("visible") == Some("false") {
            continue;
        }
        let ids: Vec<u32> = child(n, NS_LF, "prop")
            .map(|p| {
                p.children()
                    .filter(|c| is(*c, NS_LF, "id"))
                    .filter_map(|c| c.text().and_then(unsigned))
                    .collect()
            })
            .unwrap_or_default();
        if let [path, id] = ids.as_slice()
            && *path == data_path
        {
            out.push(Column {
                name: n.attribute("name").unwrap_or_default().to_string(),
                field_id: *id,
                title: child_text(n, NS_LF, "title"),
            });
        }
    }
    Ok(out)
}

pub(crate) fn table_fields<'a, 'i>(table: Node<'a, 'i>) -> Vec<Node<'a, 'i>> {
    let mut out = Vec::new();
    let mut stack: Vec<Node> = table.children().rev().collect();
    while let Some(n) = stack.pop() {
        if !n.is_element() || n.tag_name().namespace() != Some(NS_LF) {
            continue;
        }
        match n.tag_name().name() {
            "predefined" | "addProps" | "contextMenu" => {}
            "field" => out.push(n),
            _ => stack.extend(n.children().rev()),
        }
    }
    out
}

const REQUEST_HEAD: &str = "\u{feff}<?xml version=\"1.0\" encoding=\"UTF-8\"?>";

pub fn form_query(key: &str) -> Vec<u8> {
    format!(
        "{REQUEST_HEAD}<request xmlns=\"http://v8.1c.ru/8.2/mngsrv/ws\" \
xmlns:cfg=\"http://v8.1c.ru/8.1/data/enterprise/current-config\" xmlns:ent=\"http://v8.1c.ru/8.1/data/enterprise\" \
xmlns:v8=\"http://v8.1c.ru/8.1/data/core\" xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" \
xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"><key>{}</key><mw>0</mw><mh>0</mh><pres>false</pres>\
<trdata>false</trdata></request>",
        escape_text(key)
    )
    .into_bytes()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Position {
    Top,
    At(Ref),
    Parent(Ref),
    Root(Ref),
}

#[derive(Debug, Clone)]
pub struct DlistQuery<'a> {
    pub form: &'a ListForm,
    pub forward: bool,
    pub backward: bool,
    pub page: u32,
    pub position: Position,
    pub settings: Option<(&'a str, u64)>,
}

fn ref_xml(tag: &str, r: &Ref) -> String {
    format!(
        "<{tag} xsi:type=\"cfg:{}\">{}</{tag}>",
        escape_attr(r.type_name().local()),
        r.uuid()
    )
}

pub fn dlist_query(q: &DlistQuery) -> Vec<u8> {
    let f = q.form;
    let mut a = format!(
        " remoteKey=\"{}\" tableId=\"{}\" formUUID=\"{}\"",
        escape_attr(&f.remote_key),
        f.table_id,
        escape_attr(&f.fid)
    );
    if q.settings.is_some() {
        a.push_str(" refresh=\"true\"");
    }
    if q.forward {
        a.push_str(" forward=\"true\"");
    }
    if q.backward {
        a.push_str(" backward=\"true\"");
    }
    a.push_str(&format!(
        " autopos=\"true\" page=\"{}\" source=\"1\"",
        q.page
    ));
    if matches!(q.position, Position::Parent(_)) {
        a.push_str(" level=\"0\"");
    } else {
        a.push_str(" autoParent=\"true\" level=\"-1\" autoLevel=\"true\"");
    }
    a.push_str(" presents=\"true\"");
    match (&q.settings, &f.descr.digest) {
        (None, Some(d)) => a.push_str(&format!(" dataDescrDigest=\"{}\"", escape_attr(d))),
        _ => a.push_str(" needDataDescr=\"true\""),
    }
    let (state, composer, version) = match q.settings {
        Some((s, v)) => (
            "1",
            format!("<dl:SettingsComposer>{s}</dl:SettingsComposer>"),
            v,
        ),
        None => ("0", "<dl:SettingsComposer/>".to_string(), f.changes_version),
    };
    let mut b = format!(
        "<dataPath><dl:id>{}</dl:id></dataPath><list ChangeState=\"{state}\">{composer}\
<dl:ChangesVersion>{version}</dl:ChangesVersion></list>",
        f.data_path
    );
    if let Position::Parent(r) = &q.position {
        b.push_str(&ref_xml("parent", r));
    }
    b.push_str(
        "<period><v8:variant xsi:type=\"v8:StandardPeriodVariant\">Custom</v8:variant>\
<v8:startDate>0001-01-01T00:00:00</v8:startDate><v8:endDate>0001-01-01T00:00:00</v8:endDate></period>",
    );
    match &q.position {
        Position::At(r) => {
            b.push_str(&ref_xml("checkSL", r));
            b.push_str(&ref_xml("checkC", r));
        }
        Position::Root(r) => b.push_str(&ref_xml("checkCP", r)),
        Position::Top | Position::Parent(_) => {}
    }
    format!(
        "{REQUEST_HEAD}<request xmlns=\"http://v8.1c.ru/8.2/mngsrv/ws\" \
xmlns:cfg=\"http://v8.1c.ru/8.1/data/enterprise/current-config\" \
xmlns:dcscor=\"http://v8.1c.ru/8.1/data-composition-system/core\" \
xmlns:dcsset=\"http://v8.1c.ru/8.1/data-composition-system/settings\" \
xmlns:dl=\"http://v8.1c.ru/8.2/managed-application/dynamic-list\" xmlns:v8=\"http://v8.1c.ru/8.1/data/core\" \
xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"{a}>{b}\
</request>"
    )
    .into_bytes()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Comparison {
    Equal,
    NotEqual,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
    Contains,
    NotContains,
}

impl Comparison {
    fn xml(self) -> &'static str {
        match self {
            Comparison::Equal => "Equal",
            Comparison::NotEqual => "NotEqual",
            Comparison::Less => "Less",
            Comparison::LessOrEqual => "LessOrEqual",
            Comparison::Greater => "Greater",
            Comparison::GreaterOrEqual => "GreaterOrEqual",
            Comparison::Contains => "Contains",
            Comparison::NotContains => "NotContains",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filter {
    pub field: String,
    pub op: Comparison,
    pub value: String,
}

impl Filter {
    pub fn parse(s: &str) -> Result<Filter, ListError> {
        const OPS: [(&str, Comparison); 8] = [
            ("!~", Comparison::NotContains),
            ("<>", Comparison::NotEqual),
            ("<=", Comparison::LessOrEqual),
            (">=", Comparison::GreaterOrEqual),
            ("~", Comparison::Contains),
            ("=", Comparison::Equal),
            ("<", Comparison::Less),
            (">", Comparison::Greater),
        ];
        let at = s
            .char_indices()
            .find_map(|(i, _)| {
                OPS.iter()
                    .find(|(op, _)| s[i..].starts_with(op))
                    .map(|(op, c)| (i, op.len(), *c))
            })
            .ok_or_else(|| ListError::Filter(s.to_string()))?;
        let (i, len, op) = at;
        let field = s[..i].trim();
        if field.is_empty() {
            return Err(ListError::Filter(s.to_string()));
        }
        Ok(Filter {
            field: field.to_string(),
            op,
            value: s[i + len..].to_string(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Order {
    pub field: String,
    pub desc: bool,
}

impl Order {
    pub fn parse(s: &str) -> Result<Order, ListError> {
        let mut words = s.split_whitespace();
        let field = words
            .next()
            .ok_or_else(|| ListError::Order(s.to_string()))?;
        let desc = match words.next().map(str::to_lowercase).as_deref() {
            None | Some("asc") | Some("возр") => false,
            Some("desc") | Some("убыв") => true,
            Some(_) => return Err(ListError::Order(s.to_string())),
        };
        if words.next().is_some() {
            return Err(ListError::Order(s.to_string()));
        }
        Ok(Order {
            field: field.to_string(),
            desc,
        })
    }
}

fn right(f: &RowField, value: &str, types: &TypeIndex) -> Result<(String, String), ListError> {
    let err = |reason: String| ListError::Value {
        field: f.name_ru.clone(),
        reason,
    };
    let value = match f.ty {
        FieldType::String => value,
        _ => value.trim(),
    };
    Ok(match &f.ty {
        FieldType::String => ("xs:string".into(), value.to_string()),
        FieldType::Number => {
            let d = Decimal::parse(value).map_err(|e| err(e.to_string()))?;
            ("xs:decimal".into(), d.to_string())
        }
        FieldType::Bool => {
            let b = match value.to_lowercase().as_str() {
                "true" | "истина" | "да" => "true",
                "false" | "ложь" | "нет" => "false",
                _ => return Err(err(format!("not a boolean: {value:?}"))),
            };
            ("xs:boolean".into(), b.into())
        }
        FieldType::Date => {
            let full = if value.len() == 10 {
                format!("{value}T00:00:00")
            } else {
                value.to_string()
            };
            let d = Date::parse(&full).map_err(|e| err(e.to_string()))?;
            ("xs:dateTime".into(), d.to_string())
        }
        FieldType::Ref(clsid) => match types.by_clsid.get(clsid) {
            Some(Indexed::Ref(ty)) => {
                let u = Uuid::parse(value)
                    .map_err(|e| err(format!("{e} (a reference is its uuid)")))?;
                (format!("cfg:{}", ty.local()), u.to_string())
            }
            Some(Indexed::Enum { ty, values }) => {
                let k = fold_key(value);
                let name = values
                    .values()
                    .find(|n| fold_key(n) == k)
                    .ok_or_else(|| err(format!("{value:?} is not a value of {}", ty.local())))?;
                (format!("cfg:{}", ty.local()), name.clone())
            }
            None => return Err(err("its type is not among the types of the session".into())),
        },
        FieldType::Other(t) => {
            return Err(err(format!("a filter on the type {t} is not implemented")));
        }
    })
}

pub fn settings(
    filters: &[Filter],
    orders: &[Order],
    descr: &RowDescr,
    types: &TypeIndex,
) -> Result<Option<String>, ListError> {
    if filters.is_empty() && orders.is_empty() {
        return Ok(None);
    }
    let field = |name: &str| {
        descr
            .field(name)
            .and_then(|i| descr.fields.get(i))
            .ok_or_else(|| ListError::NoField(name.to_string(), descr.names()))
    };
    let mut s = String::new();
    if !orders.is_empty() {
        s.push_str("<dcsset:settings><dcsset:order>");
        for (i, o) in orders.iter().enumerate() {
            let f = field(&o.field)?;
            s.push_str(&format!(
                "<dcsset:item xsi:type=\"dcsset:OrderItemField\" iID=\"{}\"><dcsset:field>{}</dcsset:field>\
<dcsset:orderType>{}</dcsset:orderType></dcsset:item>",
                i + 1,
                escape_text(&f.name_ru),
                if o.desc { "Desc" } else { "Asc" }
            ));
        }
        s.push_str("<dcsset:viewMode>Normal</dcsset:viewMode></dcsset:order></dcsset:settings>");
    }
    if filters.is_empty() {
        s.push_str("<dcsset:fixedSettings/>");
    } else {
        s.push_str("<dcsset:fixedSettings><dcsset:filter>");
        for (i, flt) in filters.iter().enumerate() {
            let f = field(&flt.field)?;
            let (ty, text) = right(f, &flt.value, types)?;
            s.push_str(&format!(
                "<dcsset:item xsi:type=\"dcsset:FilterItemComparison\" iID=\"{}\"><dcsset:left \
xsi:type=\"dcscor:Field\">{}</dcsset:left><dcsset:comparisonType>{}</dcsset:comparisonType><dcsset:right \
xsi:type=\"{}\">{}</dcsset:right></dcsset:item>",
                i + 1,
                escape_text(&f.name_ru),
                flt.op.xml(),
                escape_attr(&ty),
                escape_text(&text)
            ));
        }
        s.push_str("</dcsset:filter></dcsset:fixedSettings>");
    }
    Ok(Some(s))
}

#[derive(Debug, Clone, Default)]
pub struct ListQuery {
    pub object: String,
    pub form: Option<String>,
    pub parent: Option<Uuid>,
    pub filters: Vec<Filter>,
    pub orders: Vec<Order>,
    pub skip: usize,
    pub take: Option<usize>,
    pub limit: usize,
    pub presentations: bool,
}

#[derive(Debug, Clone)]
pub struct ListView {
    pub form: ListForm,
    pub columns: Vec<(Option<usize>, String)>,
    pub columns_warning: Option<String>,
    pub rows: Vec<Row>,
    pub more: bool,
    pub refs: Presentations,
    pub enums: EnumPresentations,
    pub presentations_error: Option<String>,
    pub close_error: Option<String>,
}

impl ListView {
    pub fn role(&self, name: Option<&str>) -> Option<usize> {
        self.form.descr.field(name?)
    }
}

type Columns = Vec<(Option<usize>, String)>;

fn map_columns(columns: &[Column], descr: &RowDescr) -> Result<(Columns, Vec<String>), String> {
    if columns.is_empty() {
        return Err("the table of the form has no columns".into());
    }
    let mut out = Vec::with_capacity(columns.len());
    let mut missing = Vec::new();
    for c in columns {
        match descr.fields.iter().position(|f| f.id == c.field_id) {
            Some(i) => {
                let title = c
                    .title
                    .clone()
                    .unwrap_or_else(|| descr.fields[i].name_ru.clone());
                out.push((Some(i), title));
            }
            None => {
                let name = format!("{} (field {})", c.name, c.field_id);
                if !missing.contains(&name) {
                    missing.push(name);
                }
                out.push((None, c.title.clone().unwrap_or_else(|| c.name.clone())));
            }
        }
    }
    if out.iter().all(|(i, _)| i.is_none()) {
        return Err(format!(
            "no column of the form shows a field of the rows: {}",
            missing.join(", ")
        ));
    }
    Ok((out, missing))
}

fn descr_columns(descr: &RowDescr) -> Columns {
    descr
        .fields
        .iter()
        .enumerate()
        .filter(|(_, f)| !f.service)
        .map(|(i, f)| (Some(i), f.name_ru.clone()))
        .collect()
}

fn form_key(q: &ListQuery) -> Result<String, ListError> {
    let parts: Vec<&str> = q.object.split('.').collect();
    if parts.len() != 2 || parts.iter().any(|p| p.trim().is_empty()) {
        return Err(ListError::Object(q.object.clone()));
    }
    Ok(format!(
        "{}.{}",
        q.object,
        q.form.as_deref().unwrap_or("ФормаСписка")
    ))
}

fn row_key(form: &ListForm, row: &Row) -> Option<Ref> {
    let i = form.descr.field(form.roles.key.as_deref()?)?;
    match row.cells.get(i) {
        Some(Cell::Value(Value::Ref(r))) => Some(r.clone()),
        _ => None,
    }
}

impl<R: RandomSource> Session<R> {
    fn dlist(&mut self, q: &DlistQuery, types: &TypeIndex) -> Result<Portion, SessionError> {
        let req = crate::body::zstd_compress(&dlist_query(q));
        let h = self.headers("application/xml", true, true);
        let resp = self.step("dlist", "POST", "/e1cib/dlist?cmd=query", &h, &req, &[200])?;
        Ok(parse_dlist(&Self::decoded(&resp)?, &q.form.descr, types)?)
    }

    pub fn run_list(&mut self, q: &ListQuery) -> Result<ListView, SessionError> {
        let key = form_key(q)?;
        if let Some(t) = q.take {
            let end = q.skip.saturating_add(t);
            if end > q.limit {
                return Err(ListError::PageLimit {
                    end,
                    limit: q.limit,
                }
                .into());
            }
        }
        let info = self
            .login_info()
            .cloned()
            .ok_or_else(|| SessionError::Protocol("not logged in".into()))?;
        let conf = info
            .config
            .ok_or_else(|| SessionError::Protocol("login response had no config".into()))?;
        let roles = info.roles_id.unwrap_or_else(|| "0".to_string());
        let md_types = self.session_types("the rows of a list")?;
        let types = TypeIndex::new(&md_types);

        let h = self.headers("application/xml", true, true);
        let req = crate::body::zstd_compress(&form_query(&key));
        let resp = self.step(
            "logForm/query",
            "POST",
            "/e1cib/logForm?cmd=query",
            &h,
            &req,
            &[200],
        )?;
        let form = parse_form(&Self::decoded(&resp)?, &types)?;
        let rows = self.list_rows(&form, q, &types, &conf, &roles);
        let close_error = self.close_form(&form.fid).err().map(|e| e.to_string());
        let (columns, columns_warning, rows, more, refs) = rows?;
        let mut view = ListView {
            form,
            columns,
            columns_warning,
            rows,
            more,
            refs: Presentations::new(Vec::new(), Vec::new()),
            enums: EnumPresentations::new(Vec::new()),
            presentations_error: None,
            close_error,
        };
        if q.presentations
            && let Err(e) = self.present(&mut view, &refs)
        {
            view.presentations_error = Some(e.to_string());
        }
        Ok(view)
    }

    #[allow(
        clippy::type_complexity,
        reason = "one tuple of the parts of the view, used once"
    )]
    fn list_rows(
        &mut self,
        form: &ListForm,
        q: &ListQuery,
        types: &TypeIndex,
        conf: &str,
        roles: &str,
    ) -> Result<
        (
            Columns,
            Option<String>,
            Vec<Row>,
            bool,
            HashMap<(Uuid, Uuid), Presentation>,
        ),
        SessionError,
    > {
        let sysver = self.version.clone();
        let h = self.headers("application/xml", false, false);
        let elements = self.step(
            "logForm/elements",
            "GET",
            &format!(
                "/e1cib/logForm?sysver={sysver}&cmd=elements&clang=ru&formId={}&confver={conf}&rolesId={roles}\
&iweb=16",
                form.form_id
            ),
            &h,
            &[],
            &[200],
        );
        let (columns, columns_warning) = match elements
            .and_then(|r| Self::decoded(&r))
            .map_err(|e| e.to_string())
            .and_then(|b| {
                parse_columns(&b, form.table_id, form.data_path).map_err(|e| e.to_string())
            })
            .and_then(|c| map_columns(&c, &form.descr))
        {
            Ok((c, missing)) if missing.is_empty() => (c, None),
            Ok((c, missing)) => (
                c,
                Some(format!(
                    "columns of the form whose field is not in the rows are shown empty: {}",
                    missing.join(", ")
                )),
            ),
            Err(e) => (
                descr_columns(&form.descr),
                Some(format!(
                    "the columns of the form are not known ({e}): the fields of the rows are shown"
                )),
            ),
        };

        let settings = settings(&q.filters, &q.orders, &form.descr, types)?;
        let settings = settings
            .as_deref()
            .map(|s| (s, form.changes_version + CHANGES_STEP));
        let parent = match q.parent {
            Some(u) => {
                let ty = form.ref_type.clone().ok_or_else(|| {
                    SessionError::Protocol(
                        "the list has no reference type: --parent cannot be given".into(),
                    )
                })?;
                Some(Ref::new(ty, u).map_err(|e| SessionError::Protocol(e.to_string()))?)
            }
            None => None,
        };
        let page = q.take.map_or(PAGE, |t| {
            u32::try_from(t).unwrap_or(u32::MAX).clamp(PAGE, MAX_PAGE)
        });
        let mut refs = form.first.refs.clone();
        let rows: Vec<Row>;
        let last: bool;
        let start_position = match &parent {
            Some(p) => Some(Position::Parent(p.clone())),
            None if settings.is_some() => Some(Position::Top),
            None => None,
        };
        match start_position {
            Some(position) => {
                let p = self.dlist(
                    &DlistQuery {
                        form,
                        forward: true,
                        backward: false,
                        page,
                        position,
                        settings,
                    },
                    types,
                )?;
                let mut p = p;
                refs.extend(std::mem::take(&mut p.refs));
                (rows, last) = opening(forward_of(&mut p, form)?);
            }
            None => {
                let mut first = form.first.clone();
                let f = forward_of(&mut first, form)?;
                if !(f.first && form.first.not_found) {
                    let p = self.dlist(
                        &DlistQuery {
                            form,
                            forward: true,
                            backward: false,
                            page,
                            position: Position::Top,
                            settings: None,
                        },
                        types,
                    )?;
                    let mut p = p;
                    refs.extend(std::mem::take(&mut p.refs));
                    (rows, last) = opening(forward_of(&mut p, form)?);
                } else {
                    (rows, last) = opening(f);
                }
            }
        }
        let (mut rows, mut last) = (rows, last);
        let need = q.take.map(|t| q.skip.saturating_add(t));
        let cap = need.unwrap_or(q.limit);
        let mut seen: std::collections::HashSet<Ref> =
            rows.iter().filter_map(|r| row_key(form, r)).collect();
        while !last && rows.len() <= cap {
            let Some(at) = rows.last().and_then(|r| row_key(form, r)) else {
                return Err(SessionError::Protocol(
                    "the rows of this list have no reference key: paging past the first portion is not \
implemented"
                        .into(),
                ));
            };
            let p = self.dlist(
                &DlistQuery {
                    form,
                    forward: true,
                    backward: false,
                    page,
                    position: Position::At(at.clone()),
                    settings: None,
                },
                types,
            )?;
            let mut p = p;
            refs.extend(std::mem::take(&mut p.refs));
            let f = forward_of(&mut p, form)?;
            let mut new = f.rows;
            if new.first().and_then(|r| row_key(form, r)).as_ref() == Some(&at) {
                new.remove(0);
            }
            if new.is_empty() && !(f.last || f.empty) {
                return Err(SessionError::Protocol(
                    "dlist: a portion without new rows before the end of the list".into(),
                ));
            }
            for r in &new {
                if let Some(k) = row_key(form, r)
                    && !seen.insert(k)
                {
                    return Err(SessionError::Protocol(
                        "dlist: a row came again: the server does not advance from the position asked".into(),
                    ));
                }
            }
            last = f.last || f.empty;
            rows.extend(new);
        }
        if need.is_none() && rows.len() > q.limit {
            return Err(ListError::Limit(q.limit).into());
        }
        let more = !last || need.is_some_and(|n| rows.len() > n);
        let end = need.unwrap_or(rows.len()).min(rows.len());
        let rows = rows.drain(q.skip.min(end)..end).collect();
        Ok((columns, columns_warning, rows, more, refs))
    }

    fn present(
        &mut self,
        view: &mut ListView,
        known: &HashMap<(Uuid, Uuid), Presentation>,
    ) -> Result<(), SessionError> {
        let values: Vec<&Value> = view
            .rows
            .iter()
            .flat_map(|r| r.cells.iter())
            .filter_map(|c| match c {
                Cell::Value(v) => Some(v),
                _ => None,
            })
            .collect();
        let all = crate::refs::collect(values.iter().copied());
        let types = TypeIndex::new(&self.session_types("references")?);
        let mut items = Vec::with_capacity(all.len());
        let mut ask = Vec::new();
        for r in &all {
            let hit = if r.is_empty() {
                Some(Presentation::Empty)
            } else {
                types
                    .clsid_of(r.type_name())
                    .and_then(|c| known.get(&(c, r.uuid())).cloned())
            };
            match hit {
                Some(p) => items.push((r.clone(), p)),
                None => ask.push(r.clone()),
            }
        }
        if !ask.is_empty() {
            let asked = self.presentations(&ask)?;
            items.extend(asked.items().iter().cloned());
        }
        view.refs = Presentations::new(items, Vec::new());
        let enums = crate::enums::collect(values.iter().copied());
        view.enums = self.enum_presentations(&enums)?;
        Ok(())
    }
}

fn forward_of(p: &mut Portion, form: &ListForm) -> Result<Part, SessionError> {
    let f = p
        .forward
        .take()
        .ok_or_else(|| SessionError::Protocol("dlist: the answer has no forward part".into()))?;
    let other = |d: &Option<RowDescr>| d.as_ref().is_some_and(|d| d.fields != form.descr.fields);
    if other(&f.descr) || p.backward.as_ref().is_some_and(|b| other(&b.descr)) {
        return Err(SessionError::Protocol(
            "dlist: the description of the rows changed (another set or order of fields): not implemented".into(),
        ));
    }
    Ok(f)
}

fn opening(f: Part) -> (Vec<Row>, bool) {
    let last = f.last || f.empty || f.rows.is_empty();
    (f.rows, last)
}
