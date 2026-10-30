pub(crate) mod retire;
use crate::{Exception, Execution, Number, ObjRef, RetainedMemory, Str, TypeId, Value};
use bsl_prim::BslDate;
use std::{
    any::Any,
    collections::{HashMap, HashSet},
    mem::{size_of, size_of_val},
    rc::Rc,
    sync::Arc,
};

pub fn session_shared_bytes<T>() -> u64 {
    #[repr(C)]
    struct Shared<T> {
        counts: [usize; 2],
        value: T,
    }
    size_of::<Shared<T>>() as u64
}

pub trait SessionObject: Send + Sync {
    fn retained_memory(&self, visit: &mut dyn RetainedMemory) -> Result<(), Exception>;
    fn create(&self, ex: &mut Execution, imports: &mut SessionImports) -> Result<Value, Exception>;
    fn fill(
        &self,
        ex: &mut Execution,
        shell: &Value,
        children: Vec<Value>,
    ) -> Result<(), Exception>;
    fn clear(&self, shell: &Value);
    fn restore_bytes(&self, children: usize) -> u64;
    fn restore_memory(
        &self,
        children: usize,
        visit: &mut dyn RetainedMemory,
    ) -> Result<(), Exception> {
        visit.bytes(self.restore_bytes(children))
    }
}

pub type SessionFactory<'a> =
    dyn FnMut(&mut dyn SessionExport) -> Result<Arc<dyn Any + Send + Sync>, Exception> + 'a;

pub trait SessionExport {
    fn memory_error(&self) -> Exception;
    fn value(&mut self, value: &Value) -> Result<(), Exception>;
    fn resource(&mut self, identity: usize, source: ObjRef) -> Result<(), Exception>;
    fn reserve(&mut self, bytes: u64) -> Result<(), Exception>;
    fn backing(
        &mut self,
        kind: std::any::TypeId,
        identity: usize,
        make: &mut SessionFactory<'_>,
    ) -> Result<Arc<dyn Any + Send + Sync>, Exception>;
}

#[derive(Default)]
pub struct SessionImports {
    backings: HashMap<(std::any::TypeId, usize), Rc<dyn Any>>,
}
impl SessionImports {
    pub fn get<T: Any>(&self, identity: usize) -> Option<Rc<T>> {
        self.backings
            .get(&(std::any::TypeId::of::<T>(), identity))?
            .clone()
            .downcast()
            .ok()
    }
    pub fn backing<T: Any>(
        &mut self,
        ex: &mut Execution,
        identity: usize,
        make: impl FnOnce(&mut Execution) -> Result<Rc<T>, Exception>,
    ) -> Result<Rc<T>, Exception> {
        let key = (std::any::TypeId::of::<T>(), identity);
        if let Some(value) = self.backings.get(&key) {
            return value
                .clone()
                .downcast::<T>()
                .map_err(|_| ex.out_of_memory());
        }
        ex.reserve((self.backings.capacity().saturating_add(4) as u64).saturating_mul(128))?;
        self.backings
            .try_reserve(1)
            .map_err(|_| ex.out_of_memory())?;
        let value = make(ex)?;
        self.backings.insert(key, value.clone());
        Ok(value)
    }
}

enum Wire {
    Undefined,
    Null,
    Bool(bool),
    Number(Number),
    String(usize),
    Date(BslDate),
    Type(TypeId),
    Object(usize),
}
struct Node {
    state: Box<dyn SessionObject>,
    children: Vec<Wire>,
}
pub(crate) struct Graph {
    roots: Vec<Wire>,
    strings: Vec<Vec<u16>>,
    nodes: Vec<Node>,
    pub(crate) bytes: u64,
}
pub(crate) struct Roots {
    values: Vec<Value>,
    limit: Option<u64>,
}
impl std::ops::Deref for Roots {
    type Target = Vec<Value>;
    fn deref(&self) -> &Self::Target {
        &self.values
    }
}
impl Drop for Roots {
    fn drop(&mut self) {
        retire::release(self.values.iter(), self.limit);
    }
}
struct Exporter<'a> {
    ex: Option<&'a mut Execution>,
    offset: usize,
    strings: Vec<Vec<u16>>,
    objects: Vec<ObjRef>,
    identities: HashMap<(u8, usize), usize>,
    children: Vec<Wire>,
    allocated: u64,
    backings: HashMap<(std::any::TypeId, usize), Arc<dyn Any + Send + Sync>>,
}
impl Exporter<'_> {
    fn wire(&mut self, value: &Value) -> Result<Wire, Exception> {
        if let Some(ex) = self.ex.as_mut() {
            ex.charge(1)?;
        }
        Ok(match value {
            Value::Undefined => Wire::Undefined,
            Value::Null => Wire::Null,
            Value::Bool(v) => Wire::Bool(*v),
            Value::Date(v) => Wire::Date(*v),
            Value::Type(v) => Wire::Type(*v),
            Value::Number(v) => {
                if let Some(ex) = self.ex.as_mut() {
                    ex.charge(v.size_hint_limbs() / 64)?;
                }
                self.reserve(v.size_hint_limbs().saturating_mul(4))?;
                Wire::Number(
                    v.try_clone_with(|_| Ok::<_, ()>(()))
                        .map_err(|_| self.memory_error())?,
                )
            }
            Value::String(v) => {
                let identity = (0, v.backing_identity());
                let index = if let Some(index) = self.identities.get(&identity) {
                    *index
                } else {
                    self.reserve((v.len() as u64).saturating_mul(2).saturating_add(256))?;
                    let mut text = Vec::new();
                    text.try_reserve_exact(v.len())
                        .map_err(|_| self.memory_error())?;
                    text.extend_from_slice(v.units());
                    self.strings
                        .try_reserve_exact(1)
                        .map_err(|_| self.memory_error())?;
                    self.identities
                        .try_reserve(1)
                        .map_err(|_| self.memory_error())?;
                    let index = self.strings.len();
                    self.strings.push(text);
                    self.identities.insert(identity, index);
                    index
                };
                Wire::String(index)
            }
            Value::Object(v) => {
                let identity = (1, Rc::as_ptr(v).cast::<()>() as usize);
                let index = if let Some(index) = self.identities.get(&identity) {
                    *index
                } else {
                    self.reserve(256)?;
                    self.objects
                        .try_reserve_exact(1)
                        .map_err(|_| self.memory_error())?;
                    self.identities
                        .try_reserve(1)
                        .map_err(|_| self.memory_error())?;
                    let index = self.objects.len() + self.offset;
                    self.objects.push(v.clone());
                    self.identities.insert(identity, index);
                    index
                };
                Wire::Object(index)
            }
        })
    }
}
impl SessionExport for Exporter<'_> {
    fn memory_error(&self) -> Exception {
        self.ex.as_ref().map_or_else(
            || {
                Exception::Fatal(Rc::new(crate::FatalError {
                    kind: crate::FatalKind::Memory,
                    text: "Insufficient memory".into(),
                    cause: None,
                }))
            },
            |ex| ex.out_of_memory(),
        )
    }
    fn resource(&mut self, identity: usize, source: ObjRef) -> Result<(), Exception> {
        self.reserve(256 + size_of::<Wire>() as u64)?;
        let key = (2, identity);
        let index = if let Some(index) = self.identities.get(&key) {
            *index
        } else {
            self.identities
                .try_reserve(1)
                .map_err(|_| self.memory_error())?;
            self.objects
                .try_reserve_exact(1)
                .map_err(|_| self.memory_error())?;
            let index = self.objects.len() + self.offset;
            self.objects.push(source);
            self.identities.insert(key, index);
            index
        };
        self.children
            .try_reserve_exact(1)
            .map_err(|_| self.memory_error())?;
        self.children.push(Wire::Object(index));
        Ok(())
    }
    fn backing(
        &mut self,
        kind: std::any::TypeId,
        identity: usize,
        make: &mut SessionFactory<'_>,
    ) -> Result<Arc<dyn Any + Send + Sync>, Exception> {
        let key = (kind, identity);
        if let Some(value) = self.backings.get(&key) {
            return Ok(value.clone());
        }
        self.reserve(256)?;
        self.backings
            .try_reserve(1)
            .map_err(|_| self.memory_error())?;
        let value = make(self)?;
        self.backings.insert(key, value.clone());
        Ok(value)
    }
    fn value(&mut self, value: &Value) -> Result<(), Exception> {
        self.reserve(size_of::<Wire>() as u64)?;
        let wire = self.wire(value)?;
        self.children
            .try_reserve_exact(1)
            .map_err(|_| self.memory_error())?;
        self.children.push(wire);
        Ok(())
    }
    fn reserve(&mut self, bytes: u64) -> Result<(), Exception> {
        self.allocated = self.allocated.saturating_add(bytes);
        if self.allocated == u64::MAX {
            return Err(self.memory_error());
        }
        if let Some(ex) = self.ex.as_mut() {
            ex.reserve(self.allocated.saturating_mul(2))
        } else {
            Ok(())
        }
    }
}
impl Graph {
    pub(crate) fn export(
        ex: &mut Execution,
        roots: &[Value],
        scratch: u64,
    ) -> Result<Option<Self>, Exception> {
        let mut out = Exporter {
            ex: Some(ex),
            offset: 0,
            strings: Vec::new(),
            objects: Vec::new(),
            identities: HashMap::new(),
            backings: HashMap::new(),
            children: Vec::new(),
            allocated: scratch,
        };
        for root in roots {
            out.value(root)?;
        }
        let roots = std::mem::take(&mut out.children);
        let mut nodes = Vec::new();
        let mut index = 0;
        while let Some(object) = out.objects.get(index).cloned() {
            out.reserve((2 * size_of::<Node>()) as u64)?;
            let Some(state) = object.session_transfer(&mut out)? else {
                return Ok(None);
            };
            nodes.try_reserve_exact(1).map_err(|_| out.memory_error())?;
            nodes.push(Node {
                state,
                children: std::mem::take(&mut out.children),
            });
            index += 1;
        }
        let mut graph = Self {
            roots,
            strings: std::mem::take(&mut out.strings),
            nodes,
            bytes: 0,
        };
        if let Some(ex) = out.ex.as_deref_mut() {
            graph.bytes = graph.measure(ex)?;
        } else {
            return Err(out.memory_error());
        }
        Ok(Some(graph))
    }
    pub(crate) fn snapshot_object(object: &dyn crate::HostObject) -> Option<Self> {
        retire::acyclic(object, None)?;
        let mut out = Exporter {
            ex: None,
            offset: 1,
            strings: Vec::new(),
            objects: Vec::new(),
            identities: HashMap::new(),
            backings: HashMap::new(),
            children: Vec::new(),
            allocated: 0,
        };
        out.identities.try_reserve(1).ok()?;
        out.identities
            .insert((1, std::ptr::from_ref(object).cast::<()>() as usize), 0);
        let state = object.session_transfer(&mut out).ok()??;
        let mut nodes = Vec::new();
        nodes.try_reserve_exact(1).ok()?;
        nodes.push(Node {
            state,
            children: std::mem::take(&mut out.children),
        });
        let mut index = 0;
        while let Some(object) = out.objects.get(index).cloned() {
            out.reserve((2 * size_of::<Node>()) as u64).ok()?;
            let state = object.session_transfer(&mut out).ok()??;
            nodes.try_reserve_exact(1).ok()?;
            nodes.push(Node {
                state,
                children: std::mem::take(&mut out.children),
            });
            index += 1;
        }
        let mut roots = Vec::new();
        roots.try_reserve_exact(1).ok()?;
        roots.push(Wire::Object(0));
        Some(Self {
            roots,
            strings: out.strings,
            nodes,
            bytes: 0,
        })
    }
    pub(crate) fn materialize_copy(&self, ex: &mut Execution) -> Result<Value, Exception> {
        let extra = (if self.bytes == 0 {
            self.measure(ex)?
        } else {
            0
        })
        .saturating_add(session_shared_bytes::<Self>());
        let mut roots = self.restore(ex, extra)?;
        roots.values.pop().ok_or_else(|| ex.out_of_memory())
    }
    fn measure(&self, ex: &mut Execution) -> Result<u64, Exception> {
        crate::reuse::memory::transfer(ex, 0, false, |visit| self.retained_memory(visit))
    }
    pub(crate) fn retained_memory(
        &self,
        counter: &mut dyn RetainedMemory,
    ) -> Result<(), Exception> {
        counter.bytes(
            (self.roots.capacity() * size_of::<Wire>()
                + self.strings.capacity() * size_of::<Vec<u16>>()
                + self.nodes.capacity() * size_of::<Node>()) as u64,
        )?;
        for text in &self.strings {
            counter.bytes((text.capacity() * 2) as u64)?;
        }
        for wire in self
            .roots
            .iter()
            .chain(self.nodes.iter().flat_map(|node| &node.children))
        {
            if let Wire::Number(n) = wire {
                counter.bytes((n.backing_capacity_limbs() * 4) as u64)?;
            }
        }
        for node in &self.nodes {
            counter.bytes(
                (size_of_val(&*node.state) + node.children.capacity() * size_of::<Wire>()) as u64,
            )?;
            counter.work(1 + node.children.len() as u64)?;
            node.state.retained_memory(counter)?;
        }
        Ok(())
    }
    pub(crate) fn restore(&self, ex: &mut Execution, extra: u64) -> Result<Roots, Exception> {
        let mut counter = Counter {
            ex,
            bytes: self.bytes.saturating_add(extra),
            seen: HashSet::new(),
        };
        counter.bytes(
            ((self.nodes.len() + self.roots.len()) * size_of::<Value>()
                + self.strings.len() * size_of::<Str>()) as u64,
        )?;
        for text in &self.strings {
            counter
                .bytes((text.len() * 2 + size_of::<Vec<u16>>() + 2 * size_of::<usize>()) as u64)?;
        }
        for wire in self
            .roots
            .iter()
            .chain(self.nodes.iter().flat_map(|node| &node.children))
        {
            if let Wire::Number(number) = wire {
                counter.work(number.size_hint_limbs() / 64)?;
                counter.bytes(number.size_hint_limbs().saturating_mul(8))?;
            }
        }
        for node in &self.nodes {
            node.state
                .restore_memory(node.children.len(), &mut counter)?;
            counter.bytes((node.children.len() * size_of::<Value>() * 2) as u64)?;
        }
        drop(counter);
        let mut strings = Vec::new();
        strings
            .try_reserve_exact(self.strings.len())
            .map_err(|_| ex.out_of_memory())?;
        for text in &self.strings {
            let mut copy = Vec::new();
            copy.try_reserve_exact(text.len())
                .map_err(|_| ex.out_of_memory())?;
            copy.extend_from_slice(text);
            strings.push(Str::from_vec(copy));
        }
        let mut values = Vec::new();
        values
            .try_reserve_exact(self.nodes.len())
            .map_err(|_| ex.out_of_memory())?;
        let mut imports = SessionImports::default();
        for node in &self.nodes {
            values.push(node.state.create(ex, &mut imports)?);
        }
        let result = (|| {
            for (index, node) in self.nodes.iter().enumerate() {
                ex.charge(1 + node.children.len() as u64)?;
                let mut children = Vec::new();
                children
                    .try_reserve_exact(node.children.len())
                    .map_err(|_| ex.out_of_memory())?;
                for wire in &node.children {
                    children.push(wire.restore(ex, &strings, &values)?);
                }
                node.state.fill(ex, &values[index], children)?;
            }
            let mut roots = Vec::new();
            roots
                .try_reserve_exact(self.roots.len())
                .map_err(|_| ex.out_of_memory())?;
            for root in &self.roots {
                roots.push(root.restore(ex, &strings, &values)?);
            }
            Ok(Roots {
                values: roots,
                limit: crate::reuse::memory_limit(ex),
            })
        })();
        if result.is_err() {
            for (node, value) in self.nodes.iter().zip(&values) {
                node.state.clear(value);
            }
        }
        result
    }
}
impl Wire {
    fn restore(
        &self,
        ex: &Execution,
        strings: &[Str],
        objects: &[Value],
    ) -> Result<Value, Exception> {
        Ok(match self {
            Self::Undefined => Value::Undefined,
            Self::Null => Value::Null,
            Self::Bool(v) => Value::Bool(*v),
            Self::Date(v) => Value::Date(*v),
            Self::Type(v) => Value::Type(*v),
            Self::Number(v) => Value::Number(
                v.try_clone_with(|_| Ok::<_, ()>(()))
                    .map_err(|_| ex.out_of_memory())?,
            ),
            Self::String(index) => Value::String(strings[*index].clone()),
            Self::Object(index) => objects[*index].clone(),
        })
    }
}
struct Counter<'a> {
    ex: &'a mut Execution,
    bytes: u64,
    seen: HashSet<usize>,
}
impl RetainedMemory for Counter<'_> {
    fn work(&mut self, units: u64) -> Result<(), Exception> {
        self.ex.charge(units)
    }
    fn mutable(&mut self) {}
    fn bytes(&mut self, bytes: u64) -> Result<(), Exception> {
        self.bytes = self.bytes.saturating_add(bytes);
        self.ex.reserve(
            self.bytes
                .saturating_add((self.seen.capacity() as u64).saturating_mul(32)),
        )
    }
    fn allocation(&mut self, identity: usize, bytes: u64) -> Result<bool, Exception> {
        if self.seen.contains(&identity) {
            return Ok(false);
        }
        self.ex
            .reserve(self.bytes.saturating_add(
                (self.seen.capacity().saturating_add(4) as u64).saturating_mul(64),
            ))?;
        self.seen
            .try_reserve(1)
            .map_err(|_| self.ex.out_of_memory())?;
        self.seen.insert(identity);
        self.bytes(bytes)?;
        Ok(true)
    }
    fn value(&mut self, _: &Value) -> Result<(), Exception> {
        Err(self.ex.out_of_memory())
    }
    fn string(&mut self, _: &Str) -> Result<(), Exception> {
        Err(self.ex.out_of_memory())
    }
}
