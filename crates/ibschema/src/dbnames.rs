use std::collections::HashMap;

use brace::Node;

use crate::error::{Error, Result};
use crate::values::Uuid16;

mod managed;

const WHAT: &str = "DBNames";
const BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbName {
    pub uuid: Uuid16,
    pub purpose: String,
    pub number: u32,
}

#[derive(Debug, Clone, Default)]
pub struct DbNames {
    counter: u32,
    entries: Vec<DbName>,
    lookup: Lookup,
    top: u32,
}

#[derive(Debug, Clone)]
enum Lookup {
    Native {
        by_key: HashMap<(Uuid16, String), usize>,
        by_num: HashMap<u32, usize>,
    },
    Managed {
        by_key: Vec<usize>,
        by_num: Vec<usize>,
    },
}
impl Default for Lookup {
    fn default() -> Self {
        Self::Native {
            by_key: HashMap::new(),
            by_num: HashMap::new(),
        }
    }
}
fn entry_key(entries: &[DbName], i: usize) -> Option<(Uuid16, &str)> {
    entries
        .get(i)
        .map(|entry| (entry.uuid, entry.purpose.as_str()))
}
fn entry_number(entries: &[DbName], i: usize) -> Option<u32> {
    entries.get(i).map(|entry| entry.number)
}

fn perr(at: impl Into<String>, why: impl Into<String>) -> Error {
    Error::Parse {
        what: WHAT,
        at: at.into(),
        why: why.into(),
    }
}

fn plain_u32(n: &Node) -> Option<u32> {
    u32::try_from(crate::canonical_i64(n.as_atom()?)?).ok()
}

fn parse_root(text: &[u8]) -> Result<Node> {
    brace::parse_bytes(text).map_err(|e| perr("file", e.to_string()))
}

impl DbNames {
    pub fn parse(text: &[u8]) -> Result<DbNames> {
        let root = parse_root(text)?;
        let top = root
            .as_list()
            .ok_or_else(|| perr("file", "root is not a list"))?;
        let (c, body) = match top {
            [c, body] => (c, body),
            _ => return Err(perr("file", "expected {counter,{n,...}}")),
        };
        let counter = plain_u32(c).ok_or_else(|| perr("file", "counter is not a number"))?;
        let items = body
            .as_list()
            .ok_or_else(|| perr("file", "entries are not a list"))?;
        let (n, rows) = items
            .split_first()
            .ok_or_else(|| perr("file", "entries list is empty"))?;
        let n = plain_u32(n).ok_or_else(|| perr("file", "entry count is not a number"))?;
        if usize::try_from(n).ok() != Some(rows.len()) {
            return Err(perr(
                "file",
                format!("entry count {n} but {} entries", rows.len()),
            ));
        }
        let mut out = DbNames {
            counter,
            ..DbNames::default()
        };
        for (i, row) in rows.iter().enumerate() {
            let at = format!("entries[{i}]");
            let e = match row.as_list() {
                Some([u, p, num]) => {
                    let uuid = u.as_guid().ok_or_else(|| perr(&at, "bad uuid"))?;
                    let purpose = p
                        .as_str()
                        .ok_or_else(|| perr(&at, "purpose is not a string"))?;
                    let number = plain_u32(num).ok_or_else(|| perr(&at, "bad number"))?;
                    DbName {
                        uuid,
                        purpose: purpose.to_string(),
                        number,
                    }
                }
                _ => return Err(perr(&at, "expected {uuid,\"purpose\",number}")),
            };
            out.insert(e).map_err(|why| perr(&at, why))?;
        }
        Ok(out)
    }

    fn insert(&mut self, e: DbName) -> std::result::Result<(), String> {
        let idx = self.entries.len();
        match &mut self.lookup {
            Lookup::Native { by_key, by_num } => {
                let key = (e.uuid, e.purpose.clone());
                if by_key.contains_key(&key) {
                    return Err(format!("duplicate key {} {:?}", e.uuid, e.purpose));
                }
                if by_num.contains_key(&e.number) {
                    return Err(format!("duplicate number {}", e.number));
                }
                by_key.insert(key, idx);
                by_num.insert(e.number, idx);
                self.top = self.top.max(e.number);
                self.entries.push(e);
            }
            Lookup::Managed { by_key, by_num } => {
                if by_key
                    .binary_search_by(|&i| {
                        entry_key(&self.entries, i).cmp(&Some((e.uuid, e.purpose.as_str())))
                    })
                    .is_ok()
                {
                    return Err(format!("duplicate key {} {:?}", e.uuid, e.purpose));
                }
                if by_num
                    .binary_search_by(|&i| entry_number(&self.entries, i).cmp(&Some(e.number)))
                    .is_ok()
                {
                    return Err(format!("duplicate number {}", e.number));
                }
                self.top = self.top.max(e.number);
                self.entries.push(e);
                by_key.push(idx);
                by_num.push(idx);
                by_key.sort_unstable_by(|&a, &b| {
                    entry_key(&self.entries, a).cmp(&entry_key(&self.entries, b))
                });
                by_num.sort_unstable_by_key(|&i| entry_number(&self.entries, i));
            }
        }
        Ok(())
    }

    pub fn to_text(&self) -> Vec<u8> {
        let mut rows = vec![Node::atom(self.entries.len().to_string())];
        rows.extend(self.entries.iter().map(|e| {
            Node::list(vec![
                Node::guid(e.uuid),
                Node::str(e.purpose.clone()),
                Node::atom(e.number.to_string()),
            ])
        }));
        let root = Node::list(vec![Node::atom(self.counter.to_string()), Node::list(rows)]);
        let mut out = BOM.to_vec();
        out.extend_from_slice(brace::to_text(&root).as_bytes());
        out
    }

    pub fn counter(&self) -> u32 {
        self.counter
    }

    pub fn entries(&self) -> &[DbName] {
        &self.entries
    }

    pub fn number(&self, uuid: Uuid16, purpose: &str) -> Option<u32> {
        let i = match &self.lookup {
            Lookup::Native { by_key, .. } => *by_key.get(&(uuid, purpose.to_string()))?,
            Lookup::Managed { by_key, .. } => {
                let found = by_key
                    .binary_search_by(|&i| entry_key(&self.entries, i).cmp(&Some((uuid, purpose))))
                    .ok()?;
                *by_key.get(found)?
            }
        };
        self.entries.get(i).map(|e| e.number)
    }

    pub fn by_number(&self, n: u32) -> Option<&DbName> {
        let i = match &self.lookup {
            Lookup::Native { by_num, .. } => *by_num.get(&n)?,
            Lookup::Managed { by_num, .. } => {
                let found = by_num
                    .binary_search_by(|&i| entry_number(&self.entries, i).cmp(&Some(n)))
                    .ok()?;
                *by_num.get(found)?
            }
        };
        self.entries.get(i)
    }

    pub fn push(&mut self, e: DbName) -> Result<()> {
        let number = e.number;
        self.insert(e)
            .map_err(|why| perr(format!("entries[{}]", self.entries.len()), why))?;
        self.counter = self.counter.max(number);
        Ok(())
    }

    pub fn allocate(&mut self, uuid: Uuid16, purpose: &str) -> Result<u32> {
        if let Some(n) = self.number(uuid, purpose) {
            return Ok(n);
        }
        let number = self
            .counter
            .max(self.top)
            .checked_add(1)
            .ok_or_else(|| Error::Unsupported("DBNames number counter overflow".into()))?;
        self.push(DbName {
            uuid,
            purpose: purpose.to_string(),
            number,
        })?;
        Ok(number)
    }
}

pub fn parse_version(text: &[u8]) -> Result<Uuid16> {
    let root = parse_root(text)?;
    match root.as_list() {
        Some([z, u]) if z.as_atom() == Some("0") => {
            u.as_guid().ok_or_else(|| perr("file", "bad version uuid"))
        }
        _ => Err(perr("file", "expected {0,<uuid>}")),
    }
}

pub fn version_text(u: Uuid16) -> Vec<u8> {
    let root = Node::list(vec![Node::atom("0"), Node::guid(u)]);
    let mut out = BOM.to_vec();
    out.extend_from_slice(brace::to_text(&root).as_bytes());
    out
}
