use std::collections::HashMap;

use roxmltree::Node;

use crate::answer::{AnswerError, NS_WS, Reader, attr, boolean, read, unsigned};
use crate::info::Unrecognized;

pub(crate) const NS_UO: &str = "http://v8.1c.ru/8.2/uobjects";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormKind {
    Object,
    RecordSet,
    Record,
    List,
}

impl FormKind {
    pub fn code(self) -> u8 {
        match self {
            FormKind::Object => 0,
            FormKind::RecordSet => 2,
            FormKind::Record => 3,
            FormKind::List => 4,
        }
    }

    pub fn form_id(self, md_uuid: &str) -> String {
        format!("urn:form:md:{}:{md_uuid}", self.code())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FieldType {
    pub ids: Vec<String>,
    pub length: Option<u32>,
    pub digits: Option<u32>,
    pub fraction: Option<u32>,
    pub nonnegative: bool,
    pub date_fraction: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormField {
    pub id: String,
    pub name: Option<String>,
    pub name_ru: Option<String>,
    pub ty: FieldType,
    pub read_only: bool,
    pub no_edit: bool,
}

impl FormField {
    pub fn display_name(&self) -> &str {
        self.name_ru
            .as_deref()
            .or(self.name.as_deref())
            .unwrap_or(&self.id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormTable {
    pub field: FormField,
    pub columns: Vec<FormField>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Field(usize),
    Table(usize),
    Other,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FormData {
    pub object: String,
    pub fields: Vec<FormField>,
    pub tables: Vec<FormTable>,
    pub layout: Vec<Slot>,
}

struct Structure<'a, 'i> {
    fields: Vec<Node<'a, 'i>>,
}

impl FormData {
    pub fn from_answer(
        body: &[u8],
        unrecognized: &mut Vec<Unrecognized>,
    ) -> Result<FormData, AnswerError> {
        read(
            "logForm/dataDescrWMethods",
            body,
            NS_WS,
            &[],
            unrecognized,
            form_data,
        )
    }
}

fn form_data(r: &Reader) -> Result<FormData, AnswerError> {
    let descr = r.one(NS_WS, "descr")?.ok_or_else(|| r.missing("descr"))?;
    let mut structures: HashMap<String, Structure> = HashMap::new();
    let mut collections: HashMap<String, String> = HashMap::new();
    for n in descr.descendants().filter(|n| n.is_element()) {
        if n.tag_name().namespace() != Some(NS_UO) {
            continue;
        }
        match n.tag_name().name() {
            "structure" => {
                let name = attr(n, "name").ok_or_else(|| r.missing("structure name"))?;
                let fields = n
                    .children()
                    .filter(|c| c.is_element() && c.tag_name().namespace() == Some(NS_UO))
                    .filter(|c| c.tag_name().name() == "field")
                    .collect();
                if structures
                    .insert(name.clone(), Structure { fields })
                    .is_some()
                {
                    return Err(AnswerError::Duplicate {
                        answer: r.answer,
                        field: format!("structure {name}"),
                    });
                }
            }
            "collection" => {
                let name = attr(n, "name").ok_or_else(|| r.missing("collection name"))?;
                let item = attr(n, "itemType").ok_or_else(|| r.missing("collection itemType"))?;
                collections.insert(name, item);
            }
            _ => {}
        }
    }
    let root = structures
        .get("root")
        .ok_or_else(|| r.missing("structure root"))?;
    let (object, main) = root
        .fields
        .iter()
        .find_map(|f| object_of(*f).map(|s| (attr(*f, "name"), s)))
        .ok_or_else(|| r.missing("object field of root"))?;
    let object = object.ok_or_else(|| r.missing("name of the object field of root"))?;
    let rows = collections.get(&main).cloned().unwrap_or(main);
    let rows = structures
        .get(&rows)
        .ok_or_else(|| r.missing(&format!("structure {rows}")))?;
    let mut data = FormData {
        object,
        ..FormData::default()
    };
    for f in &rows.fields {
        match object_of(*f) {
            None => {
                data.layout.push(Slot::Field(data.fields.len()));
                data.fields.push(field(r, *f)?);
            }
            Some(s) => {
                let Some(item) = collections.get(&s) else {
                    data.layout.push(Slot::Other);
                    continue;
                };
                let items = structures
                    .get(item)
                    .ok_or_else(|| r.missing(&format!("structure {item}")))?;
                let columns = items
                    .fields
                    .iter()
                    .filter(|c| object_of(**c).is_none())
                    .map(|c| field(r, *c))
                    .collect::<Result<Vec<_>, _>>()?;
                data.layout.push(Slot::Table(data.tables.len()));
                data.tables.push(FormTable {
                    field: field(r, *f)?,
                    columns,
                });
            }
        }
    }
    Ok(data)
}

fn type_of<'a, 'i>(f: Node<'a, 'i>) -> Option<Node<'a, 'i>> {
    f.children().find(|c| {
        c.is_element() && c.tag_name().namespace() == Some(NS_UO) && c.tag_name().name() == "type"
    })
}

pub(crate) fn object_of(f: Node) -> Option<String> {
    let t = type_of(f)?;
    (t.attribute("object") == Some("true"))
        .then(|| attr(t, "name"))
        .flatten()
}

pub(crate) fn field(r: &Reader, f: Node) -> Result<FormField, AnswerError> {
    let id = attr(f, "id").ok_or_else(|| r.missing("field id"))?;
    let ty = match type_of(f) {
        Some(t) => {
            let ids: Vec<String> = t
                .text()
                .unwrap_or("")
                .split_whitespace()
                .map(str::to_string)
                .collect();
            let number = ids.iter().any(|id| id.eq_ignore_ascii_case(NUMBER));
            FieldType {
                length: r.attr_parsed(t, "length", "type length", unsigned)?,
                digits: r
                    .attr_parsed(t, "digits", "type digits", unsigned)?
                    .or(number.then_some(DEFAULT_DIGITS)),
                ids,
                fraction: r.attr_parsed(t, "precision", "type precision", unsigned)?,
                nonnegative: r
                    .attr_parsed(t, "unsigned", "type unsigned", boolean)?
                    .unwrap_or(false),
                date_fraction: attr(t, "dateFraction"),
            }
        }
        None => FieldType::default(),
    };
    Ok(FormField {
        id,
        name: attr(f, "name"),
        name_ru: attr(f, "nameRu"),
        ty,
        read_only: f.attribute("mode") == Some("r"),
        no_edit: f.attribute("imode") == Some("r"),
    })
}

const NUMBER: &str = "b0be78f2-0ee6-4d31-a3bb-77dd32ba5bec";
const DEFAULT_DIGITS: u32 = 10;

const PLATFORM_TYPES: &[(&str, &str, &str)] = &[
    ("9b6abf8b-0173-48e5-b0a0-83b21fcf63c5", "String", "Строка"),
    ("b0be78f2-0ee6-4d31-a3bb-77dd32ba5bec", "Number", "Число"),
    ("5d4125ad-f6e7-4313-be32-f71d0ab60915", "Boolean", "Булево"),
    ("aae38c48-a877-411c-a6d3-fbaa1f83c4bd", "Date", "Дата"),
    (
        "fc01b5df-97fe-449b-83d4-218a090e681e",
        "UUID",
        "УникальныйИдентификатор",
    ),
    (
        "e199ca70-93cf-46ce-a54b-6edc88c3a296",
        "ValueStorage",
        "ХранилищеЗначения",
    ),
    (
        "87126200-3e98-44e0-b931-ccb1d7edc497",
        "BinaryData",
        "ДвоичныеДанные",
    ),
    ("af40a278-63bc-478e-91a8-19e0d16b10b5", "Null", "Null"),
    (
        "ee8d3e7c-f930-4a76-8aad-4ff9083a6ea6",
        "Undefined",
        "Неопределено",
    ),
    ("d47d59f8-73f0-481c-8b5e-f6384c0a4804", "Type", "Тип"),
    (
        "4772b3b4-f4a3-49c0-a1a5-8cb5961511a3",
        "ValueList",
        "СписокЗначений",
    ),
    (
        "2fdc88ec-7c9b-43cd-8ba5-873f043bdd88",
        "StandardPeriod",
        "СтандартныйПериод",
    ),
    (
        "f5c65050-3bbb-11d5-b988-0050bae0a95d",
        "TypeDescription",
        "ОписаниеТипов",
    ),
    (
        "5b2b819e-ebc7-4d0b-9af6-1d5e2c8cdb0c",
        "PointInTime",
        "МоментВремени",
    ),
    (
        "5a49abd5-936d-4f46-8499-9cb204efe378",
        "AccumulationRecordType",
        "ВидДвиженияНакопления",
    ),
    (
        "b1b064f3-ae38-49bf-8c6d-390c65fd94af",
        "ComparisonType",
        "ВидСравнения",
    ),
    (
        crate::report::SPREADSHEET,
        "SpreadsheetDocument",
        "ТабличныйДокумент",
    ),
];

pub fn platform_type(guid: &str) -> Option<(&'static str, &'static str)> {
    PLATFORM_TYPES
        .iter()
        .find(|(g, _, _)| g.eq_ignore_ascii_case(guid))
        .map(|(_, en, ru)| (*en, *ru))
}
