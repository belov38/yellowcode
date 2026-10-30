use super::*;
use crate::session_graph::Graph as TransferGraph;
use std::time::Duration;

enum Tag {
    Value(usize),
    Structure(usize),
    Field(String),
}
struct Stored {
    module: Weak<CompiledModule>,
    method: u32,
    atoms: Vec<Tag>,
    value: usize,
    expires: Duration,
}

#[derive(Default)]
pub struct ReuseStore {
    entries: Vec<Stored>,
    graph: Option<TransferGraph>,
    bytes: u64,
    pub(crate) limit: Option<u64>,
}
impl ReuseStore {
    pub fn clear(&mut self) {
        self.entries = Vec::new();
        self.graph = None;
        self.bytes = 0;
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn retained_bytes(&self) -> u64 {
        self.bytes
    }

    fn retained_memory(&self, visit: &mut dyn crate::RetainedMemory) -> Result<(), Exception> {
        visit.bytes((self.entries.capacity() * size_of::<Stored>()) as u64)?;
        for entry in &self.entries {
            visit.allocation(
                entry.module.as_ptr() as usize,
                crate::session_graph::session_shared_bytes::<CompiledModule>(),
            )?;
            visit.bytes((entry.atoms.capacity() * size_of::<Tag>()) as u64)?;
            for atom in &entry.atoms {
                visit.work(1)?;
                if let Tag::Field(name) = atom {
                    visit.bytes(name.capacity() as u64)?;
                }
            }
        }
        if let Some(graph) = &self.graph {
            graph.retained_memory(visit)?;
        }
        Ok(())
    }
}

fn save(ex: &mut Execution) -> Result<ReuseStore, Exception> {
    let mut entries = Vec::new();
    let mut roots = Vec::new();
    let now = ex.env().reuse_clock();
    let mut bytes = 0u64;
    let mut work = 0u64;
    for (key, entry) in &ex.request_reuse.entries {
        let Some(expires) = entry.expires.filter(|deadline| now < *deadline) else {
            continue;
        };
        let (supported, spent) = memory::transferable(ex, &entry.value);
        work = work.saturating_add(spent);
        ex.check_charge(work)?;
        if !supported? {
            continue;
        }
        bytes = bytes.saturating_add(
            (size_of::<Stored>()
                + size_of::<Value>()
                + key.atoms.len() * (size_of::<Tag>() + size_of::<Value>())) as u64,
        );
        reserve(ex, bytes.saturating_mul(2))?;
        entries
            .try_reserve_exact(1)
            .map_err(|_| ex.out_of_memory())?;
        roots
            .try_reserve_exact(key.atoms.len() + 1)
            .map_err(|_| ex.out_of_memory())?;
        let mut atoms = Vec::new();
        atoms
            .try_reserve_exact(key.atoms.len())
            .map_err(|_| ex.out_of_memory())?;
        for atom in &key.atoms {
            let value = match atom {
                Atom::Structure(n) => {
                    atoms.push(Tag::Structure(*n));
                    continue;
                }
                Atom::Field(name) => {
                    bytes = bytes.saturating_add(name.len() as u64);
                    reserve(ex, bytes.saturating_mul(2))?;
                    let mut copy = String::new();
                    copy.try_reserve_exact(name.len())
                        .map_err(|_| ex.out_of_memory())?;
                    copy.push_str(name);
                    atoms.push(Tag::Field(copy));
                    continue;
                }
                Atom::Undefined => Value::Undefined,
                Atom::Null => Value::Null,
                Atom::Bool(v) => Value::Bool(*v),
                Atom::Date(v) => Value::Date(*v),
                Atom::String(v) => Value::String(v.clone()),
                Atom::Number(v) => {
                    bytes = bytes.saturating_add(v.size_hint_limbs().saturating_mul(4));
                    reserve(ex, bytes.saturating_mul(2))?;
                    Value::Number(
                        v.try_clone_with(|_| Ok::<_, ()>(()))
                            .map_err(|_| ex.out_of_memory())?,
                    )
                }
            };
            atoms.push(Tag::Value(roots.len()));
            roots.push(value);
        }
        let value = roots.len();
        roots.push(copy_value(ex, &entry.value)?);
        entries.push(Stored {
            module: key.module.clone(),
            method: key.method,
            atoms,
            value,
            expires,
        });
    }
    ex.charge(work)?;
    if entries.is_empty() {
        return Ok(ReuseStore {
            limit: memory_limit(ex),
            ..Default::default()
        });
    }
    let Some(graph) = TransferGraph::export(ex, &roots, bytes)? else {
        return Ok(ReuseStore {
            limit: memory_limit(ex),
            ..Default::default()
        });
    };
    let mut store = ReuseStore {
        entries,
        graph: Some(graph),
        bytes: 0,
        limit: memory_limit(ex),
    };
    let scratch =
        (roots.capacity() * size_of::<Value>()) as u64 + roots.iter().map(value_bytes).sum::<u64>();
    memory::transfer(ex, scratch, true, |visit| store.retained_memory(visit))?;
    store.bytes = memory::transfer(ex, scratch, false, |visit| store.retained_memory(visit))?;
    Ok(store)
}

pub(crate) fn detach(ex: &mut Execution) -> ReuseStore {
    save(ex).unwrap_or_else(|_| ReuseStore {
        limit: memory_limit(ex),
        ..Default::default()
    })
}

fn load(ex: &mut Execution, store: &ReuseStore) -> Result<(), Exception> {
    let Some(graph) = &store.graph else {
        return Ok(());
    };
    let now = ex.env().reuse_clock();
    if store.entries.iter().all(|entry| now >= entry.expires) {
        return Ok(());
    }
    let mut extra = store.bytes.saturating_sub(graph.bytes).saturating_add(
        (store.entries.len().max(3) as u64)
            .saturating_mul(ENTRY_BYTES)
            .saturating_mul(2),
    );
    for entry in &store.entries {
        extra = extra.saturating_add((entry.atoms.len() * size_of::<Atom>()) as u64);
        for atom in &entry.atoms {
            if let Tag::Field(name) = atom {
                extra = extra.saturating_add(name.len() as u64);
            }
        }
    }
    let roots = graph.restore(ex, extra)?;
    ex.request_reuse
        .entries
        .try_reserve(store.entries.len())
        .map_err(|_| ex.out_of_memory())?;
    for entry in &store.entries {
        if now >= entry.expires {
            continue;
        }
        let mut atoms = Vec::new();
        atoms
            .try_reserve_exact(entry.atoms.len())
            .map_err(|_| ex.out_of_memory())?;
        for atom in &entry.atoms {
            atoms.push(match atom {
                Tag::Structure(n) => Atom::Structure(*n),
                Tag::Field(name) => {
                    let mut copy = String::new();
                    copy.try_reserve_exact(name.len())
                        .map_err(|_| ex.out_of_memory())?;
                    copy.push_str(name);
                    Atom::Field(copy)
                }
                Tag::Value(index) => match copy_value(ex, &roots[*index])? {
                    Value::Undefined => Atom::Undefined,
                    Value::Null => Atom::Null,
                    Value::Bool(v) => Atom::Bool(v),
                    Value::Date(v) => Atom::Date(v),
                    Value::Number(v) => Atom::Number(v),
                    Value::String(v) => Atom::String(v),
                    _ => return Err(ex.out_of_memory()),
                },
            });
        }
        let key = Key {
            module: entry.module.clone(),
            method: entry.method,
            atoms,
            bytes: 0,
        };
        let value = copy_value(ex, &roots[entry.value])?;
        ex.request_reuse.entries.insert(
            key,
            Entry {
                value,
                expires: Some(entry.expires),
            },
        );
    }
    memory::transfer(
        ex,
        (roots.capacity() * size_of::<Value>()) as u64,
        true,
        |visit| store.retained_memory(visit),
    )?;
    let (measured, work) = memory::measure(ex, None, false);
    ex.charge(work)?;
    let Some(measured) = measured? else {
        return Err(ex.out_of_memory());
    };
    measured.apply(ex);
    ex.retained_check = Some(reconcile);
    Ok(())
}

pub(crate) fn attach(ex: &mut Execution, store: ReuseStore) {
    ex.reuse = Some(ReuseStore {
        limit: store.limit.or(ex.options().memory_limit),
        ..Default::default()
    });
    if load(ex, &store).is_err() {
        clear(ex);
    }
}
