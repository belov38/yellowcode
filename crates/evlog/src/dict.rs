use std::collections::{HashMap, HashSet};

use brace::Node;

use crate::{Error, Event, Meta, Uuid16};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DictKey {
    User(Uuid16, String),
    Computer(String),
    App(String),
    Event(String),
    Meta(Uuid16, String),
    Server(String),
    MainPort(u32),
    AuxPort(u32),
    Separator(Uuid16, String),
    SeparatorValue(u32, Node),
}

impl DictKey {
    pub fn kind(&self) -> u8 {
        match self {
            DictKey::User(..) => 1,
            DictKey::Computer(_) => 2,
            DictKey::App(_) => 3,
            DictKey::Event(_) => 4,
            DictKey::Meta(..) => 5,
            DictKey::Server(_) => 6,
            DictKey::MainPort(_) => 7,
            DictKey::AuxPort(_) => 8,
            DictKey::Separator(..) => 9,
            DictKey::SeparatorValue(..) => 10,
        }
    }
}

const KINDS: usize = 9;

#[derive(Debug, Default, Clone)]
pub struct Dicts {
    by_idx: [Vec<Option<DictKey>>; KINDS],
    by_key: [HashMap<DictKey, u32>; KINDS],
    values: Vec<Vec<Option<Node>>>,
    value_keys: HashMap<(u32, Node), u32>,
    user_marks: HashSet<(String, u32)>,
    computer_marks: HashSet<(String, u32)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventIdx {
    pub user: u32,
    pub computer: u32,
    pub app: u32,
    pub event: u32,
    pub metadata: u32,
    pub meta_array: Vec<u32>,
    pub server: u32,
    pub main_port: u32,
    pub aux_port: u32,
    pub separators: Node,
}

impl Default for EventIdx {
    fn default() -> Self {
        EventIdx {
            user: 0,
            computer: 0,
            app: 0,
            event: 0,
            metadata: 0,
            meta_array: Vec::new(),
            server: 0,
            main_port: 0,
            aux_port: 0,
            separators: Node::list(vec![Node::int(0)]),
        }
    }
}

fn slot(kind: u8) -> Option<usize> {
    let k = usize::from(kind);
    (1..=KINDS).contains(&k).then(|| k - 1)
}

impl Dicts {
    pub fn get(&self, k: &DictKey) -> Option<u32> {
        if let DictKey::SeparatorValue(s, v) = k {
            return self.value_keys.get(&(*s, v.clone())).copied();
        }
        self.by_key.get(slot(k.kind())?)?.get(k).copied()
    }

    pub fn key(&self, kind: u8, idx: u32) -> Option<&DictKey> {
        let i = usize::try_from(idx).ok()?.checked_sub(1)?;
        self.by_idx.get(slot(kind)?)?.get(i)?.as_ref()
    }

    pub fn value(&self, sep: u32, idx: u32) -> Option<&Node> {
        let s = usize::try_from(sep).ok()?.checked_sub(1)?;
        let i = usize::try_from(idx).ok()?.checked_sub(1)?;
        self.values.get(s)?.get(i)?.as_ref()
    }

    pub fn insert(&mut self, k: DictKey) -> u32 {
        self.insert_new(k).0
    }

    fn insert_new(&mut self, k: DictKey) -> (u32, bool) {
        if let Some(i) = self.get(&k) {
            return (i, false);
        }
        if let DictKey::SeparatorValue(sep, v) = k {
            return self.insert_value(sep, v);
        }
        let Some(s) = slot(k.kind()) else {
            return (0, false);
        };
        let (Some(v), Some(m)) = (self.by_idx.get_mut(s), self.by_key.get_mut(s)) else {
            return (0, false);
        };
        let idx = u32::try_from(v.len() + 1).unwrap_or(u32::MAX);
        v.push(Some(k.clone()));
        m.insert(k, idx);
        (idx, true)
    }

    fn insert_value(&mut self, sep: u32, v: Node) -> (u32, bool) {
        if let Some(i) = self.value_keys.get(&(sep, v.clone())) {
            return (*i, false);
        }
        let known = self.by_idx.get(8).map_or(0, Vec::len);
        let Some(s) = usize::try_from(sep).ok().and_then(|s| s.checked_sub(1)) else {
            return (0, false);
        };
        if s >= known {
            return (0, false);
        }
        if self.values.len() <= s {
            self.values.resize(s + 1, Vec::new());
        }
        let Some(list) = self.values.get_mut(s) else {
            return (0, false);
        };
        let idx = u32::try_from(list.len() + 1).unwrap_or(u32::MAX);
        list.push(Some(v.clone()));
        self.value_keys.insert((sep, v), idx);
        (idx, true)
    }

    pub fn resolve(&mut self, e: &Event) -> (EventIdx, Vec<Node>) {
        let mut new = Vec::new();
        let mut ix = EventIdx::default();
        let mut add = |d: &mut Dicts, k: DictKey| -> (u32, bool) {
            let (i, fresh) = d.insert_new(k.clone());
            if fresh {
                new.push(dict_node(&k, i));
            }
            (i, fresh)
        };
        let mut new_user = None;
        if let Some(u) = &e.user {
            let (i, fresh) = add(self, DictKey::User(u.uuid, u.name.clone()));
            ix.user = i;
            if fresh {
                new_user = Some(i);
            }
        }
        if !e.computer.is_empty() {
            ix.computer = add(self, DictKey::Computer(e.computer.clone())).0;
        }
        if !e.app.is_empty() {
            ix.app = add(self, DictKey::App(e.app.clone())).0;
        }
        if !e.server.is_empty() {
            ix.server = add(self, DictKey::Server(e.server.clone())).0;
        }
        if e.main_port != 0 {
            ix.main_port = add(self, DictKey::MainPort(e.main_port)).0;
        }
        if e.aux_port != 0 {
            ix.aux_port = add(self, DictKey::AuxPort(e.aux_port)).0;
        }
        if !e.event.is_empty() {
            ix.event = add(self, DictKey::Event(e.event.clone())).0;
        }
        match &e.metadata {
            Meta::None => {}
            Meta::One(m) => ix.metadata = add(self, DictKey::Meta(m.uuid, m.name.clone())).0,
            Meta::Many(v) => {
                for m in v {
                    let i = add(self, DictKey::Meta(m.uuid, m.name.clone())).0;
                    ix.meta_array.push(i);
                }
            }
        }
        let mut pairs = Vec::with_capacity(e.separators.len());
        for p in &e.separators {
            let sep = add(
                self,
                DictKey::Separator(p.separator.uuid, p.separator.name.clone()),
            )
            .0;
            let val = add(self, DictKey::SeparatorValue(sep, p.value.clone())).0;
            pairs.push((sep, val));
        }
        ix.separators = separators_field(pairs);
        if !e.separators.is_empty() {
            let set = brace::to_text(&ix.separators);
            if ix.user != 0 && self.user_marks.insert((set.clone(), ix.user)) {
                new.push(mark_node(11, &ix.separators, ix.user));
            }
            if ix.computer != 0 && self.computer_marks.insert((set, ix.computer)) {
                new.push(mark_node(12, &ix.separators, ix.computer));
            }
        }
        if let Some(i) = new_user {
            new.push(Node::list(vec![
                Node::int(13),
                Node::int(1),
                Node::int(i64::from(i)),
            ]));
        }
        (ix, new)
    }

    pub fn from_lgf_records(nodes: &[Node]) -> Result<Dicts, Error> {
        Self::build(nodes, 0)
    }

    pub fn from_lgf_records_with_gaps(nodes: &[Node], lost: u64) -> Result<Dicts, Error> {
        Self::build(nodes, lost)
    }

    fn build(nodes: &[Node], mut lost: u64) -> Result<Dicts, Error> {
        let mut d = Dicts::default();
        for n in nodes {
            d.apply(n, &mut lost)?;
        }
        Ok(d)
    }

    pub(crate) fn apply(&mut self, n: &Node, lost: &mut u64) -> Result<(), Error> {
        let d = self;
        let bad = || Error::Malformed(format!("bad .lgf record: {}", brace::to_text(n)));
        let items = n.as_list().ok_or_else(bad)?;
        let kind = items
            .first()
            .and_then(Node::as_u32)
            .and_then(|k| u8::try_from(k).ok())
            .ok_or_else(bad)?;
        if kind == 13 {
            return Ok(());
        }
        if kind == 11 || kind == 12 {
            let (set, who) = match items {
                [_, set, who] if set.is_list() => (set, who.as_u32().ok_or_else(bad)?),
                _ => return Err(bad()),
            };
            let marks = if kind == 11 {
                &mut d.user_marks
            } else {
                &mut d.computer_marks
            };
            marks.insert((brace::to_text(set), who));
            return Ok(());
        }
        let idx = items.last().and_then(Node::as_u32).ok_or_else(bad)?;
        let text = |i: usize| items.get(i).and_then(Node::text).ok_or_else(bad);
        let key = match (kind, items.len()) {
            (1 | 5 | 9, 4) => {
                let uuid = items.get(1).and_then(Node::as_guid).ok_or_else(bad)?;
                let name = items
                    .get(2)
                    .and_then(Node::text)
                    .ok_or_else(bad)?
                    .to_owned();
                match kind {
                    1 => DictKey::User(uuid, name),
                    5 => DictKey::Meta(uuid, name),
                    _ => DictKey::Separator(uuid, name),
                }
            }
            (2, 3) => DictKey::Computer(text(1)?.to_owned()),
            (3, 3) => DictKey::App(text(1)?.to_owned()),
            (4, 3) => DictKey::Event(text(1)?.to_owned()),
            (6, 3) => DictKey::Server(text(1)?.to_owned()),
            (7 | 8, 3) => {
                let p = items.get(1).and_then(Node::as_u32).ok_or_else(bad)?;
                if kind == 7 {
                    DictKey::MainPort(p)
                } else {
                    DictKey::AuxPort(p)
                }
            }
            (10, 4) => {
                let sep = items.get(2).and_then(Node::as_u32).ok_or_else(bad)?;
                let value = items.get(1).cloned().ok_or_else(bad)?;
                let known = d.by_idx.get(8).map_or(0, Vec::len) as u64;
                if sep == 0 || u64::from(sep) > known.saturating_add(*lost) {
                    return Err(bad());
                }
                let s = usize::try_from(sep - 1).map_err(|_| bad())?;
                if d.values.len() <= s {
                    d.values.resize(s + 1, Vec::new());
                }
                let list = d.values.get_mut(s).ok_or_else(bad)?;
                let skip = u64::from(idx).saturating_sub(list.len() as u64 + 1);
                *lost = lost.checked_sub(skip).ok_or_else(bad)?;
                let at = list
                    .len()
                    .saturating_add(usize::try_from(skip).map_err(|_| bad())?);
                if idx == 0 || u64::from(idx) != at as u64 + 1 {
                    return Err(bad());
                }
                if d.value_keys.contains_key(&(sep, value.clone())) {
                    return Err(bad());
                }
                list.resize(at, None);
                list.push(Some(value.clone()));
                d.value_keys.insert((sep, value), idx);
                return Ok(());
            }
            _ => return Err(bad()),
        };
        if let Some(v) = slot(kind).and_then(|s| d.by_idx.get_mut(s)) {
            let skip = u64::from(idx).saturating_sub(v.len() as u64 + 1);
            *lost = lost.checked_sub(skip).ok_or_else(bad)?;
            v.resize(
                v.len()
                    .saturating_add(usize::try_from(skip).map_err(|_| bad())?),
                None,
            );
        }
        let (got, fresh) = d.insert_new(key);
        if !fresh || got != idx {
            return Err(bad());
        }
        Ok(())
    }

    #[cfg(feature = "testdata")]
    pub(crate) fn preview(&self, e: &Event) -> Vec<Node> {
        match self.known_idx(e) {
            Some(ix) => {
                let mut new = Vec::new();
                if !e.separators.is_empty() {
                    let set = brace::to_text(&ix.separators);
                    if ix.user != 0 && !self.user_marks.contains(&(set.clone(), ix.user)) {
                        new.push(mark_node(11, &ix.separators, ix.user));
                    }
                    if ix.computer != 0 && !self.computer_marks.contains(&(set, ix.computer)) {
                        new.push(mark_node(12, &ix.separators, ix.computer));
                    }
                }
                new
            }
            None => self.clone().resolve(e).1,
        }
    }

    #[cfg(feature = "testdata")]
    fn known_idx(&self, e: &Event) -> Option<EventIdx> {
        let mut ix = EventIdx::default();
        if let Some(u) = &e.user {
            ix.user = self.get(&DictKey::User(u.uuid, u.name.clone()))?;
        }
        if !e.computer.is_empty() {
            ix.computer = self.get(&DictKey::Computer(e.computer.clone()))?;
        }
        if !e.app.is_empty() {
            self.get(&DictKey::App(e.app.clone()))?;
        }
        if !e.server.is_empty() {
            self.get(&DictKey::Server(e.server.clone()))?;
        }
        if e.main_port != 0 {
            self.get(&DictKey::MainPort(e.main_port))?;
        }
        if e.aux_port != 0 {
            self.get(&DictKey::AuxPort(e.aux_port))?;
        }
        if !e.event.is_empty() {
            self.get(&DictKey::Event(e.event.clone()))?;
        }
        match &e.metadata {
            Meta::None => {}
            Meta::One(m) => {
                self.get(&DictKey::Meta(m.uuid, m.name.clone()))?;
            }
            Meta::Many(v) => {
                for m in v {
                    self.get(&DictKey::Meta(m.uuid, m.name.clone()))?;
                }
            }
        }
        let mut pairs = Vec::with_capacity(e.separators.len());
        for p in &e.separators {
            let sep = self.get(&DictKey::Separator(
                p.separator.uuid,
                p.separator.name.clone(),
            ))?;
            let val = self.get(&DictKey::SeparatorValue(sep, p.value.clone()))?;
            pairs.push((sep, val));
        }
        ix.separators = separators_field(pairs);
        Some(ix)
    }
}

pub fn dict_node(k: &DictKey, idx: u32) -> Node {
    let i = Node::int(i64::from(idx));
    let kind = Node::int(i64::from(k.kind()));
    let items = match k {
        DictKey::User(u, n) | DictKey::Meta(u, n) | DictKey::Separator(u, n) => {
            vec![kind, Node::guid(*u), Node::str(n.clone()), i]
        }
        DictKey::Computer(s) | DictKey::App(s) | DictKey::Event(s) | DictKey::Server(s) => {
            vec![kind, Node::str(s.clone()), i]
        }
        DictKey::MainPort(p) | DictKey::AuxPort(p) => vec![kind, Node::int(i64::from(*p)), i],
        DictKey::SeparatorValue(s, v) => vec![kind, v.clone(), Node::int(i64::from(*s)), i],
    };
    Node::list(items)
}

fn mark_node(kind: u8, set: &Node, who: u32) -> Node {
    Node::list(vec![
        Node::int(i64::from(kind)),
        set.clone(),
        Node::int(i64::from(who)),
    ])
}

fn separators_field(mut pairs: Vec<(u32, u32)>) -> Node {
    pairs.sort_by_key(|&(sep, _)| sep);
    let mut field = vec![Node::int(pairs.len() as i64)];
    for (sep, val) in pairs {
        field.push(Node::int(i64::from(sep)));
        field.push(Node::int(i64::from(val)));
    }
    Node::list(field)
}
