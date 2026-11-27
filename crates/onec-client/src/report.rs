use roxmltree::{Document, Node};

use crate::answer::{AnswerError, NS_MODULES, NS_VRS, NS_WS, attr, read, text_of};
use crate::formdescr::{FormField, NS_UO, field, object_of};
use crate::info::Unrecognized;
use crate::metadata::MdType;
use crate::object::implied_type;
use crate::spreadsheet::{self, CellCount, NS_SHEET, SheetChanges, SheetError, SheetLimits};
use crate::value::Value;
use crate::write::{Assign, field_value, value_xml};
use crate::xml::{self, XmlError};

pub const SPREADSHEET: &str = "e603103e-a318-4edc-a014-b1c6cf94d49f";

const REQUEST_NS: &[(&str, &str)] = &[
    ("cfg", "http://v8.1c.ru/8.1/data/enterprise/current-config"),
    ("m", "http://v8.1c.ru/8.2/managed-application/modules"),
    ("uo", NS_UO),
    ("v8", "http://v8.1c.ru/8.1/data/core"),
    ("xs", "http://www.w3.org/2001/XMLSchema"),
    ("xsi", "http://www.w3.org/2001/XMLSchema-instance"),
];

#[derive(Debug, thiserror::Error)]
pub enum ReportError {
    #[error(transparent)]
    Answer(#[from] AnswerError),
    #[error(transparent)]
    Sheet(#[from] SheetError),
    #[error(transparent)]
    Xml(#[from] XmlError),
    #[error("{answer}: XML parse error: {message}")]
    Parse {
        answer: &'static str,
        message: String,
    },
    #[error("attribute {name}: {error}")]
    Attribute { name: String, error: String },
    #[error(
        "{procedure}: the long operation did not end in {seconds} s ({calls} calls); it may still run on the server"
    )]
    PollTimeout {
        procedure: String,
        seconds: u64,
        calls: usize,
    },
    #[error("{procedure} returned {got}: {need}")]
    PollAnswer {
        procedure: String,
        got: String,
        need: String,
    },
    #[error("{answer}: missing {field}")]
    Missing {
        answer: &'static str,
        field: &'static str,
    },
    #[error("{answer}: bad {field}: {value:?}")]
    Bad {
        answer: &'static str,
        field: &'static str,
        value: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenedForm {
    pub id: String,
    pub fid: String,
    pub remote_key: String,
    pub fover: Option<String>,
    pub names: Vec<String>,
    prms: Option<String>,
    dd: Option<String>,
    namespaces: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormAttribute {
    pub position: usize,
    pub field: FormField,
    pub object: Option<String>,
}

impl FormAttribute {
    pub fn is_spreadsheet(&self) -> bool {
        self.field
            .ty
            .ids
            .iter()
            .any(|id| id.eq_ignore_ascii_case(SPREADSHEET))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedSheet {
    pub position: usize,
    pub changes: SheetChanges,
}

pub fn query_request(key: &str) -> Vec<u8> {
    query_request_with(key, &[])
}

pub fn query_request_with(key: &str, params: &[(&str, &str)]) -> Vec<u8> {
    let mut prms = String::new();
    if !params.is_empty() {
        prms.push_str("<prms>");
        for (name, value) in params {
            prms.push_str(&format!(
                "<prm><name>{}</name><val xsi:type=\"xs:string\">{}</val></prm>",
                xml::escape_text(name),
                xml::escape_text(value)
            ));
        }
        prms.push_str("</prms>");
    }
    format!(
        "\u{feff}<?xml version=\"1.0\" encoding=\"UTF-8\"?><request {}><key>{}</key>{prms}<mw>0</mw><mh>0</mh>\
         <pres>false</pres><trdata>false</trdata></request>",
        namespaces(&[]),
        xml::escape_text(key)
    )
    .into_bytes()
}

pub fn query_request_values(key: &str, params: &[(String, Value)]) -> Result<Vec<u8>, ReportError> {
    let mut prms = String::new();
    if !params.is_empty() {
        prms.push_str("<prms>");
        for (name, value) in params {
            prms.push_str(&format!(
                "<prm><name>{}</name>{}</prm>",
                xml::escape_text(name),
                xml::value_element("val", value)?
            ));
        }
        prms.push_str("</prms>");
    }
    Ok(format!(
        "\u{feff}<?xml version=\"1.0\" encoding=\"UTF-8\"?><request {}><key>{}</key>{prms}<mw>0</mw><mh>0</mh>\
         <pres>false</pres><trdata>false</trdata></request>",
        namespaces(&[]),
        xml::escape_text(key)
    )
    .into_bytes())
}

pub fn read_added_attributes(body: &[u8], count: usize) -> Result<Vec<FormAttribute>, ReportError> {
    let mut unrecognized = Vec::new();
    Ok(read(
        "logForm/query",
        body,
        NS_WS,
        &[],
        &mut unrecognized,
        |r| {
            let mut out = Vec::new();
            let Some(pdchs) = r.one(NS_WS, "pdchs")? else {
                return Ok(out);
            };
            for changed in pdchs.children().filter(|n| {
                n.is_element()
                    && n.tag_name().namespace() == Some(NS_UO)
                    && n.tag_name().name() == "changed"
                    && n.attribute("name") == Some("root")
            }) {
                for c in changed.children().filter(Node::is_element) {
                    if c.tag_name().namespace() != Some(NS_UO) || c.tag_name().name() != "added" {
                        return Err(r.bad(
                            "pdchs",
                            &format!("<{}> of the data of the form", c.tag_name().name()),
                        ));
                    }
                    out.push(FormAttribute {
                        position: count + out.len(),
                        field: field(r, c)?,
                        object: None,
                    });
                }
            }
            Ok(out)
        },
    )?)
}

pub fn read_opened_sheets(
    body: &[u8],
    attributes: &[FormAttribute],
    limits: SheetLimits,
    count: &mut CellCount,
) -> Result<Vec<ChangedSheet>, ReportError> {
    const ANSWER: &str = "logForm/query";
    let text = xml::value_text(body);
    let doc = Document::parse(&text).map_err(|e| ReportError::Parse {
        answer: ANSWER,
        message: e.to_string(),
    })?;
    let root = doc.root_element();
    let Some(props) = root.children().find(|n| {
        n.is_element() && n.tag_name().namespace() == Some(NS_WS) && n.tag_name().name() == "props"
    }) else {
        return Ok(Vec::new());
    };
    let properties: Vec<&str> = root
        .children()
        .filter(|n| {
            n.is_element()
                && n.tag_name().namespace() == Some(NS_WS)
                && n.tag_name().name() == "property"
        })
        .map(|n| n.attribute("id").unwrap_or(""))
        .collect();
    let values: Vec<Node> = props
        .children()
        .filter(|n| {
            n.is_element() && n.tag_name().namespace() == Some(NS_UO) && n.tag_name().name() == "v"
        })
        .collect();
    if !properties.is_empty() && properties.len() != values.len() {
        return Err(ReportError::Bad {
            answer: ANSWER,
            field: "props",
            value: format!(
                "{} values for {} properties",
                values.len(),
                properties.len()
            ),
        });
    }
    if properties.is_empty() && values.len() != attributes.len() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for a in attributes.iter().filter(|a| a.is_spreadsheet()) {
        let at = if properties.is_empty() {
            Some(a.position)
        } else {
            properties.iter().position(|id| *id == a.field.id)
        };
        let Some(v) = at.and_then(|i| values.get(i)) else {
            continue;
        };
        let is_document = v.children().any(|n| {
            n.is_element()
                && n.tag_name().namespace() == Some(NS_SHEET)
                && n.tag_name().name() == "distributedKey"
        });
        if !is_document {
            continue;
        }
        let changes = spreadsheet::read_document(*v, false, limits, count)?;
        let height = match &changes {
            SheetChanges::Full(s) | SheetChanges::WithoutContent { sheet: s, .. } => s.height,
        };
        if height > 0 {
            out.push(ChangedSheet {
                position: a.position,
                changes,
            });
        }
    }
    Ok(out)
}

pub fn read_opened(body: &[u8]) -> Result<OpenedForm, ReportError> {
    const ANSWER: &str = "logForm/query";
    let text = String::from_utf8_lossy(body.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(body));
    let mut unrecognized: Vec<Unrecognized> = Vec::new();
    let form = read(ANSWER, body, NS_WS, &[], &mut unrecognized, |r| {
        let text_req = |local: &'static str| -> Result<String, AnswerError> {
            r.text(NS_WS, local)?.ok_or_else(|| r.missing(local))
        };
        let id = text_req("id")?;
        let fid = text_req("fid")?;
        let remote_key = text_req("remotekey")?;
        let fover = r.text(NS_WS, "fover")?;
        let names = r
            .children(NS_WS, "name")
            .into_iter()
            .filter_map(text_of)
            .collect();
        let raw = |local: &'static str| -> Result<Option<String>, AnswerError> {
            Ok(r.one(NS_WS, local)?.map(|n| text[n.range()].to_string()))
        };
        Ok(OpenedForm {
            id,
            fid,
            remote_key,
            fover,
            names,
            prms: raw("prms")?,
            dd: raw("dd")?,
            namespaces: r
                .root
                .namespaces()
                .filter_map(|ns| ns.name().map(|p| (p.to_string(), ns.uri().to_string())))
                .filter(|(p, _)| p != "xml")
                .collect(),
        })
    })?;
    Ok(form)
}

pub fn fid_of(body: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(body.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(body));
    let doc = Document::parse(&text).ok()?;
    let root = doc.root_element();
    root.children()
        .find(|n| {
            n.is_element()
                && n.tag_name().namespace() == Some(NS_WS)
                && n.tag_name().name() == "fid"
        })
        .and_then(text_of)
}

pub fn read_attributes(body: &[u8]) -> Result<Vec<FormAttribute>, ReportError> {
    let mut unrecognized = Vec::new();
    Ok(read(
        "logForm/dataDescrWMethods",
        body,
        NS_WS,
        &[],
        &mut unrecognized,
        |r| {
            let descr = r.one(NS_WS, "descr")?.ok_or_else(|| r.missing("descr"))?;
            let root = descr
                .children()
                .find(|n| {
                    n.is_element()
                        && n.tag_name().namespace() == Some(NS_UO)
                        && n.tag_name().name() == "structure"
                        && n.attribute("name") == Some("root")
                })
                .ok_or_else(|| r.missing("structure root"))?;
            root.children()
                .filter(|n| {
                    n.is_element()
                        && n.tag_name().namespace() == Some(NS_UO)
                        && n.tag_name().name() == "field"
                })
                .enumerate()
                .map(|(position, f)| {
                    Ok(FormAttribute {
                        position,
                        field: field(r, f)?,
                        object: object_of(f),
                    })
                })
                .collect()
        },
    )?)
}

pub fn call_request(
    form: &OpenedForm,
    procedure: &str,
    params: &[Value],
) -> Result<Vec<u8>, ReportError> {
    call_request_with(form, procedure, params, EMPTY_PCHS)
}

pub const EMPTY_PCHS: &str = "<pchs sinDesc=\"0\" sin=\"0\" seq=\"0\"/>";

pub fn call_request_with(
    form: &OpenedForm,
    procedure: &str,
    params: &[Value],
    pchs: &str,
) -> Result<Vec<u8>, ReportError> {
    if let Some((p, uri)) = form
        .namespaces
        .iter()
        .find(|(p, uri)| REQUEST_NS.iter().any(|(q, u)| q == p && u != uri))
    {
        return Err(ReportError::Bad {
            answer: "logForm/query",
            field: "namespace prefix",
            value: format!("{p}={uri}"),
        });
    }
    let mut r = format!(
        "\u{feff}<?xml version=\"1.0\" encoding=\"UTF-8\"?><request {}>",
        namespaces(&form.namespaces)
    );
    r.push_str(&format!(
        "<rKey>{}</rKey><id>{}</id><fid>{}</fid>",
        xml::escape_text(&form.remote_key),
        xml::escape_text(&form.id),
        xml::escape_text(&form.fid)
    ));
    if let Some(f) = &form.fover {
        r.push_str(&format!("<fover>{}</fover>", xml::escape_text(f)));
    }
    r.push_str(&format!(
        "<prm name=\"{}\">{}</prm>",
        xml::escape_attr(procedure),
        xml::params_xml(params)?
    ));
    r.push_str(pchs);
    r.push_str(form.prms.as_deref().unwrap_or(""));
    r.push_str(form.dd.as_deref().unwrap_or(""));
    r.push_str("</request>");
    Ok(r.into_bytes())
}

pub fn attribute_edits(
    attributes: &[FormAttribute],
    sets: &[Assign],
    types: &[MdType],
) -> Result<Vec<(usize, String)>, ReportError> {
    let mut out: Vec<(usize, String)> = Vec::with_capacity(sets.len());
    for s in sets {
        let fail = |error: String| ReportError::Attribute {
            name: s.name.clone(),
            error,
        };
        let lower = s.name.to_lowercase();
        let a = attributes
            .iter()
            .find(|a| {
                [&a.field.name, &a.field.name_ru]
                    .into_iter()
                    .flatten()
                    .any(|n| n.to_lowercase() == lower)
            })
            .ok_or_else(|| {
                fail(format!(
                    "the form has no such attribute (attributes: {})",
                    attributes
                        .iter()
                        .map(|a| a.field.display_name())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            })?;
        if a.object.is_some() || a.is_spreadsheet() {
            return Err(fail(
                "an attribute holding an object or a spreadsheet document is not set".into(),
            ));
        }
        if out.iter().any(|(p, _)| *p == a.position) {
            return Err(fail("set twice".into()));
        }
        let v = field_value(&s.input, &a.field, types).map_err(fail)?;
        let typed = implied_type(&a.field.ty, types).is_none();
        let x = value_xml(&v, typed).map_err(|e| fail(e.to_string()))?;
        out.push((a.position, x));
    }
    out.sort_by_key(|(p, _)| *p);
    Ok(out)
}

pub fn attributes_pchs(edits: &[(usize, String)]) -> String {
    if edits.is_empty() {
        return EMPTY_PCHS.to_string();
    }
    let fields: Vec<String> = edits.iter().map(|(p, _)| p.to_string()).collect();
    let values: String = edits.iter().map(|(_, v)| v.as_str()).collect();
    format!(
        "<pchs sinDesc=\"0\" sin=\"0\" seq=\"0\"><uo:structure fields=\"{}\">{values}</uo:structure></pchs>",
        fields.join(" ")
    )
}

pub fn read_call_return(body: &[u8]) -> Result<Option<Value>, ReportError> {
    const ANSWER: &str = "logForm/call";
    let text = xml::value_text(body);
    let doc = Document::parse(&text).map_err(|e| ReportError::Parse {
        answer: ANSWER,
        message: e.to_string(),
    })?;
    let root = doc.root_element();
    if root.tag_name().namespace() == Some(NS_VRS) && root.tag_name().name() == "exception" {
        return Err(AnswerError::Exception {
            answer: ANSWER,
            descr: xml::parse_exception(body),
        }
        .into());
    }
    let Some(prm) = root.children().find(|n| {
        n.is_element() && n.tag_name().namespace() == Some(NS_WS) && n.tag_name().name() == "prm"
    }) else {
        return Ok(None);
    };
    let rets: Vec<Node> = prm
        .children()
        .filter(|n| {
            n.is_element()
                && n.tag_name().namespace() == Some(NS_MODULES)
                && n.tag_name().name() == "ret"
        })
        .collect();
    match rets.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(xml::value_of(*one)?)),
        _ => Err(ReportError::Bad {
            answer: ANSWER,
            field: "prm",
            value: format!("{} m:ret", rets.len()),
        }),
    }
}

pub fn read_states(body: &[u8]) -> Vec<String> {
    let text = xml::value_text(body);
    let Ok(doc) = Document::parse(&text) else {
        return Vec::new();
    };
    let Some(dd) = doc.root_element().children().find(|n| {
        n.is_element() && n.tag_name().namespace() == Some(NS_WS) && n.tag_name().name() == "dd"
    }) else {
        return Vec::new();
    };
    let ws = |n: Node, local: &str| {
        n.is_element() && n.tag_name().namespace() == Some(NS_WS) && n.tag_name().name() == local
    };
    dd.children()
        .filter(|n| ws(*n, "f"))
        .flat_map(|f| f.children().filter(|n| ws(*n, "shs")))
        .filter_map(|shs| shs.attribute("txt"))
        .filter(|t| !t.trim().is_empty())
        .map(str::to_string)
        .collect()
}

pub fn poll_running(
    procedure: &str,
    returned: Option<&Value>,
    running: Option<&str>,
) -> Result<bool, ReportError> {
    let got = || match returned {
        None => "nothing (a procedure)".to_string(),
        Some(v) => v
            .type_name()
            .map_or_else(|| "Неопределено".to_string(), |t| t.to_string()),
    };
    match (returned, running) {
        (Some(Value::Bool(b)), None) => Ok(*b),
        (_, None) => Err(ReportError::PollAnswer {
            procedure: procedure.to_string(),
            got: got(),
            need: "a poll procedure returns Булево (Истина while the operation runs), or name the Булево field \
                   of the Структура it returns"
                .into(),
        }),
        (Some(Value::Structure(st)), Some(field)) => {
            let lower = field.to_lowercase();
            match st.props().iter().find(|(k, _)| k.to_lowercase() == lower) {
                Some((_, Value::Bool(b))) => Ok(*b),
                Some((_, v)) => Err(ReportError::PollAnswer {
                    procedure: procedure.to_string(),
                    got: format!(
                        "{field} = {}",
                        v.type_name()
                            .map_or_else(|| "Неопределено".to_string(), |t| t.to_string())
                    ),
                    need: format!("the field {field} is not Булево"),
                }),
                None => Err(ReportError::PollAnswer {
                    procedure: procedure.to_string(),
                    got: format!(
                        "a Структура of {}",
                        st.props().iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(", ")
                    ),
                    need: format!("it has no field {field}"),
                }),
            }
        }
        (_, Some(field)) => Err(ReportError::PollAnswer {
            procedure: procedure.to_string(),
            got: got(),
            need: format!("a Структура with the Булево field {field} is expected"),
        }),
    }
}

pub fn read_call(
    body: &[u8],
    limits: SheetLimits,
    count: &mut CellCount,
) -> Result<Vec<ChangedSheet>, ReportError> {
    const ANSWER: &str = "logForm/call";
    let text = String::from_utf8_lossy(body.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(body));
    let doc = Document::parse(&text).map_err(|e| ReportError::Parse {
        answer: ANSWER,
        message: e.to_string(),
    })?;
    let root = doc.root_element();
    if root.tag_name().namespace() == Some(NS_VRS) && root.tag_name().name() == "exception" {
        return Err(AnswerError::Exception {
            answer: ANSWER,
            descr: xml::parse_exception(body),
        }
        .into());
    }
    if root.tag_name().namespace() != Some(NS_WS) {
        return Err(AnswerError::NotResponse {
            answer: ANSWER,
            root: root.tag_name().name().to_string(),
        }
        .into());
    }
    let Some(pchs) = root
        .children()
        .find(|n| n.is_element() && n.tag_name().name() == "pchs")
    else {
        return Ok(Vec::new());
    };
    let Some(structure) = uo(pchs, "structure") else {
        return Ok(Vec::new());
    };
    let values: Vec<Node> = structure
        .children()
        .filter(|n| {
            n.is_element() && n.tag_name().namespace() == Some(NS_UO) && n.tag_name().name() == "v"
        })
        .collect();
    let positions: Vec<usize> = match attr(structure, "fields") {
        Some(f) => f
            .split_whitespace()
            .map(|p| {
                p.parse().map_err(|_| ReportError::Bad {
                    answer: ANSWER,
                    field: "pchs fields",
                    value: p.to_string(),
                })
            })
            .collect::<Result<_, _>>()?,
        None => (0..values.len()).collect(),
    };
    if positions.len() != values.len() {
        return Err(ReportError::Bad {
            answer: ANSWER,
            field: "pchs fields",
            value: format!("{} fields, {} values", positions.len(), values.len()),
        });
    }
    let mut out = Vec::new();
    for (position, v) in positions.into_iter().zip(values) {
        let Some(changes) = v.children().find(|n| {
            n.is_element()
                && n.tag_name().namespace() == Some(NS_SHEET)
                && n.tag_name().name() == "changes"
        }) else {
            continue;
        };
        if !changes
            .children()
            .any(|n| n.is_element() && n.tag_name().name() != "commonPart")
        {
            continue;
        }
        out.push(ChangedSheet {
            position,
            changes: spreadsheet::read_changes(changes, limits, count)?,
        });
    }
    Ok(out)
}

fn uo<'a, 'i>(n: Node<'a, 'i>, local: &str) -> Option<Node<'a, 'i>> {
    n.children().find(|c| {
        c.is_element() && c.tag_name().namespace() == Some(NS_UO) && c.tag_name().name() == local
    })
}

fn namespaces(extra: &[(String, String)]) -> String {
    let mut s = String::from("xmlns=\"http://v8.1c.ru/8.2/mngsrv/ws\"");
    for (p, uri) in REQUEST_NS {
        s.push_str(&format!(" xmlns:{p}=\"{uri}\""));
    }
    for (p, uri) in extra {
        if !REQUEST_NS.iter().any(|(q, _)| q == p) {
            s.push_str(&format!(" xmlns:{p}=\"{}\"", xml::escape_attr(uri)));
        }
    }
    s
}
