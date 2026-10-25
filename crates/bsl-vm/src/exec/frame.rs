use crate::error::ErrorInfo;
use crate::host::{ExecFlags, ModuleInstanceRef};
use bsl_compiler::{CompiledModule, MethodCode};
use std::rc::Rc;
use std::sync::Arc;

#[derive(Clone, Copy, Debug)]
pub(crate) enum MethodRef {
    Body,
    Method(u32),
    Fragment,
}

pub(crate) struct Frame {
    pub method: MethodRef,
    pub module: Arc<CompiledModule>,
    pub inst: Option<ModuleInstanceRef>,
    pub home: Arc<CompiledModule>,
    pub base: usize,
    pub sp_base: usize,
    pub pc: u32,
    pub handler_exc: Option<Rc<ErrorInfo>>,
    pub handlers: Vec<Rc<ErrorInfo>>,
    pub flags: ExecFlags,
    pub outer: Option<usize>,
    pub used: u64,
}

impl Frame {
    pub fn code(&self) -> Option<&MethodCode> {
        code_of(&self.module, self.method)
    }
}

pub(crate) fn code_of(module: &CompiledModule, m: MethodRef) -> Option<&MethodCode> {
    match m {
        MethodRef::Body | MethodRef::Fragment => module.body.as_ref(),
        MethodRef::Method(i) => module.methods.get(usize::try_from(i).ok()?),
    }
}

pub(crate) fn line_at(code: &MethodCode, pc: u32) -> u32 {
    let i = code.lines.partition_point(|&(p, _)| p <= pc);
    i.checked_sub(1).map_or(0, |i| code.lines[i].1)
}
