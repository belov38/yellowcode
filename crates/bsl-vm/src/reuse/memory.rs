use super::*;
use crate::{ObjRef, RetainedMemory};
use std::alloc::Layout;

struct Census<'a> {
    ex: &'a Execution,
    overhead: u64,
    live_floor: u64,
    retained: u64,
    work: u64,
    mutable: bool,
    seen: HashSet<(u8, usize)>,
    todo: Vec<ObjRef>,
    pending: Vec<u64>,
}

impl Census<'_> {
    fn guard(&self, retained_extra: u64, scratch_extra: u64) -> Result<(), Exception> {
        let scratch = (self.seen.capacity() as u64)
            .saturating_mul(64)
            .saturating_add((self.todo.capacity() as u64).saturating_mul(32))
            .saturating_add((self.pending.capacity() as u64).saturating_mul(16));
        if memory_limit(self.ex).is_some_and(|limit| {
            self.overhead
                .saturating_add(self.retained)
                .saturating_add(retained_extra)
                .max(self.live_floor)
                .saturating_add(scratch)
                .saturating_add(scratch_extra)
                > limit
        }) {
            Err(self.ex.out_of_memory())
        } else {
            Ok(())
        }
    }

    fn first(&mut self, kind: u8, identity: usize) -> Result<bool, Exception> {
        if self.seen.contains(&(kind, identity)) {
            return Ok(false);
        }
        let growth = if self.seen.len() == self.seen.capacity() {
            self.seen.capacity().saturating_add(1).max(3) as u64 * 64
        } else {
            0
        };
        self.guard(0, growth.saturating_add(256))?;
        self.seen
            .try_reserve(1)
            .map_err(|_| self.ex.out_of_memory())?;
        self.seen.insert((kind, identity));
        Ok(true)
    }

    fn key(&mut self, key: &Key) -> Result<(), Exception> {
        let layout = Layout::new::<[usize; 2]>()
            .extend(Layout::new::<CompiledModule>())
            .map_err(|_| self.ex.out_of_memory())?
            .0
            .pad_to_align();
        self.allocation(key.module.as_ptr() as usize, layout.size() as u64)?;
        self.bytes((key.atoms.capacity() * size_of::<Atom>()) as u64)?;
        for atom in &key.atoms {
            self.work(1)?;
            match atom {
                Atom::String(text) => self.string(text)?,
                Atom::Number(number) => {
                    self.bytes((number.backing_capacity_limbs() as u64).saturating_mul(4))?
                }
                Atom::Field(name) => self.bytes(name.capacity() as u64)?,
                _ => (),
            }
        }
        Ok(())
    }
}

impl RetainedMemory for Census<'_> {
    fn work(&mut self, units: u64) -> Result<(), Exception> {
        self.work = self.work.saturating_add(units);
        self.ex.check_charge(self.work)
    }

    fn mutable(&mut self) {
        self.mutable = true;
    }
    fn bytes(&mut self, bytes: u64) -> Result<(), Exception> {
        self.guard(bytes, 0)?;
        self.retained = self.retained.saturating_add(bytes);
        Ok(())
    }

    fn allocation(&mut self, identity: usize, bytes: u64) -> Result<bool, Exception> {
        if !self.first(2, identity)? {
            return Ok(false);
        }
        self.bytes(bytes)?;
        Ok(true)
    }

    fn string(&mut self, value: &Str) -> Result<(), Exception> {
        if self.first(1, value.backing_identity())? {
            self.bytes(
                (value.backing_capacity_units() as u64)
                    .saturating_mul(2)
                    .saturating_add((size_of::<Vec<u16>>() + 2 * size_of::<usize>()) as u64),
            )?;
        }
        Ok(())
    }

    fn value(&mut self, value: &Value) -> Result<(), Exception> {
        self.work(1)?;
        match value {
            Value::Number(_) => self.bytes(value_bytes(value))?,
            Value::String(text) => self.string(text)?,
            Value::Object(object) => {
                let identity = Rc::as_ptr(object).cast::<()>() as usize;
                if self.first(0, identity)? {
                    let layout = Layout::new::<[usize; 2]>()
                        .extend(Layout::for_value(&**object))
                        .map_err(|_| self.ex.out_of_memory())?
                        .0
                        .pad_to_align();
                    self.bytes(layout.size() as u64)?;
                    let growth = if self.todo.len() == self.todo.capacity() {
                        self.todo.capacity().saturating_add(1).max(4) as u64 * 32
                    } else {
                        0
                    };
                    self.guard(0, growth.saturating_add(64))?;
                    self.todo
                        .try_reserve(1)
                        .map_err(|_| self.ex.out_of_memory())?;
                    self.todo.push(object.clone());
                }
            }
            _ => (),
        }
        Ok(())
    }
}

pub(super) struct Graph {
    pub bytes: u64,
    pub mutable: bool,
    pub incoming: u64,
    pending: Vec<u64>,
}

impl Graph {
    pub fn apply(self, ex: &mut Execution) {
        ex.request_reuse.retire_limit =
            memory_limit(ex).map(|limit| limit.saturating_sub(self.bytes));
        ex.request_reuse.retained = self.bytes;
        ex.request_reuse.mutable = self.mutable;
        for (pending, bytes) in ex
            .exec
            .frames
            .iter_mut()
            .filter_map(|frame| frame.reuse.as_mut())
            .zip(self.pending)
        {
            pending.key.bytes = bytes;
        }
    }
}

pub(super) fn measure(
    ex: &Execution,
    incoming: Option<(&Key, &Value)>,
    replace: bool,
) -> (Result<Option<Graph>, Exception>, u64) {
    measure_all(ex, incoming, replace, None, false, 0)
}

pub(super) fn measure_prepared(
    ex: &Execution,
    prepared: u64,
) -> (Result<Option<Graph>, Exception>, u64) {
    measure_all(ex, None, false, None, false, prepared)
}

pub(super) fn session_entry(
    ex: &Execution,
    incoming: (&Key, &Value),
    replace: bool,
) -> (Result<Option<Graph>, Exception>, u64) {
    measure_all(ex, Some(incoming), replace, None, true, 0)
}

pub(super) fn pending(ex: &Execution, key: &Key) -> (Result<Option<Graph>, Exception>, u64) {
    measure_all(ex, None, false, Some(key), false, 0)
}

pub(super) fn transferable(ex: &Execution, value: &Value) -> (Result<bool, Exception>, u64) {
    let mut census = Census {
        ex,
        overhead: 0,
        live_floor: base_bytes(ex),
        retained: 0,
        work: 0,
        mutable: false,
        seen: HashSet::new(),
        todo: Vec::new(),
        pending: Vec::new(),
    };
    let result = (|| {
        census.value(value)?;
        while let Some(object) = census.todo.pop() {
            if !object.session_transfer_supported() || !object.retained_memory(&mut census)? {
                return Ok(false);
            }
        }
        Ok(true)
    })();
    (result, census.work)
}

pub(crate) fn transfer(
    ex: &mut Execution,
    scratch: u64,
    include_live: bool,
    visit: impl FnOnce(&mut dyn RetainedMemory) -> Result<(), Exception>,
) -> Result<u64, Exception> {
    transfer_prepared(ex, scratch, include_live, 0, visit)
}

pub(super) fn transfer_prepared(
    ex: &mut Execution,
    scratch: u64,
    include_live: bool,
    prepared: u64,
    visit: impl FnOnce(&mut dyn RetainedMemory) -> Result<(), Exception>,
) -> Result<u64, Exception> {
    let (result, work) = {
        let mut census = Census {
            ex,
            overhead: scratch,
            live_floor: base_bytes(ex).saturating_add(prepared),
            retained: 0,
            work: 0,
            mutable: false,
            seen: HashSet::new(),
            todo: Vec::new(),
            pending: Vec::new(),
        };
        let result = (|| {
            if include_live {
                census.bytes(
                    (ex.request_reuse.entries.capacity() as u64).saturating_mul(ENTRY_BYTES),
                )?;
                if let Some(value) = &ex.request_reuse.intrinsic {
                    census.value(value)?;
                }
                for (key, value) in &ex.request_reuse.entries {
                    census.key(key)?;
                    census.value(value)?;
                }
            }
            visit(&mut census)?;
            while let Some(object) = census.todo.pop() {
                if !object.retained_memory(&mut census)? {
                    return Err(ex.out_of_memory());
                }
            }
            census.guard(0, 0)?;
            Ok(census.retained)
        })();
        (result, census.work)
    };
    ex.charge(work)?;
    result
}

fn measure_all(
    ex: &Execution,
    incoming: Option<(&Key, &Value)>,
    replace: bool,
    new_pending: Option<&Key>,
    session: bool,
    prepared: u64,
) -> (Result<Option<Graph>, Exception>, u64) {
    let entries = &ex.request_reuse.entries;
    let grows = incoming.is_some_and(|(key, _)| !entries.contains_key(key))
        && entries.len() == entries.capacity();
    let capacity = entries.capacity().saturating_add(if grows {
        entries.capacity().saturating_add(1).max(3)
    } else {
        0
    });
    let table_bytes = (capacity as u64).saturating_mul(ENTRY_BYTES);
    let mut census = Census {
        ex,
        overhead: table_bytes,
        live_floor: base_bytes(ex).saturating_add(prepared).saturating_add(
            incoming
                .map(|(key, _)| key)
                .or(new_pending)
                .map_or(0, |key| key.bytes),
        ),
        retained: 0,
        work: 0,
        mutable: false,
        seen: HashSet::new(),
        todo: Vec::new(),
        pending: Vec::new(),
    };
    let measured = (|| {
        if let Some(value) = &ex.request_reuse.intrinsic {
            census.value(value)?;
        }
        for (key, value) in entries {
            census.key(key)?;
            if replace && incoming.is_some_and(|(next, _)| key == next) {
                continue;
            }
            census.value(value)?;
        }
        if let Some((key, value)) = incoming {
            if !entries.contains_key(key) {
                census.key(key)?;
            }
            census.value(value)?;
        }
        while let Some(object) = census.todo.pop() {
            if !object.retained_memory(&mut census)? {
                return Ok(None);
            }
        }
        if session || entries.values().any(|entry| entry.expires.is_some()) {
            census.guard(
                0,
                (census.seen.len() as u64)
                    .saturating_mul(1024)
                    .saturating_add(census.work.saturating_mul(32)),
            )?;
        }
        let cache_bytes = census.retained.saturating_add(table_bytes);
        let mutable = census.mutable;
        let count = ex
            .exec
            .frames
            .iter()
            .filter(|frame| frame.reuse.is_some())
            .count();
        census.guard(0, (count as u64).saturating_mul(16))?;
        census
            .pending
            .try_reserve_exact(count)
            .map_err(|_| ex.out_of_memory())?;
        for pending in ex
            .exec
            .frames
            .iter()
            .filter_map(|frame| frame.reuse.as_ref())
        {
            let before = census.retained;
            census.bytes(size_of::<Key>() as u64)?;
            census.key(&pending.key)?;
            census.pending.push(census.retained - before);
        }
        let before = census.retained;
        if let Some(key) = new_pending {
            census.bytes(size_of::<Key>() as u64)?;
            census.key(key)?;
            if entries.len() == entries.capacity() {
                census.guard(
                    0,
                    (entries.capacity().saturating_add(1).max(3) as u64)
                        .saturating_mul(ENTRY_BYTES),
                )?;
            }
        }
        census.guard(0, 0)?;
        Ok(Some(Graph {
            bytes: cache_bytes,
            mutable,
            incoming: census.retained - before,
            pending: std::mem::take(&mut census.pending),
        }))
    })();
    (measured, census.work)
}
