use super::native_error;
use bsl_prim::Str;
use bsl_vm::{
    Arg, CallResult, Exception, Execution, HostError, HostObject, MemberId, RetainedMemory, TypeId,
    Value,
};
use icu46_sys::{
    managed::runtime::{Mode, mode, process_root},
    regex::{Regex, RegexFlags},
};
use std::{any::Any, cell::RefCell, mem::size_of, rc::Rc};

#[derive(Clone)]
struct Entry {
    pattern: Str,
    flags: RegexFlags,
    regex: Rc<RefCell<Regex>>,
}
struct Cache(Vec<Entry>);

fn inaccessible() -> bsl_vm::CallError {
    HostError::not_implemented("Intrinsic regex cache", "member").into()
}
impl HostObject for Cache {
    fn type_id(&self) -> TypeId {
        TypeId::UNDEFINED
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, _: &mut Execution, _: MemberId) -> CallResult<Value> {
        Err(inaccessible())
    }
    fn set(&self, _: &mut Execution, _: MemberId, _: Value) -> CallResult<()> {
        Err(inaccessible())
    }
    fn call(&self, _: &mut Execution, _: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        Err(inaccessible())
    }
    fn retained_memory(&self, visit: &mut dyn RetainedMemory) -> Result<bool, Exception> {
        visit.bytes((self.0.capacity() * size_of::<Entry>()) as u64)?;
        for entry in &self.0 {
            visit.work(1)?;
            visit.string(&entry.pattern)?;
            visit.allocation(
                Rc::as_ptr(&entry.regex) as usize,
                crate::collections::accounting::retained_rc_size::<RefCell<Regex>>(),
            )?;
        }
        Ok(true)
    }
}

pub(super) fn matcher(ex: &mut Execution, pattern: &Str, flags: RegexFlags) -> CallResult<Regex> {
    let old = ex.intrinsic_cache().cloned();
    let cached = old
        .as_ref()
        .and_then(Value::as_object)
        .and_then(|o| o.as_any().downcast_ref::<Cache>());
    let hit = cached.and_then(|cache| {
        cache
            .0
            .iter()
            .position(|e| e.pattern == *pattern && e.flags == flags)
    });
    if let (Some(cache), Some(0)) = (cached, hit) {
        ex.charge(pattern.len() as u64 / 64 + 1)?;
        return cache.0[0]
            .regex
            .borrow_mut()
            .fork()
            .map_err(|error| native_error(ex, pattern, error));
    }
    let mut prepared = (16 * size_of::<Entry>() as u64)
        .saturating_add(crate::collections::accounting::retained_rc_size::<Cache>());
    if hit.is_none() {
        prepared = prepared
            .saturating_add(crate::collections::accounting::retained_rc_size::<
                RefCell<Regex>,
            >())
            .saturating_add(crate::collections::accounting::retained_shallow_bytes(
                &Value::String(pattern.clone()),
            ));
    }
    ex.reserve(prepared)?;
    ex.charge(pattern.len() as u64 / 64 + 1)?;
    let mut entries = Vec::new();
    entries
        .try_reserve_exact(16)
        .map_err(|_| ex.out_of_memory())?;
    if let Some(cache) = cached {
        entries.extend(cache.0.iter().cloned());
    }
    let entry = if let Some(index) = hit {
        entries.remove(index)
    } else {
        let compiled = match mode() {
            Mode::Managed | Mode::Quarantined(_) => {
                let root = process_root().ok_or_else(|| ex.out_of_memory())?;
                if let Some(cancel) = &ex.options().cancel {
                    Regex::new_managed_cancellable(pattern.units(), flags, &root, cancel)
                } else {
                    Regex::new_managed(pattern.units(), flags, &root)
                }
            }
            _ => Regex::new(pattern.units(), flags),
        };
        let mut compiled = compiled.map_err(|error| native_error(ex, pattern, error))?;
        compiled
            .clear_input()
            .map_err(|error| native_error(ex, pattern, error))?;
        Entry {
            pattern: pattern.clone(),
            flags,
            regex: Rc::new(RefCell::new(compiled)),
        }
    };
    let working = entry
        .regex
        .borrow_mut()
        .fork()
        .map_err(|error| native_error(ex, pattern, error))?;
    if entries.len() == 16 {
        entries.pop();
    }
    entries.insert(0, entry);
    entries.truncate(16);
    if hit != Some(0) {
        ex.set_intrinsic_cache_prepared(Value::Object(Rc::new(Cache(entries))), prepared)?;
    }
    Ok(working)
}
