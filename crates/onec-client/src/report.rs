use roxmltree::{Document, Node};

use crate::answer::{AnswerError, NS_VRS, NS_WS, attr, read, text_of};
use crate::formdescr::{FormField, NS_UO, field, object_of};
use crate::info::Unrecognized;
use crate::spreadsheet::{self, CellCount, NS_SHEET, SheetChanges, SheetError, SheetLimits};
use crate::value::Value;
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
    format!(
        "\u{feff}<?xml version=\"1.0\" encoding=\"UTF-8\"?><request {}><key>{}</key><mw>0</mw><mh>0</mh>\
         <pres>false</pres><trdata>false</trdata></request>",
        namespaces(&[]),
        xml::escape_text(key)
    )
    .into_bytes()
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
    r.push_str("<pchs sinDesc=\"0\" sin=\"0\" seq=\"0\"/>");
    r.push_str(form.prms.as_deref().unwrap_or(""));
    r.push_str(form.dd.as_deref().unwrap_or(""));
    r.push_str("</request>");
    Ok(r.into_bytes())
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
