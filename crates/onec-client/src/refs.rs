use std::collections::{HashMap, HashSet};
use std::hash::Hash;

use roxmltree::Node;

use crate::answer::{AnswerError, NS_WS, Reader, Unrecognized, attr, is, read};
use crate::metadata::MdType;
use crate::tabular::TreeRow;
use crate::value::{Ref, TypeName, UUID_REF_KINDS, Uuid, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Presentation {
    Valid(String),
    NotFound(String),
    Blank,
    Empty,
    UnknownType,
}

#[derive(Debug, Clone)]
pub(crate) struct FirstSeen<K, V> {
    items: Vec<(K, V)>,
    at: HashMap<K, usize>,
}

impl<K: Hash + Eq + Clone, V> FirstSeen<K, V> {
    pub(crate) fn new() -> Self {
        FirstSeen {
            items: Vec::new(),
            at: HashMap::new(),
        }
    }

    pub(crate) fn entry(&mut self, k: &K, make: impl FnOnce() -> V) -> &mut V {
        let i = match self.at.get(k) {
            Some(&i) => i,
            None => {
                self.items.push((k.clone(), make()));
                self.at.insert(k.clone(), self.items.len() - 1);
                self.items.len() - 1
            }
        };
        &mut self.items[i].1
    }

    pub(crate) fn contains(&self, k: &K) -> bool {
        self.at.contains_key(k)
    }

    pub(crate) fn get(&self, k: &K) -> Option<&V> {
        self.at.get(k).map(|&i| &self.items[i].1)
    }

    pub(crate) fn items(&self) -> &[(K, V)] {
        &self.items
    }

    pub(crate) fn into_items(self) -> Vec<(K, V)> {
        self.items
    }
}

#[derive(Debug, Clone)]
pub struct Presentations {
    items: FirstSeen<Ref, Presentation>,
    unrecognized: Vec<Unrecognized>,
}

impl Presentations {
    pub fn new(items: Vec<(Ref, Presentation)>, unrecognized: Vec<Unrecognized>) -> Presentations {
        let mut index = FirstSeen::new();
        for (r, p) in items {
            *index.entry(&r, || Presentation::Blank) = p;
        }
        Presentations {
            items: index,
            unrecognized,
        }
    }

    pub fn get(&self, r: &Ref) -> Option<&Presentation> {
        self.items.get(r)
    }

    pub fn items(&self) -> &[(Ref, Presentation)] {
        self.items.items()
    }

    pub fn unrecognized(&self) -> &[Unrecognized] {
        &self.unrecognized
    }
}

pub fn collect<'v>(values: impl IntoIterator<Item = &'v Value>) -> Vec<Ref> {
    let mut out: FirstSeen<Ref, ()> = FirstSeen::new();
    walk(values, |v| {
        if let Value::Ref(r) = v {
            out.entry(r, || ());
        }
    });
    out.into_items().into_iter().map(|(r, ())| r).collect()
}

pub(crate) fn walk<'v>(values: impl IntoIterator<Item = &'v Value>, mut f: impl FnMut(&'v Value)) {
    let mut stack: Vec<&Value> = values.into_iter().collect();
    stack.reverse();
    while let Some(v) = stack.pop() {
        f(v);
        let mut inner: Vec<&Value> = Vec::new();
        match v {
            Value::Array(a) => inner.extend(a.items.iter()),
            Value::Structure(s) => inner.extend(s.props().iter().map(|(_, x)| x)),
            Value::Map(m) => {
                for (k, x) in m.pairs() {
                    inner.push(k);
                    inner.push(x);
                }
            }
            Value::ValueTable(t) => inner.extend(t.rows().iter().flatten()),
            Value::ValueTree(t) => {
                let mut rows: Vec<&TreeRow> = t.rows().iter().rev().collect();
                while let Some(row) = rows.pop() {
                    inner.extend(row.values());
                    rows.extend(row.rows().iter().rev());
                }
            }
            Value::ValueList(l) => inner.extend(l.items().iter().map(|i| &i.value)),
            _ => {}
        }
        stack.extend(inner.into_iter().rev());
    }
}

pub(crate) fn type_ids(types: &[MdType]) -> Result<HashMap<TypeName, Uuid>, AnswerError> {
    let mut out = HashMap::new();
    for t in types {
        let kind = format!("{}Ref", t.kind.en());
        if !UUID_REF_KINDS.contains(&kind.as_str()) {
            continue;
        }
        let id = Uuid::parse(&t.type_id).map_err(|_| AnswerError::Bad {
            answer: "types",
            field: "type clsid".into(),
            value: t.type_id.clone(),
        })?;
        let name = TypeName::config(&format!("{kind}.{}", t.name));
        if out.insert(name.clone(), id).is_some() {
            return Err(AnswerError::Duplicate {
                answer: "types",
                field: format!("type {name}"),
            });
        }
    }
    Ok(out)
}

pub(crate) struct Plan {
    pub(crate) groups: Vec<(Uuid, Vec<Uuid>)>,
    pub(crate) refs: Vec<(Ref, Result<Uuid, Presentation>)>,
}

pub(crate) fn plan(refs: &[Ref], ids: &HashMap<TypeName, Uuid>) -> Plan {
    let mut groups: FirstSeen<Uuid, Vec<Uuid>> = FirstSeen::new();
    let mut seen: FirstSeen<Ref, Result<Uuid, Presentation>> = FirstSeen::new();
    for r in refs {
        if seen.contains(r) {
            continue;
        }
        let state = if r.is_empty() {
            Err(Presentation::Empty)
        } else {
            match ids.get(r.type_name()) {
                None => Err(Presentation::UnknownType),
                Some(&clsid) => {
                    groups.entry(&clsid, Vec::new).push(r.uuid());
                    Ok(clsid)
                }
            }
        };
        seen.entry(r, || state);
    }
    Plan {
        groups: groups.into_items(),
        refs: seen.into_items(),
    }
}

pub(crate) const BATCH: usize = 500;

pub(crate) fn batches(groups: &[(Uuid, Vec<Uuid>)], max: usize) -> Vec<Vec<(Uuid, Vec<Uuid>)>> {
    assert!(max > 0, "batches of 0 references");
    let mut out: Vec<Vec<(Uuid, Vec<Uuid>)>> = Vec::new();
    let mut room = 0;
    for (clsid, ids) in groups {
        let mut rest = ids.as_slice();
        while !rest.is_empty() {
            if room == 0 {
                out.push(Vec::new());
                room = max;
            }
            let (now, later) = rest.split_at(room.min(rest.len()));
            room -= now.len();
            rest = later;
            if let Some(batch) = out.last_mut() {
                batch.push((*clsid, now.to_vec()));
            }
        }
    }
    out
}

pub(crate) fn build_request(groups: &[(Uuid, Vec<Uuid>)]) -> Vec<u8> {
    let mut r = String::from(
        "\u{feff}<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
<request xmlns=\"http://v8.1c.ru/8.2/mngsrv/ws\" \
xmlns:cfg=\"http://v8.1c.ru/8.1/data/enterprise/current-config\" \
xmlns:v8=\"http://v8.1c.ru/8.1/data/core\" \
xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" \
xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"><refs>",
    );
    for (clsid, ids) in groups {
        r.push_str(&format!("<item typeId=\"{clsid}\">"));
        for id in ids {
            r.push_str(&format!("<ref id=\"{id}\"/>"));
        }
        r.push_str("</item>");
    }
    r.push_str("</refs></request>");
    r.into_bytes()
}

pub(crate) fn parse_answer(
    body: &[u8],
    groups: &[(Uuid, Vec<Uuid>)],
    unrecognized: &mut Vec<Unrecognized>,
) -> Result<HashMap<(Uuid, Uuid), Presentation>, AnswerError> {
    let mut inner = Vec::new();
    let out = read("refs", body, NS_WS, &[], unrecognized, |r| {
        answer(r, groups, &mut inner)
    })?;
    unrecognized.extend(inner);
    Ok(out)
}

fn others(parent: Node, local: &str, unrecognized: &mut Vec<Unrecognized>) {
    for n in parent
        .children()
        .filter(|n| n.is_element() && !is(*n, NS_WS, local))
    {
        unrecognized.push(Unrecognized {
            answer: "refs",
            name: format!("{}/{}", parent.tag_name().name(), n.tag_name().name()),
        });
    }
}

fn answer(
    r: &Reader,
    groups: &[(Uuid, Vec<Uuid>)],
    unrecognized: &mut Vec<Unrecognized>,
) -> Result<HashMap<(Uuid, Uuid), Presentation>, AnswerError> {
    let asked: HashMap<Uuid, HashSet<Uuid>> = groups
        .iter()
        .map(|(t, ids)| (*t, ids.iter().copied().collect()))
        .collect();
    let items: Vec<Node> = match r.one(NS_WS, "refs")? {
        Some(refs) => {
            others(refs, "item", unrecognized);
            refs.children().filter(|n| is(*n, NS_WS, "item")).collect()
        }
        None => Vec::new(),
    };
    let mut out = HashMap::new();
    for item in items {
        let clsid = uuid_attr(r, item, "typeId", "item typeId")?;
        let Some(asked) = asked.get(&clsid) else {
            return Err(r.bad("item typeId (not asked)", &clsid.to_string()));
        };
        others(item, "ref", unrecognized);
        for n in item.children().filter(|n| is(*n, NS_WS, "ref")) {
            let id = uuid_attr(r, n, "id", "ref id")?;
            if !asked.contains(&id) {
                return Err(r.bad("ref id (not asked)", &id.to_string()));
            }
            let text = n.text().unwrap_or_default().to_string();
            let p = match n.attribute("valid") {
                Some("true") => Presentation::Valid(text),
                Some(other) => return Err(r.bad("ref valid", other)),
                None if text.is_empty() => Presentation::Blank,
                None => Presentation::NotFound(text),
            };
            if out.insert((clsid, id), p).is_some() {
                return Err(AnswerError::Duplicate {
                    answer: r.answer,
                    field: format!("ref {id}"),
                });
            }
        }
    }
    for (clsid, ids) in groups {
        if let Some(id) = ids.iter().find(|id| !out.contains_key(&(*clsid, **id))) {
            return Err(r.missing(&format!("ref {id} of type {clsid}")));
        }
    }
    Ok(out)
}

fn uuid_attr(r: &Reader, n: Node, name: &str, field: &str) -> Result<Uuid, AnswerError> {
    let t = attr(n, name).ok_or_else(|| r.missing(field))?;
    Uuid::parse(&t).map_err(|_| r.bad(field, &t))
}
