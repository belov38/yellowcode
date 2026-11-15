use std::cell::RefCell;

use roxmltree::{Document, Node};

use crate::value::Uuid;

pub(crate) const NS_WS: &str = "http://v8.1c.ru/8.2/mngsrv/ws";
pub(crate) const NS_APP: &str = "http://v8.1c.ru/8.2/managed-application/core";
pub(crate) const NS_MODULES: &str = "http://v8.1c.ru/8.2/managed-application/modules";
pub(crate) const NS_CMI: &str = "http://v8.1c.ru/8.2/managed-application/cmi";
pub(crate) const NS_VRS: &str = "http://v8.1c.ru/8.2/virtual-resource-system";
pub(crate) const NS_CORE: &str = "http://v8.1c.ru/8.1/data/core";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AnswerError {
    #[error("{answer}: XML parse error: {message}")]
    Parse {
        answer: &'static str,
        message: String,
    },
    #[error("{answer}: the server answered with an exception: {descr}")]
    Exception { answer: &'static str, descr: String },
    #[error("{answer}: the root is <{root}>, not <response>")]
    NotResponse { answer: &'static str, root: String },
    #[error("{answer}: missing {field}")]
    Missing { answer: &'static str, field: String },
    #[error("{answer}: bad {field}: {value:?}")]
    Bad {
        answer: &'static str,
        field: String,
        value: String,
    },
    #[error("{answer}: {field} is repeated")]
    Duplicate { answer: &'static str, field: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unrecognized {
    pub answer: &'static str,
    pub name: String,
}

pub(crate) struct Reader<'a, 'i> {
    pub(crate) answer: &'static str,
    pub(crate) root: Node<'a, 'i>,
    used: RefCell<Vec<(&'static str, &'static str)>>,
}

pub(crate) fn read<T>(
    answer: &'static str,
    body: &[u8],
    hidden_ns: &'static str,
    hidden: &[&'static str],
    unrecognized: &mut Vec<Unrecognized>,
    f: impl FnOnce(&Reader) -> Result<T, AnswerError>,
) -> Result<T, AnswerError> {
    let text = String::from_utf8_lossy(body.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(body));
    let doc = Document::parse(&text).map_err(|e| AnswerError::Parse {
        answer,
        message: e.to_string(),
    })?;
    let root = doc.root_element();
    if is(root, NS_VRS, "exception") {
        let descr = root
            .descendants()
            .find(|n| is(*n, NS_CORE, "descr"))
            .and_then(text_of)
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
    let r = Reader {
        answer,
        root,
        used: RefCell::new(hidden.iter().map(|h| (hidden_ns, *h)).collect()),
    };
    let value = f(&r)?;
    let used = r.used.borrow();
    for n in root.children().filter(Node::is_element) {
        let name = n.tag_name();
        let seen = used
            .iter()
            .any(|(ns, local)| name.namespace() == Some(*ns) && name.name() == *local);
        if !seen {
            unrecognized.push(Unrecognized {
                answer,
                name: name.name().to_string(),
            });
        }
    }
    Ok(value)
}

impl<'a, 'i> Reader<'a, 'i> {
    pub(crate) fn missing(&self, field: &str) -> AnswerError {
        AnswerError::Missing {
            answer: self.answer,
            field: field.to_string(),
        }
    }

    pub(crate) fn bad(&self, field: &str, value: &str) -> AnswerError {
        AnswerError::Bad {
            answer: self.answer,
            field: field.to_string(),
            value: value.to_string(),
        }
    }

    pub(crate) fn children(&self, ns: &'static str, local: &'static str) -> Vec<Node<'a, 'i>> {
        self.children_of(ns, &[local])
    }

    pub(crate) fn children_of(
        &self,
        ns: &'static str,
        locals: &[&'static str],
    ) -> Vec<Node<'a, 'i>> {
        self.used
            .borrow_mut()
            .extend(locals.iter().map(|l| (ns, *l)));
        self.root
            .children()
            .filter(|n| {
                n.is_element()
                    && n.tag_name().namespace() == Some(ns)
                    && locals.contains(&n.tag_name().name())
            })
            .collect()
    }

    pub(crate) fn one(
        &self,
        ns: &'static str,
        local: &'static str,
    ) -> Result<Option<Node<'a, 'i>>, AnswerError> {
        let mut all = self.children(ns, local).into_iter();
        let first = all.next();
        if all.next().is_some() {
            return Err(AnswerError::Duplicate {
                answer: self.answer,
                field: local.to_string(),
            });
        }
        Ok(first)
    }

    pub(crate) fn text(
        &self,
        ns: &'static str,
        local: &'static str,
    ) -> Result<Option<String>, AnswerError> {
        Ok(self.one(ns, local)?.and_then(text_of))
    }

    pub(crate) fn parsed<T>(
        &self,
        ns: &'static str,
        local: &'static str,
        parse: fn(&str) -> Option<T>,
    ) -> Result<Option<T>, AnswerError> {
        self.text(ns, local)?
            .map(|t| parse(&t).ok_or_else(|| self.bad(local, &t)))
            .transpose()
    }

    pub(crate) fn attr_parsed<T>(
        &self,
        n: Node,
        name: &str,
        field: &str,
        parse: fn(&str) -> Option<T>,
    ) -> Result<Option<T>, AnswerError> {
        attr(n, name)
            .map(|t| parse(&t).ok_or_else(|| self.bad(field, &t)))
            .transpose()
    }
}

pub(crate) fn is(n: Node, ns: &str, local: &str) -> bool {
    n.is_element() && n.tag_name().namespace() == Some(ns) && n.tag_name().name() == local
}

pub(crate) fn text_of(n: Node) -> Option<String> {
    n.text().filter(|t| !t.is_empty()).map(str::to_string)
}

pub(crate) fn attr(n: Node, name: &str) -> Option<String> {
    n.attribute(name)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

pub(crate) fn boolean(t: &str) -> Option<bool> {
    match t {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

pub(crate) fn unsigned(t: &str) -> Option<u32> {
    if t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    t.parse().ok()
}

pub(crate) fn signed(t: &str) -> Option<i64> {
    let digits = t.strip_prefix('-').unwrap_or(t);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    t.parse().ok()
}

pub(crate) fn guid(t: &str) -> Option<String> {
    Uuid::parse(t).ok().map(|_| t.to_string())
}
