use roxmltree::Node;

use crate::answer::{AnswerError, NS_WS, Reader, boolean, read};
use crate::body;
use crate::formdescr::{FieldType, FormData, FormField, NS_UO, Slot};
use crate::info::Unrecognized;
use crate::metadata::{MdKind, MdType};
use crate::session::{RandomSource, Session, SessionError};
use crate::value::{
    NS_CONFIG, NS_CORE, NS_XS, Ref, TypeName, UUID_REF_KINDS, Uuid, Value, config_ref_class,
    is_structure_key,
};
use crate::xml::{self, XmlError};

pub(crate) const NS_LF: &str = "http://v8.1c.ru/8.2/managed-application/logform";

const IGNORED: &[&str] = &[
    "fover",
    "assemblyVersion",
    "name",
    "objname",
    "property",
    "remotekey",
    "prms",
    "cd",
    "cacheformid",
    "wokey",
    "ucDsc",
    "securityInfo",
    "compactScaleIfNeeded",
    "hippo",
];

#[derive(Debug, thiserror::Error)]
pub enum ObjectError {
    #[error(transparent)]
    Answer(#[from] AnswerError),
    #[error("logForm/query: field {field}: {error}")]
    Value { field: String, error: XmlError },
    #[error("logForm/query: {0}")]
    Shape(String),
    #[error("logForm/query: the form shows {got}, not the object asked {asked}")]
    OtherObject { asked: String, got: String },
    #[error("change refused before the write: {0}")]
    Change(String),
    #[error("logForm/call: {0}")]
    WriteAnswer(String),
    #[error(
        "logForm/call: the write was sent and its outcome is not known ({0}); read the object before writing it again"
    )]
    WriteSent(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    Value(Value),
    Raw(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ObjectField {
    pub field: FormField,
    pub value: Cell,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ObjectTable {
    pub field: FormField,
    pub columns: Vec<FormField>,
    pub rows: Vec<Vec<Cell>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ObjectData {
    pub reference: Ref,
    pub form_id: String,
    pub title: Option<String>,
    pub read_only: bool,
    pub fields: Vec<ObjectField>,
    pub tables: Vec<ObjectTable>,
    pub unrecognized: Vec<Unrecognized>,
}

pub fn form_key(r: &Ref) -> Result<String, ObjectError> {
    let local = r.type_name().local();
    let (kind, name) = local
        .split_once('.')
        .ok_or_else(|| ObjectError::Shape(format!("not a reference type: {local}")))?;
    let stem = kind
        .strip_suffix("Ref")
        .ok_or_else(|| ObjectError::Shape(format!("not a reference type: {local}")))?;
    Ok(format!(
        "{}.{name}.ФормаОбъекта",
        MdKind::of_stem(stem).ru()
    ))
}

pub fn parse_ref(text: &str) -> Result<Ref, String> {
    let bad = || {
        format!(
            "not a reference: {text:?} (cfg:<Kind>Ref.<Name>(<uuid>) or <Вид>.<Имя>(<uuid>), e.g. \
             cfg:CatalogRef.Имя(…) or Справочник.Имя(…))"
        )
    };
    let (ty, rest) = text.split_once('(').ok_or_else(bad)?;
    let uuid = rest.strip_suffix(')').ok_or_else(bad)?;
    let uuid = Uuid::parse(uuid).map_err(|_| bad())?;
    let local = match ty.strip_prefix("cfg:") {
        Some(local) => local.to_string(),
        None => {
            let (kind, name) = ty.split_once('.').ok_or_else(bad)?;
            let kind = MdKind::of_ru(kind).ok_or_else(bad)?;
            format!("{}Ref.{name}", kind.en())
        }
    };
    let (kind, name) = local.split_once('.').ok_or_else(bad)?;
    if !UUID_REF_KINDS.contains(&kind) || !is_structure_key(name) {
        return Err(bad());
    }
    Ref::new(TypeName::config(&local), uuid).map_err(|_| bad())
}

pub(crate) fn open_request(key: &str, r: Option<&Ref>) -> String {
    let prms = r.map_or(String::new(), |r| {
        format!(
            "<prms><prm><name>Ключ</name><val xsi:type=\"cfg:{}\">{}</val></prm></prms>",
            escape(r.type_name().local()),
            r.uuid()
        )
    });
    format!(
        "\u{feff}<?xml version=\"1.0\" encoding=\"UTF-8\"?><request xmlns=\"http://v8.1c.ru/8.2/mngsrv/ws\" \
         xmlns:cfg=\"{NS_CONFIG}\" xmlns:ent=\"http://v8.1c.ru/8.1/data/enterprise\" xmlns:v8=\"{NS_CORE}\" \
         xmlns:xs=\"{NS_XS}\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"><key>{}</key>{prms}<mw>0</mw>\
         <mh>0</mh><pres>false</pres><trdata>false</trdata></request>",
        escape(key),
    )
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub(crate) fn opened(body: &[u8]) -> Result<String, AnswerError> {
    read("logForm/query", body, NS_WS, &[], &mut Vec::new(), |r| {
        let fid = r.text(NS_WS, "fid")?.ok_or_else(|| r.missing("fid"))?;
        Uuid::parse(&fid).map_err(|_| r.bad("fid", &fid))?;
        Ok(fid)
    })
}

fn is_form_id(id: &str) -> bool {
    let Some((kind, uuid)) = id
        .strip_prefix("urn:form:md:")
        .and_then(|rest| rest.split_once(':'))
    else {
        return false;
    };
    !kind.is_empty() && kind.bytes().all(|b| b.is_ascii_digit()) && Uuid::parse(uuid).is_ok()
}

fn form_id_of(body: &[u8]) -> Result<String, AnswerError> {
    read("logForm/query", body, NS_WS, &[], &mut Vec::new(), |r| {
        let id = r.text(NS_WS, "id")?.ok_or_else(|| r.missing("id"))?;
        if !is_form_id(&id) {
            return Err(r.bad("id", &id));
        }
        Ok(id)
    })
}

pub(crate) fn type_of_id(id: &str, types: &[MdType]) -> Option<TypeName> {
    let platform = [
        ("9b6abf8b-0173-48e5-b0a0-83b21fcf63c5", NS_XS, "string"),
        ("b0be78f2-0ee6-4d31-a3bb-77dd32ba5bec", NS_XS, "decimal"),
        ("5d4125ad-f6e7-4313-be32-f71d0ab60915", NS_XS, "boolean"),
        ("aae38c48-a877-411c-a6d3-fbaa1f83c4bd", NS_XS, "dateTime"),
        ("fc01b5df-97fe-449b-83d4-218a090e681e", NS_CORE, "UUID"),
    ];
    if let Some((_, ns, local)) = platform.iter().find(|(g, _, _)| g.eq_ignore_ascii_case(id)) {
        return Some(TypeName::new(ns, local));
    }
    let t = types.iter().find(|t| t.type_id.eq_ignore_ascii_case(id))?;
    let kind = if t.kind == MdKind::Enum {
        "EnumRef".to_string()
    } else {
        format!("{}Ref", t.kind.en())
    };
    let ty = TypeName::config(&format!("{kind}.{}", t.name));
    config_ref_class(&ty).is_some().then_some(ty)
}

pub(crate) fn implied_type(ty: &FieldType, types: &[MdType]) -> Option<TypeName> {
    match ty.ids.as_slice() {
        [one] => type_of_id(one, types),
        _ => None,
    }
}

fn cell(n: Node, field: &FormField, implied: Option<&TypeName>) -> Result<Cell, ObjectError> {
    let wrap = |error| ObjectError::Value {
        field: field.display_name().to_string(),
        error,
    };
    match implied {
        Some(t) => xml::typed_value(n, Some(t)).map(Cell::Value).map_err(wrap),
        None if xml::has_xsi_type(n) => xml::typed_value(n, None).map(Cell::Value).map_err(wrap),
        None => {
            if n.children().any(|c| c.is_element()) {
                return Err(ObjectError::Shape(format!(
                    "field {}: an element inside a value of a type the client does not model",
                    field.display_name()
                )));
            }
            Ok(Cell::Raw(n.text().unwrap_or("").to_string()))
        }
    }
}

pub(crate) fn uo_children<'a, 'i>(n: Node<'a, 'i>) -> impl Iterator<Item = Node<'a, 'i>> {
    n.children()
        .filter(|c| c.is_element() && c.tag_name().namespace() == Some(NS_UO))
}

fn table_rows(
    n: Node,
    table: &crate::formdescr::FormTable,
    types: &[MdType],
) -> Result<Vec<Vec<Cell>>, ObjectError> {
    let name = table.field.display_name();
    let shape = |what: String| ObjectError::Shape(format!("tabular section {name}: {what}"));
    let count = n
        .attribute("count")
        .ok_or_else(|| shape("no count".into()))?;
    let count: usize = count
        .parse()
        .map_err(|_| shape(format!("bad count {count:?}")))?;
    let implied: Vec<Option<TypeName>> = table
        .columns
        .iter()
        .map(|c| implied_type(&c.ty, types))
        .collect();
    let mut rows = Vec::new();
    for c in uo_children(n) {
        match c.tag_name().name() {
            "item" => {
                let values: Vec<Node> = uo_children(c).collect();
                if values.len() != table.columns.len()
                    || values.iter().any(|v| v.tag_name().name() != "v")
                {
                    return Err(shape(format!(
                        "a row of {} values for {} columns",
                        values.len(),
                        table.columns.len()
                    )));
                }
                let row = values
                    .iter()
                    .zip(table.columns.iter().zip(&implied))
                    .map(|(v, (col, ty))| cell(*v, col, ty.as_ref()))
                    .collect::<Result<Vec<_>, _>>()?;
                rows.push(row);
            }
            "column" => {}
            other => return Err(shape(format!("unexpected uo:{other}"))),
        }
    }
    if rows.len() != count {
        return Err(shape(format!("{} rows for count {count}", rows.len())));
    }
    Ok(rows)
}

pub(crate) fn object_of(
    body: &[u8],
    data: &FormData,
    types: &[MdType],
    r: Asked,
    unrecognized: &mut Vec<Unrecognized>,
) -> Result<ObjectData, ObjectError> {
    let text = xml::value_text(body);
    read(
        "logForm/query",
        text.as_bytes(),
        NS_WS,
        IGNORED,
        unrecognized,
        |rd| Ok(read_object(rd, data, types, r)),
    )?
}

pub(crate) fn main_of<'a, 'i>(
    properties: &[Node<'a, 'i>],
    props: Node<'a, 'i>,
) -> Result<Node<'a, 'i>, ObjectError> {
    let mains: Vec<usize> = properties
        .iter()
        .enumerate()
        .filter(|(_, p)| p.attribute("main").and_then(boolean) == Some(true))
        .map(|(i, _)| i)
        .collect();
    let [at] = mains.as_slice() else {
        return Err(ObjectError::Shape(format!(
            "the form has {} main attributes (property main=\"true\"), not one",
            mains.len()
        )));
    };
    let values: Vec<Node> = uo_children(props).collect();
    if values.len() != properties.len() {
        return Err(ObjectError::Shape(format!(
            "props has a value for each attribute of the form: values {}, attributes {}",
            values.len(),
            properties.len()
        )));
    }
    values
        .get(*at)
        .copied()
        .ok_or_else(|| ObjectError::Shape("no value of the main attribute".into()))
}

#[derive(Clone, Copy)]
pub(crate) enum Asked<'r> {
    Existing(&'r Ref),
    New(&'r Ref),
}

fn read_object(
    rd: &Reader,
    data: &FormData,
    types: &[MdType],
    asked: Asked,
) -> Result<ObjectData, ObjectError> {
    let (r, new) = match asked {
        Asked::Existing(r) => (r, false),
        Asked::New(r) => (r, true),
    };
    let form_id = rd.text(NS_WS, "id")?.ok_or_else(|| rd.missing("id"))?;
    rd.text(NS_WS, "fid")?;
    let main_value = |n: &Node| n.tag_name().name() == "v";
    let title = rd
        .one(NS_WS, "dd")?
        .and_then(|dd| {
            dd.descendants()
                .find(|n| n.is_element() && n.tag_name().name() == "attl")
        })
        .and_then(|n| n.text().map(str::to_string));
    let read_only = rd.children(NS_WS, "elchs").iter().any(|e| {
        e.descendants().any(|n| {
            n.is_element()
                && n.tag_name().namespace() == Some(NS_LF)
                && n.tag_name().name() == "main"
                && n.attribute("readOnly") == Some("true")
        })
    });
    let props = rd.one(NS_WS, "props")?.ok_or_else(|| rd.missing("props"))?;
    let main = main_of(&rd.children(NS_WS, "property"), props)?;
    if !main_value(&main) {
        return Err(ObjectError::Shape(format!(
            "the value of the main attribute is <uo:{}>, not <uo:v>",
            main.tag_name().name()
        )));
    }
    let values: Vec<Node> = uo_children(main).collect();
    if values.len() != data.layout.len() || values.iter().any(|v| v.tag_name().name() != "v") {
        return Err(ObjectError::Shape(format!(
            "{} values for {} fields of the description",
            values.len(),
            data.layout.len()
        )));
    }
    let mut fields = Vec::with_capacity(data.fields.len());
    let mut tables = Vec::with_capacity(data.tables.len());
    for (v, slot) in values.iter().zip(&data.layout) {
        match *slot {
            Slot::Field(i) => {
                let f = &data.fields[i];
                fields.push(ObjectField {
                    field: f.clone(),
                    value: cell(*v, f, implied_type(&f.ty, types).as_ref())?,
                });
            }
            Slot::Table(i) => {
                let t = &data.tables[i];
                tables.push(ObjectTable {
                    field: t.field.clone(),
                    columns: t.columns.clone(),
                    rows: table_rows(*v, t, types)?,
                });
            }
            Slot::Other => {}
        }
    }
    let shown = fields
        .iter()
        .find(|f| f.field.name.as_deref() == Some("Ref"))
        .ok_or_else(|| ObjectError::Shape("no field Ref".into()))?;
    let asked = || format!("{}({})", r.type_name(), r.uuid());
    match &shown.value {
        Cell::Value(Value::Ref(got)) if same_ref(got, r) && (!new || got.is_empty()) => {}
        Cell::Value(Value::Ref(got)) => {
            return Err(ObjectError::OtherObject {
                asked: asked(),
                got: format!("{}({})", got.type_name(), got.uuid()),
            });
        }
        Cell::Value(v) => {
            return Err(ObjectError::OtherObject {
                asked: asked(),
                got: format!("a value of kind {:?}", v.kind()),
            });
        }
        Cell::Raw(t) => {
            return Err(ObjectError::OtherObject {
                asked: asked(),
                got: format!("the text {t:?}"),
            });
        }
    }
    Ok(ObjectData {
        reference: r.clone(),
        form_id,
        title,
        read_only,
        fields,
        tables,
        unrecognized: Vec::new(),
    })
}

fn same_ref(a: &Ref, b: &Ref) -> bool {
    a.uuid() == b.uuid()
        && a.type_name().ns() == b.type_name().ns()
        && a.type_name().local().to_lowercase() == b.type_name().local().to_lowercase()
}

impl<R: RandomSource> Session<R> {
    pub fn read_object(&mut self, r: &Ref) -> Result<ObjectData, SessionError> {
        let conf = self.prologue(true)?.conf;
        let types = self.session_types("the types of the values of the object")?;
        let key = form_key(r)?;
        let (fid, answer) = self.open_form(&key, Some(r))?;
        let read = self
            .describe_form(&conf, &answer)
            .and_then(|(data, mut unrecognized)| {
                let mut object = object_of(
                    &answer,
                    &data,
                    &types,
                    Asked::Existing(r),
                    &mut unrecognized,
                )?;
                object.unrecognized = unrecognized;
                Ok(object)
            });
        self.closing(&fid, read)
    }

    pub(crate) fn open_form(
        &mut self,
        key: &str,
        r: Option<&Ref>,
    ) -> Result<(String, Vec<u8>), SessionError> {
        let req = body::zstd_compress(open_request(key, r).as_bytes());
        let h = self.headers("application/xml", true, true);
        let resp = self.step(
            "logForm/query",
            "POST",
            "/e1cib/logForm?cmd=query",
            &h,
            &req,
            &[200],
        )?;
        let answer = Self::decoded(&resp)?;
        let fid = opened(&answer).map_err(ObjectError::from)?;
        Ok((fid, answer))
    }

    pub(crate) fn closing<T>(
        &mut self,
        fid: &str,
        outcome: Result<T, SessionError>,
    ) -> Result<T, SessionError> {
        let closed = self.close_form(fid);
        match (outcome, closed) {
            (Ok(v), Ok(())) => Ok(v),
            (Err(e), Ok(())) | (Ok(_), Err(e)) => Err(e),
            (Err(e), Err(c)) => Err(SessionError::Protocol(format!(
                "{e}; the form was not closed either: {c}"
            ))),
        }
    }

    pub(crate) fn describe_form(
        &mut self,
        conf: &str,
        answer: &[u8],
    ) -> Result<(FormData, Vec<Unrecognized>), SessionError> {
        let form_id = form_id_of(answer).map_err(ObjectError::from)?;
        let roles = self
            .login_info()
            .and_then(|i| i.roles_id.clone())
            .unwrap_or_else(|| "0".to_string());
        let sysver = self.version.clone();
        let h = self.headers("application/xml", false, false);
        let resp = self.step(
            "logForm/dataDescrWMethods",
            "GET",
            &format!(
                "/e1cib/logForm?sysver={sysver}&cmd=dataDescrWMethods&formId={form_id}&confver={conf}\
                 &rolesId={roles}"
            ),
            &h,
            &[],
            &[200],
        )?;
        let mut unrecognized = Vec::new();
        let data = FormData::from_answer(&Self::decoded(&resp)?, &mut unrecognized)
            .map_err(ObjectError::from)?;
        Ok((data, unrecognized))
    }
}
