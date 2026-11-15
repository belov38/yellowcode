use std::collections::HashMap;

use crate::answer::{AnswerError, NS_APP, read};
use crate::metadata::{MdKind, MdType};
use crate::refs::{FirstSeen, walk};
use crate::value::{EnumRef, TypeName, Uuid, Value};

pub(crate) const ANSWER: &str = "types/view/values";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnumPresentation {
    Synonym(String),
    Empty,
    UnknownType,
    UnknownValue,
}

#[derive(Debug, Clone)]
pub struct EnumPresentations {
    items: FirstSeen<EnumRef, EnumPresentation>,
}

impl EnumPresentations {
    pub fn new(items: Vec<(EnumRef, EnumPresentation)>) -> EnumPresentations {
        let mut index = FirstSeen::new();
        for (e, p) in items {
            *index.entry(&e, || EnumPresentation::Empty) = p;
        }
        EnumPresentations { items: index }
    }

    pub fn get(&self, e: &EnumRef) -> Option<&EnumPresentation> {
        self.items.get(e)
    }

    pub fn items(&self) -> &[(EnumRef, EnumPresentation)] {
        self.items.items()
    }
}

pub fn collect<'v>(values: impl IntoIterator<Item = &'v Value>) -> Vec<EnumRef> {
    let mut out: FirstSeen<EnumRef, ()> = FirstSeen::new();
    walk(values, |v| {
        if let Value::EnumRef(e) = v {
            out.entry(e, || ());
        }
    });
    out.into_items().into_iter().map(|(e, ())| e).collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EnumType {
    pub(crate) clsid: Uuid,
    pub(crate) names: Vec<String>,
    at: HashMap<String, usize>,
}

impl EnumType {
    pub(crate) fn new(clsid: Uuid, names: Vec<String>) -> Result<EnumType, AnswerError> {
        let mut at = HashMap::with_capacity(names.len());
        for (i, n) in names.iter().enumerate() {
            if at.insert(n.clone(), i).is_some() {
                return Err(AnswerError::Duplicate {
                    answer: "types",
                    field: format!("val code {n:?} of type {clsid}"),
                });
            }
        }
        Ok(EnumType { clsid, names, at })
    }
}

pub(crate) fn clsid_of(t: &MdType) -> Result<Uuid, AnswerError> {
    Uuid::parse(&t.type_id).map_err(|_| AnswerError::Bad {
        answer: "types",
        field: "type clsid".into(),
        value: t.type_id.clone(),
    })
}

pub(crate) fn enum_type(t: &MdType) -> Result<EnumType, AnswerError> {
    EnumType::new(
        clsid_of(t)?,
        t.values.iter().map(|v| v.name.clone()).collect(),
    )
}

pub(crate) fn enum_index(types: &[MdType]) -> HashMap<TypeName, Vec<&MdType>> {
    let mut out: HashMap<TypeName, Vec<&MdType>> = HashMap::new();
    for t in types.iter().filter(|t| t.kind == MdKind::Enum) {
        out.entry(TypeName::config(&format!("EnumRef.{}", t.name)))
            .or_default()
            .push(t);
    }
    out
}

fn checked(
    ty: &TypeName,
    index: &HashMap<TypeName, Vec<&MdType>>,
) -> Result<Option<EnumType>, AnswerError> {
    match index.get(ty).map(Vec::as_slice) {
        None | Some([]) => Ok(None),
        Some([t]) => enum_type(t).map(Some),
        Some(_) => Err(AnswerError::Duplicate {
            answer: "types",
            field: format!("type {ty}"),
        }),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Place {
    pub(crate) ty: usize,
    pub(crate) pos: usize,
}

pub(crate) struct Plan {
    pub(crate) types: Vec<EnumType>,
    pub(crate) values: Vec<(EnumRef, Result<Place, EnumPresentation>)>,
}

pub(crate) fn plan(
    enums: &[EnumRef],
    index: &HashMap<TypeName, Vec<&MdType>>,
) -> Result<Plan, AnswerError> {
    let mut known: HashMap<TypeName, Option<EnumType>> = HashMap::new();
    let mut asked: FirstSeen<TypeName, usize> = FirstSeen::new();
    let mut types = Vec::new();
    let mut seen: FirstSeen<EnumRef, Result<Place, EnumPresentation>> = FirstSeen::new();
    for e in enums {
        if seen.contains(e) {
            continue;
        }
        let state = if e.is_empty() {
            Err(EnumPresentation::Empty)
        } else {
            let ty = e.type_name();
            if !known.contains_key(ty) {
                known.insert(ty.clone(), checked(ty, index)?);
            }
            match known.get(ty).and_then(Option::as_ref) {
                None => Err(EnumPresentation::UnknownType),
                Some(t) => match t.at.get(e.name()) {
                    None => Err(EnumPresentation::UnknownValue),
                    Some(&pos) => {
                        let ti = *asked.entry(ty, || {
                            types.push(t.clone());
                            types.len() - 1
                        });
                        Ok(Place { ty: ti, pos })
                    }
                },
            }
        };
        seen.entry(e, || state);
    }
    Ok(Plan {
        types,
        values: seen.into_items(),
    })
}

pub(crate) fn values_path(clsid: Uuid, version: &str, key: Option<&str>) -> String {
    let mut p = format!("/e1cib/types/view/{clsid}/values?sysver={version}&clang=ru");
    if let Some(k) = key {
        p.push_str(&format!("&types={k}"));
    }
    p
}

pub(crate) fn parse_values(body: &[u8], names: &[String]) -> Result<Vec<String>, AnswerError> {
    let bom = [0xEF, 0xBB, 0xBF];
    if body
        .strip_prefix(&bom)
        .unwrap_or(body)
        .trim_ascii()
        .is_empty()
    {
        return Err(AnswerError::Missing {
            answer: ANSWER,
            field: "answer body".into(),
        });
    }
    let mut unrecognized = Vec::new();
    let texts = read(ANSWER, body, NS_APP, &[], &mut unrecognized, |r| {
        if let Some(a) = r.root.attributes().next() {
            return Err(r.bad("response attribute", a.name()));
        }
        let mut out = Vec::new();
        for v in r.children(NS_APP, "val") {
            if let Some(a) = v.attributes().next() {
                return Err(r.bad("val attribute", a.name()));
            }
            if let Some(c) = v.children().find(|c| c.is_element()) {
                return Err(r.bad("val content", c.tag_name().name()));
            }
            let text: String = v
                .children()
                .filter(|c| c.is_text())
                .filter_map(|c| c.text())
                .collect();
            if text.is_empty() {
                return Err(r.bad("val", ""));
            }
            out.push(text);
        }
        Ok(out)
    })?;
    if let Some(u) = unrecognized.first() {
        return Err(AnswerError::Bad {
            answer: ANSWER,
            field: "element".into(),
            value: u.name.clone(),
        });
    }
    if texts.len() != names.len() {
        return Err(AnswerError::Bad {
            answer: ANSWER,
            field: format!("number of values (types lists {})", names.len()),
            value: texts.len().to_string(),
        });
    }
    Ok(texts)
}
