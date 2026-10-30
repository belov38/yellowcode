mod match_key;
pub(crate) mod memory;
mod session;
mod validate;
pub use session::ReuseStore;
pub(crate) use session::{attach, detach};

use crate::error::info::{CallKind, from_host};
use crate::{CompiledModule, Exception, Execution, HostError, MsgRef, Number, Str, Value};
use bsl_compiler::{ModuleKind, Reuse};
use bsl_prim::{BslDate, fold_name};
use bsl_syntax::ast::MethodKind;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::mem::size_of;
use std::rc::Rc;
use std::sync::{Arc, Weak};

#[derive(PartialEq, Eq, Hash)]
enum Atom {
    Undefined,
    Null,
    Bool(bool),
    Number(Number),
    String(Str),
    Date(BslDate),
    Structure(usize),
    Field(String),
}

struct Key {
    module: Weak<CompiledModule>,
    method: u32,
    atoms: Vec<Atom>,
    bytes: u64,
}

impl PartialEq for Key {
    fn eq(&self, other: &Self) -> bool {
        Weak::ptr_eq(&self.module, &other.module)
            && self.method == other.method
            && self.atoms == other.atoms
    }
}
impl Eq for Key {}
impl Hash for Key {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.module.as_ptr().hash(state);
        self.method.hash(state);
        self.atoms.hash(state);
    }
}

#[derive(Default)]
pub(crate) struct RequestReuse {
    entries: HashMap<Key, Entry>,
    intrinsic: Option<Value>,
    retained: u64,
    mutable: bool,
    epoch: Rc<()>,
    retire_limit: Option<u64>,
}

impl Drop for RequestReuse {
    fn drop(&mut self) {
        if self.entries.values().any(|entry| entry.expires.is_some()) {
            crate::session_graph::retire::release(
                self.entries.values().map(|entry| &entry.value),
                self.retire_limit,
            );
        }
    }
}

struct Entry {
    value: Value,
    expires: Option<std::time::Duration>,
}
impl std::ops::Deref for Entry {
    type Target = Value;
    fn deref(&self) -> &Value {
        &self.value
    }
}

pub(crate) struct Pending {
    key: Key,
    epoch: Rc<()>,
    session: bool,
}

pub(crate) enum Lookup {
    Bypass,
    Hit(Value),
    Miss(Pending),
}

const ENTRY_BYTES: u64 = (size_of::<(Key, Entry)>() * 2 + 32) as u64;

impl RequestReuse {
    fn bytes(&self) -> u64 {
        self.retained
    }
}

pub(crate) fn base_bytes(ex: &Execution) -> u64 {
    ex.exec
        .frames
        .iter()
        .filter_map(|frame| frame.reuse.as_ref())
        .fold(ex.request_reuse.bytes(), |sum, pending| {
            sum.saturating_add(pending.key.bytes)
        })
}

pub(crate) fn memory_limit(ex: &Execution) -> Option<u64> {
    match (
        ex.options().memory_limit,
        ex.reuse.as_ref().and_then(|store| store.limit),
    ) {
        (Some(call), Some(session)) => Some(call.min(session)),
        (call, session) => call.or(session),
    }
}

fn reserve(ex: &Execution, bytes: u64) -> Result<(), Exception> {
    if memory_limit(ex).is_some_and(|limit| base_bytes(ex).saturating_add(bytes) > limit) {
        Err(ex.out_of_memory())
    } else {
        Ok(())
    }
}

fn copy_value(ex: &Execution, value: &Value) -> Result<Value, Exception> {
    match value {
        Value::Number(number) => number
            .try_clone_with(|limbs| reserve(ex, (limbs as u64).saturating_mul(4)))
            .map(Value::Number)
            .map_err(|_| ex.out_of_memory()),
        other => Ok(other.clone()),
    }
}

enum Work {
    Value(Value),
    Field(Str, Value),
    LeaveField,
    LeaveStructure(*const ()),
}

struct Builder {
    atoms: Vec<Atom>,
    work: Vec<Work>,
    path: Vec<Str>,
    active: HashSet<*const ()>,
    bytes: u64,
}

impl Builder {
    fn account(&mut self, ex: &mut Execution, bytes: u64) -> Result<(), Exception> {
        self.bytes = self.bytes.saturating_add(bytes);
        reserve(ex, self.bytes.saturating_mul(2))
    }

    fn atom(&mut self, ex: &mut Execution, atom: Atom) -> Result<(), Exception> {
        self.account(ex, size_of::<Atom>() as u64)?;
        self.atoms
            .try_reserve_exact(1)
            .map_err(|_| ex.out_of_memory())?;
        self.atoms.push(atom);
        Ok(())
    }

    fn work(&mut self, ex: &mut Execution, work: Work) -> Result<(), Exception> {
        self.account(ex, size_of::<Work>() as u64)?;
        self.work
            .try_reserve_exact(1)
            .map_err(|_| ex.out_of_memory())?;
        self.work.push(work);
        Ok(())
    }

    fn invalid(&self, ex: &Execution, method: &str, param: usize) -> Exception {
        invalid(ex, method, param, self.path.iter())
    }

    fn value(
        &mut self,
        ex: &mut Execution,
        value: Value,
        method: &str,
        param: usize,
    ) -> Result<(), Exception> {
        self.work(ex, Work::Value(value))?;
        while let Some(work) = self.work.pop() {
            ex.charge(1)?;
            let value = match work {
                Work::LeaveField => {
                    self.path.pop();
                    continue;
                }
                Work::LeaveStructure(identity) => {
                    self.active.remove(&identity);
                    continue;
                }
                Work::Field(name, value) => {
                    let len = (name.len() as u64).saturating_mul(12);
                    self.account(ex, len.saturating_add(size_of::<Str>() as u64))?;
                    ex.charge(len / 64)?;
                    let folded = fold_name(&name.to_string_lossy());
                    self.atom(ex, Atom::Field(folded))?;
                    self.path
                        .try_reserve_exact(1)
                        .map_err(|_| ex.out_of_memory())?;
                    self.path.push(name);
                    self.work(ex, Work::LeaveField)?;
                    value
                }
                Work::Value(value) => value,
            };
            let atom =
                match value {
                    Value::Undefined => Atom::Undefined,
                    Value::Null => Atom::Null,
                    Value::Bool(value) => Atom::Bool(value),
                    Value::Date(value) => Atom::Date(value),
                    Value::Number(value) => {
                        self.account(
                            ex,
                            value
                                .size_hint_limbs()
                                .max(value.backing_capacity_limbs() as u64)
                                .saturating_mul(4),
                        )?;
                        ex.charge(value.size_hint_limbs() / 64)?;
                        Atom::Number(value)
                    }
                    Value::String(value) => {
                        ex.charge(value.len() as u64 / 64)?;
                        Atom::String(value)
                    }
                    Value::Type(_) => return Err(self.invalid(ex, method, param)),
                    Value::Object(object) => {
                        let identity = Rc::as_ptr(&object).cast::<()>();
                        if self.active.contains(&identity) {
                            return Err(self.invalid(ex, method, param));
                        }
                        let mut fields = Vec::new();
                        let supported = object.reuse_fields(&mut |name, value| {
                            self.account(
                                ex,
                                (size_of::<(String, Str, Value)>() as u64)
                                    .saturating_add((name.len() as u64).saturating_mul(12))
                                    .saturating_add(value_bytes(value)),
                            )?;
                            ex.charge(1 + name.len() as u64 / 64)?;
                            fields
                                .try_reserve_exact(1)
                                .map_err(|_| ex.out_of_memory())?;
                            fields.push((
                                fold_name(&name.to_string_lossy()),
                                name.clone(),
                                copy_value(ex, value)?,
                            ));
                            Ok(())
                        })?;
                        if !supported {
                            return Err(self.invalid(ex, method, param));
                        }
                        ex.charge((fields.len() as u64).saturating_mul(
                            1 + u64::from(fields.len().checked_ilog2().unwrap_or(0)),
                        ))?;
                        fields.sort_unstable_by(|a, b| a.0.cmp(&b.0));
                        self.account(ex, 64)?;
                        self.active.try_reserve(1).map_err(|_| ex.out_of_memory())?;
                        self.active.insert(identity);
                        self.atom(ex, Atom::Structure(fields.len()))?;
                        self.work(ex, Work::LeaveStructure(identity))?;
                        for (_, name, value) in fields.into_iter().rev() {
                            self.work(ex, Work::Field(name, value))?;
                        }
                        continue;
                    }
                };
            self.atom(ex, atom)?;
        }
        Ok(())
    }
}

fn value_bytes(value: &Value) -> u64 {
    match value {
        Value::Number(number) => number
            .size_hint_limbs()
            .max(number.backing_capacity_limbs() as u64)
            .saturating_mul(4),
        _ => 0,
    }
}

fn invalid<'a>(
    ex: &Execution,
    method: &str,
    param: usize,
    path: impl Iterator<Item = &'a Str>,
) -> Exception {
    let path = path.map(Str::to_string_lossy).collect::<Vec<_>>().join(" ");
    let mut error = HostError::key("core85:IDS_PARAMETER_N_TYPE_MISMATCH");
    if let MsgRef::Key(key) = &mut error.msg {
        key.args.push(param.to_string());
        key.suffix = format!(
            " ({}{}{})",
            path,
            if path.is_empty() { "" } else { " " },
            match ex.lang() {
                crate::Lang::Ru =>
                    "Неверный тип параметра. Допустимы только примитивные типы, ссылки, Структура",
                crate::Lang::En =>
                    "Invalid parameter type. Only primitive types, references, Structure are allowed",
            }
        );
    }
    Exception::Catchable(from_host(ex, error, CallKind::Method, method))
}

fn build_key(
    ex: &mut Execution,
    module: &Arc<CompiledModule>,
    method: u32,
    argc: usize,
) -> Result<Key, Exception> {
    let code = &module.methods[method as usize];
    let base = ex.exec.stack.len().saturating_sub(argc);
    let mut builder = Builder {
        atoms: Vec::new(),
        work: Vec::new(),
        path: Vec::new(),
        active: HashSet::new(),
        bytes: size_of::<Key>() as u64,
    };
    for (index, param) in code.params.iter().enumerate() {
        let value = if index >= argc || crate::exec::is_omitted(&ex.exec.stack[base + index]) {
            match param.default.and_then(|i| module.consts.get(i as usize)) {
                None | Some(bsl_compiler::ir::Const::Undefined) => Value::Undefined,
                Some(bsl_compiler::ir::Const::Null) => Value::Null,
                Some(bsl_compiler::ir::Const::Bool(value)) => Value::Bool(*value),
                Some(bsl_compiler::ir::Const::Date(value)) => Value::Date(*value),
                Some(bsl_compiler::ir::Const::Number(value)) => {
                    builder.account(ex, value.size_hint_limbs().saturating_mul(4))?;
                    Value::Number(
                        value
                            .try_clone_with(|limbs| {
                                reserve(
                                    ex,
                                    builder
                                        .bytes
                                        .saturating_add((limbs as u64).saturating_mul(4)),
                                )
                            })
                            .map_err(|_| ex.out_of_memory())?,
                    )
                }
                Some(bsl_compiler::ir::Const::String(units)) => {
                    builder.account(ex, (units.len() as u64).saturating_mul(2))?;
                    ex.charge(units.len() as u64 / 64)?;
                    let mut copy = Vec::new();
                    copy.try_reserve_exact(units.len())
                        .map_err(|_| ex.out_of_memory())?;
                    copy.extend_from_slice(units);
                    Value::String(Str::from_vec(copy))
                }
            }
        } else {
            let value = &ex.exec.stack[base + index];
            let value = crate::exec::arg_initial(value);
            reserve(ex, builder.bytes.saturating_add(value_bytes(value)))?;
            copy_value(ex, value)?
        };
        builder.value(ex, value, &code.name.name, index + 1)?;
    }
    Ok(Key {
        module: Arc::downgrade(module),
        method,
        atoms: builder.atoms,
        bytes: builder.bytes,
    })
}

pub(crate) fn lookup(
    ex: &mut Execution,
    module: &Arc<CompiledModule>,
    method: u32,
    argc: usize,
) -> Result<Lookup, Exception> {
    reconcile(ex)?;
    let Some(code) = module.methods.get(method as usize) else {
        return Ok(Lookup::Bypass);
    };
    if module.context != bsl_compiler::ExecContext::Server
        || !matches!(module.kind, ModuleKind::Common(flags) if matches!(flags.reuse, Reuse::DuringRequest | Reuse::DuringSession))
        || code.kind != MethodKind::Function
        || !code.export
    {
        return Ok(Lookup::Bypass);
    }
    ex.charge(1)?;
    let mut key = match build_key(ex, module, method, argc) {
        Ok(key) => key,
        Err(Exception::Fatal(error)) if error.kind == crate::FatalKind::Memory => {
            let (hit, work) = match_key::hit(ex, module, method, argc);
            ex.charge(work)?;
            if let Some(value) = hit? {
                return Ok(Lookup::Hit(value));
            }
            validate::arguments(ex, module, method, argc)?;
            return Ok(Lookup::Bypass);
        }
        Err(error) => return Err(error),
    };
    if let Some(value) = ex.request_reuse.entries.get(&key) {
        return Ok(Lookup::Hit(copy_value(ex, value)?));
    }
    let (measured, work) = memory::pending(ex, &key);
    ex.charge(work)?;
    match measured {
        Ok(Some(graph)) => key.bytes = graph.incoming,
        Ok(None) => return Ok(Lookup::Bypass),
        Err(Exception::Fatal(error)) if error.kind == crate::FatalKind::Memory => {
            return Ok(Lookup::Bypass);
        }
        Err(error) => return Err(error),
    }
    Ok(Lookup::Miss(Pending {
        key,
        epoch: ex.request_reuse.epoch.clone(),
        session: matches!(module.kind, ModuleKind::Common(flags) if flags.reuse == Reuse::DuringSession),
    }))
}

fn admit_entry(
    ex: &mut Execution,
    key: &Key,
    value: &Value,
    session: bool,
) -> Result<Option<(Value, memory::Graph)>, Exception> {
    let measure = |replace| {
        if session {
            memory::session_entry(ex, (key, value), replace)
        } else {
            memory::measure(ex, Some((key, value)), replace)
        }
    };
    let (measured, work) = measure(false);
    ex.charge(work)?;
    let Some(mut graph) = (match measured {
        Ok(measured) => measured,
        Err(Exception::Fatal(error)) if error.kind == crate::FatalKind::Memory => return Ok(None),
        Err(error) => return Err(error),
    }) else {
        return Ok(None);
    };
    if ex.request_reuse.entries.contains_key(key) {
        drop(graph);
        let (measured, work) = if session {
            memory::session_entry(ex, (key, value), true)
        } else {
            memory::measure(ex, Some((key, value)), true)
        };
        ex.charge(work)?;
        match measured {
            Ok(Some(final_graph)) => graph = final_graph,
            Ok(None) => return Ok(None),
            Err(Exception::Fatal(error)) if error.kind == crate::FatalKind::Memory => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        }
    }
    let value = match copy_value(ex, value) {
        Ok(value) => value,
        Err(Exception::Fatal(error)) if error.kind == crate::FatalKind::Memory => return Ok(None),
        Err(error) => return Err(error),
    };
    if !ex.request_reuse.entries.contains_key(key)
        && ex.request_reuse.entries.try_reserve(1).is_err()
    {
        return Ok(None);
    }
    Ok(Some((value, graph)))
}

pub(crate) fn publish(
    ex: &mut Execution,
    pending: Pending,
    value: &Value,
) -> Result<(), Exception> {
    if !Rc::ptr_eq(&pending.epoch, &ex.request_reuse.epoch) {
        return Ok(());
    }
    if pending.session {
        let (supported, work) = memory::transferable(ex, value);
        ex.charge(work)?;
        match supported {
            Ok(true) => (),
            Ok(false) => return Ok(()),
            Err(Exception::Fatal(error)) if error.kind == crate::FatalKind::Memory => return Ok(()),
            Err(error) => return Err(error),
        }
    }
    if let Some((value, graph)) = admit_entry(ex, &pending.key, value, pending.session)? {
        let expires = pending.session.then(|| {
            ex.env()
                .reuse_clock()
                .saturating_add(std::time::Duration::from_secs(1200))
        });
        let replaced = ex
            .request_reuse
            .entries
            .insert(pending.key, Entry { value, expires });
        if let Some(old) = replaced.filter(|entry| entry.expires.is_some()) {
            crate::session_graph::retire::release(
                std::iter::once(&old.value),
                ex.request_reuse.retire_limit,
            );
        }
        ex.retained_check = Some(reconcile);
        graph.apply(ex);
    }
    Ok(())
}

pub(crate) fn reconcile(ex: &mut Execution) -> Result<(), Exception> {
    ex.retained_dirty = false;
    let now = ex.env().reuse_clock();
    let before = ex.request_reuse.entries.len();
    crate::session_graph::retire::release(
        ex.request_reuse
            .entries
            .values()
            .filter(|entry| entry.expires.is_some_and(|deadline| now >= deadline))
            .map(|entry| &entry.value),
        ex.request_reuse.retire_limit,
    );
    ex.request_reuse
        .entries
        .retain(|_, entry| entry.expires.is_none_or(|deadline| now < deadline));
    let expired = before != ex.request_reuse.entries.len();
    if !expired
        && (!ex.request_reuse.mutable
            || (ex.request_reuse.entries.is_empty() && ex.request_reuse.intrinsic.is_none()))
    {
        return Ok(());
    }
    let (measured, work) = memory::measure(ex, None, false);
    if let Err(error) = ex.charge(work) {
        clear(ex);
        return Err(error);
    }
    match measured {
        Ok(Some(retained)) => {
            retained.apply(ex);
            Ok(())
        }
        Ok(None) => {
            clear(ex);
            Ok(())
        }
        Err(error) => {
            clear(ex);
            Err(error)
        }
    }
}

pub(crate) fn clear(ex: &mut Execution) {
    ex.retained_dirty = false;
    for frame in &mut ex.exec.frames {
        frame.reuse = None;
    }
    ex.request_reuse = RequestReuse::default();
}

impl Execution {
    pub fn intrinsic_cache(&self) -> Option<&Value> {
        self.request_reuse.intrinsic.as_ref()
    }

    pub fn set_intrinsic_cache(&mut self, value: Value) -> Result<(), Exception> {
        self.set_intrinsic_cache_prepared(value, 0)
    }

    pub fn set_intrinsic_cache_prepared(
        &mut self,
        value: Value,
        prepared: u64,
    ) -> Result<(), Exception> {
        self.reserve(prepared)?;
        memory::transfer_prepared(self, 0, true, prepared, |visit| visit.value(&value))?;
        let old = self.request_reuse.intrinsic.replace(value);
        let (measured, work) = memory::measure_prepared(self, prepared);
        let next = std::mem::replace(&mut self.request_reuse.intrinsic, old);
        self.charge(work)?;
        let graph = measured?.ok_or_else(|| self.out_of_memory())?;
        self.request_reuse.intrinsic = next;
        graph.apply(self);
        self.retained_check = Some(reconcile);
        Ok(())
    }

    pub fn refresh_reusable_values(&mut self) {
        clear(self);
        self.clear_attached_reuse();
    }
}
