use crate::error::Exception;
use crate::error::info;
use crate::exec::{Frame, MethodRef};
use crate::runtime::Execution;
use bsl_compiler::{
    CompileOptions, CompiledFragment, CompiledModule, FragmentScope, Sym, compile_fragment,
};
use bsl_syntax::{ParseMode, SourceText};
use std::collections::HashMap;
use std::sync::Arc;

const CACHE_SIZE: usize = 256;
const MAX_CACHED_UNITS: usize = 64 * 1024;

#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    text: Vec<u16>,
    module: usize,
    method: (u8, u32),
    expression: bool,
}

#[derive(Default)]
pub(crate) struct FragmentCache {
    map: HashMap<Key, (Arc<CompiledFragment>, Arc<CompiledModule>, u64)>,
    tick: u64,
}

impl FragmentCache {
    fn get(&mut self, k: &Key) -> Option<Arc<CompiledFragment>> {
        self.tick += 1;
        let tick = self.tick;
        self.map.get_mut(k).map(|e| {
            e.2 = tick;
            e.0.clone()
        })
    }

    fn put(&mut self, k: Key, f: Arc<CompiledFragment>, caller: Arc<CompiledModule>) {
        if self.map.len() >= CACHE_SIZE
            && let Some(old) = self
                .map
                .iter()
                .min_by_key(|(_, e)| e.2)
                .map(|(k, _)| k.clone())
        {
            self.map.remove(&old);
        }
        self.tick += 1;
        self.map.insert(k, (f, caller, self.tick));
    }
}

pub(crate) fn start(ex: &mut Execution, sp_base: usize, expression: bool) -> Result<(), Exception> {
    let v = crate::exec::pop(&mut ex.exec, sp_base)?;
    let text = ex.present(&v)?;
    let fi = ex
        .exec
        .frames
        .len()
        .checked_sub(1)
        .ok_or_else(|| crate::exec::internal("no frame"))?;
    let (frame_module, inst, method, flags, real) = {
        let f = &ex.exec.frames[fi];
        (
            f.module.clone(),
            f.inst.clone(),
            f.method,
            f.flags,
            f.home.clone(),
        )
    };
    let key = Key {
        text: text.units().to_vec(),
        module: Arc::as_ptr(&frame_module) as usize,
        method: match method {
            MethodRef::Body => (0, 0),
            MethodRef::Method(i) => (1, i),
            MethodRef::Fragment => (2, 0),
        },
        expression,
    };
    let compiled = match ex.exec.fragments.get(&key) {
        Some(c) => c,
        None => {
            let mut slots: Vec<Sym> = crate::exec::code_of(&frame_module, method)
                .map(|c| c.slot_names.clone())
                .unwrap_or_default();
            if matches!(method, MethodRef::Fragment) {
                slots.extend(frame_module.outer.iter().cloned());
            }
            let c = compile(ex, &real, &slots, &text, expression)?;
            if text.len() <= MAX_CACHED_UNITS {
                ex.exec.fragments.put(key, c.clone(), frame_module.clone());
            }
            c
        }
    };
    crate::exec::push_fragment(
        ex,
        compiled.module.clone(),
        inst,
        real,
        flags,
        Some(fi),
        expression,
    )
}

pub(crate) fn compile(
    ex: &Execution,
    real: &Arc<CompiledModule>,
    slots: &[Sym],
    text: &bsl_prim::Str,
    expression: bool,
) -> Result<Arc<CompiledFragment>, Exception> {
    let app = ex.runtime().app().clone();
    let o = CompileOptions {
        module_name: &real.name,
        kind: real.kind,
        context: real.context,
        compat: real.compat,
        catalog: ex.runtime().registry().catalog,
        app: app.names(),
        extension: None,
    };
    let src = SourceText::from_utf16("", text.units().to_vec());
    let mode = if expression {
        ParseMode::Expression
    } else {
        ParseMode::Fragment
    };
    let scope = FragmentScope {
        caller_slots: slots,
        module: Some(real),
    };
    compile_fragment(&src, &scope, mode, &o).map_err(|e| {
        let cause = e.cause_text("", ex.lang());
        Exception::Catchable(info::compile_failure(ex, cause))
    })
}

pub(crate) fn outer_slot(ex: &Execution, mut fi: usize, mut i: usize) -> Result<usize, Exception> {
    loop {
        let oi = ex
            .exec
            .frames
            .get(fi)
            .and_then(|f| f.outer)
            .ok_or_else(|| crate::exec::internal("outer slot without a caller frame"))?;
        let c: &Frame = ex
            .exec
            .frames
            .get(oi)
            .ok_or_else(|| crate::exec::internal("caller frame is gone"))?;
        let n = c.sp_base - c.base;
        if i < n {
            return Ok(c.base + i);
        }
        i -= n;
        fi = oi;
    }
}
