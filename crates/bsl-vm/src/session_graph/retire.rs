use crate::{ObjRef, Value};
use std::{collections::HashMap, rc::Rc};

pub trait SessionBacking {
    fn session_owner(&self) -> Option<(usize, usize)> {
        None
    }
    fn session_edges(&self, visit: &mut dyn SessionEdges) -> bool;
    fn session_clear(&self) {}
}

pub trait SessionEdges {
    fn object(&mut self, object: &ObjRef) -> bool;
    fn backing(&mut self, backing: Rc<dyn SessionBacking>) -> bool;
    fn value(&mut self, value: &Value) -> bool {
        match value {
            Value::Object(object) => self.object(object),
            _ => true,
        }
    }
}

enum Owner {
    Object(ObjRef),
    Backing(Rc<dyn SessionBacking>),
}
impl Owner {
    fn id(&self) -> usize {
        match self {
            Self::Object(value) => Rc::as_ptr(value).cast::<()>() as usize,
            Self::Backing(value) => value
                .session_owner()
                .map_or_else(|| Rc::as_ptr(value).cast::<()>() as usize, |owner| owner.0),
        }
    }
    fn count(&self) -> usize {
        match self {
            Self::Object(value) => Rc::strong_count(value),
            Self::Backing(value) => value
                .session_owner()
                .map_or_else(|| Rc::strong_count(value), |owner| owner.1),
        }
    }
    fn edges(&self, visit: &mut dyn SessionEdges) -> bool {
        match self {
            Self::Object(value) => value.session_edges(visit),
            Self::Backing(value) => value.session_edges(visit),
        }
    }
    fn clear(&self) {
        match self {
            Self::Object(value) => value.session_clear(),
            Self::Backing(value) => value.session_clear(),
        }
    }
}
struct Node {
    owner: Option<Owner>,
    incoming: usize,
    edges: Vec<usize>,
    live: bool,
}
struct Walk {
    nodes: Vec<Node>,
    ids: HashMap<usize, usize>,
    current: Option<usize>,
    bytes: u64,
    limit: Option<u64>,
    failed: bool,
}
impl Walk {
    fn reserve(&mut self, bytes: u64) -> bool {
        self.bytes = self.bytes.saturating_add(bytes);
        let allowed = self
            .limit
            .is_none_or(|limit| self.bytes.saturating_mul(2) <= limit);
        self.failed |= !allowed;
        allowed
    }
    fn edge(&mut self, owner: Owner) -> bool {
        let id = owner.id();
        let index = if let Some(&index) = self.ids.get(&id) {
            index
        } else {
            if !self.reserve(256)
                || self.nodes.try_reserve_exact(1).is_err()
                || self.ids.try_reserve(1).is_err()
            {
                self.failed = true;
                return false;
            }
            let index = self.nodes.len();
            self.nodes.push(Node {
                owner: Some(owner),
                incoming: 0,
                edges: Vec::new(),
                live: false,
            });
            self.ids.insert(id, index);
            index
        };
        self.nodes[index].incoming = self.nodes[index].incoming.saturating_add(1);
        if let Some(parent) = self.current {
            if !self.reserve(std::mem::size_of::<usize>() as u64) {
                return false;
            }
            let edges = &mut self.nodes[parent].edges;
            if edges.try_reserve_exact(1).is_err() {
                self.failed = true;
                return false;
            }
            edges.push(index);
        }
        true
    }
}
impl SessionEdges for Walk {
    fn object(&mut self, object: &ObjRef) -> bool {
        self.edge(Owner::Object(object.clone()))
    }
    fn backing(&mut self, backing: Rc<dyn SessionBacking>) -> bool {
        self.edge(Owner::Backing(backing))
    }
}

pub(crate) fn acyclic(root: &dyn crate::HostObject, limit: Option<u64>) -> Option<u64> {
    let mut walk = Walk {
        nodes: Vec::new(),
        ids: HashMap::new(),
        current: Some(0),
        bytes: 0,
        limit,
        failed: false,
    };
    if !walk.reserve(256)
        || walk.nodes.try_reserve_exact(1).is_err()
        || walk.ids.try_reserve(1).is_err()
    {
        return None;
    }
    walk.ids
        .insert(std::ptr::from_ref(root).cast::<()>() as usize, 0);
    walk.nodes.push(Node {
        owner: None,
        incoming: 0,
        edges: Vec::new(),
        live: false,
    });
    if !root.session_edges(&mut walk) || walk.failed {
        return None;
    }
    let mut cursor = 1;
    while cursor < walk.nodes.len() {
        walk.current = Some(cursor);
        let owner = walk.nodes[cursor].owner.take()?;
        let supported = owner.edges(&mut walk);
        walk.nodes[cursor].owner = Some(owner);
        if !supported || walk.failed {
            return None;
        }
        cursor += 1;
    }
    let mut ready = Vec::new();
    if !walk.reserve((walk.nodes.len() as u64).saturating_mul(std::mem::size_of::<usize>() as u64))
        || ready.try_reserve_exact(walk.nodes.len()).is_err()
    {
        return None;
    }
    let mut work = walk.nodes.len() as u64;
    for (index, node) in walk.nodes.iter().enumerate() {
        if node.incoming == 0 {
            ready.push(index);
        }
        work = work.saturating_add(node.edges.len() as u64);
    }
    let mut visited = 0;
    while let Some(index) = ready.pop() {
        visited += 1;
        for edge in 0..walk.nodes[index].edges.len() {
            let target = walk.nodes[index].edges[edge];
            walk.nodes[target].incoming -= 1;
            if walk.nodes[target].incoming == 0 {
                ready.push(target);
            }
        }
    }
    (visited == walk.nodes.len()).then_some(work)
}

pub(crate) fn release<'a>(roots: impl Iterator<Item = &'a Value>, limit: Option<u64>) {
    let mut walk = Walk {
        nodes: Vec::new(),
        ids: HashMap::new(),
        current: None,
        bytes: 0,
        limit,
        failed: false,
    };
    for root in roots {
        if !walk.value(root) {
            return;
        }
    }
    let mut cursor = 0;
    while cursor < walk.nodes.len() {
        walk.current = Some(cursor);
        let Some(owner) = walk.nodes[cursor].owner.take() else {
            return;
        };
        let supported = owner.edges(&mut walk);
        walk.nodes[cursor].owner = Some(owner);
        if walk.failed {
            return;
        }
        walk.nodes[cursor].live = !supported;
        cursor += 1;
    }
    let mut pending = Vec::new();
    if !walk.reserve((walk.nodes.len() * std::mem::size_of::<usize>()) as u64)
        || pending.try_reserve_exact(walk.nodes.len()).is_err()
    {
        return;
    }
    for (index, node) in walk.nodes.iter_mut().enumerate() {
        if node.live
            || node
                .owner
                .as_ref()
                .is_some_and(|owner| owner.count() > node.incoming + 1)
        {
            node.live = true;
            pending.push(index);
        }
    }
    while let Some(index) = pending.pop() {
        for edge in 0..walk.nodes[index].edges.len() {
            let next = walk.nodes[index].edges[edge];
            if !walk.nodes[next].live {
                walk.nodes[next].live = true;
                pending.push(next);
            }
        }
    }
    for node in &walk.nodes {
        if !node.live
            && let Some(owner) = &node.owner
        {
            owner.clear();
        }
    }
}
