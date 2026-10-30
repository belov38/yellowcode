use brace::binary::{Atom, BinNode, Event, ReadLimits, Reader, Writer};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    List(Vec<Item>),
    Atom { atom: Atom, joined: bool },
}

impl Item {
    pub fn joined(atom: Atom) -> Self {
        Self::Atom { atom, joined: true }
    }

    pub fn from_node(node: &BinNode) -> Self {
        let mut result = Self::List(Vec::new());
        let mut pending = vec![(node, &mut result)];
        while let Some((source, target)) = pending.pop() {
            match source {
                BinNode::Atom(atom) => {
                    *target = Self::Atom {
                        atom: atom.clone(),
                        joined: false,
                    }
                }
                BinNode::List(children) => {
                    let mut items = Vec::new();
                    items.resize_with(children.len(), || Self::List(Vec::new()));
                    *target = Self::List(items);
                    if let Self::List(items) = target {
                        pending.extend(children.iter().zip(items.iter_mut()).rev());
                    }
                }
            }
        }
        result
    }

    pub fn to_node(&self) -> BinNode {
        let mut result = BinNode::List(Vec::new());
        let mut pending = vec![(self, &mut result)];
        while let Some((source, target)) = pending.pop() {
            match source {
                Self::Atom { atom, .. } => *target = BinNode::Atom(atom.clone()),
                Self::List(children) => {
                    let mut items = Vec::new();
                    items.resize_with(children.len(), || BinNode::List(Vec::new()));
                    *target = BinNode::List(items);
                    if let BinNode::List(items) = target {
                        pending.extend(children.iter().zip(items.iter_mut()).rev());
                    }
                }
            }
        }
        result
    }
}

#[allow(clippy::expect_used)]
pub fn encode(items: &[Item]) -> Vec<u8> {
    let write = || -> std::io::Result<Vec<u8>> {
        let mut writer = Writer::new(Vec::new());
        let mut pending = vec![items.iter()];
        while let Some(top) = pending.last_mut() {
            match top.next() {
                Some(Item::Atom { atom, joined }) => {
                    if *joined {
                        writer.atom_joined(atom)?;
                    } else {
                        writer.atom(atom)?;
                    }
                }
                Some(Item::List(children)) => {
                    writer.open()?;
                    pending.push(children.iter());
                }
                None => {
                    pending.pop();
                    if !pending.is_empty() {
                        writer.close()?;
                    }
                }
            }
        }
        writer.finish()
    };
    write().expect("balanced Item traversal into Vec is infallible")
}

fn error(offset: u64, what: impl Into<String>) -> crate::Error {
    crate::Error::Dt {
        offset,
        what: what.into(),
    }
}

pub fn decode(bytes: &[u8]) -> crate::Result<Vec<Item>> {
    let mut reader = Reader::new(bytes);
    let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    reader.begin_budget(ReadLimits {
        atom_bytes: size,
        retained_bytes: size,
        nodes: size.saturating_mul(2),
    });
    let mut parents = Vec::new();
    let mut items = Vec::new();
    loop {
        let before = reader.missing_commas();
        let event = reader
            .next_event()
            .map_err(|e| error(e.offset, e.kind.to_string()))?;
        let joined = reader.missing_commas() != before;
        match event {
            Some(Event::Open) => {
                if joined {
                    return Err(error(reader.offset(), "joined list cannot be represented"));
                }
                parents.push(std::mem::take(&mut items));
            }
            Some(Event::Close) => {
                let children = items;
                items = parents
                    .pop()
                    .ok_or_else(|| error(reader.offset(), "close without list"))?;
                items.push(Item::List(children));
            }
            Some(Event::Atom(atom)) => items.push(Item::Atom { atom, joined }),
            None => return Ok(items),
        }
    }
}
