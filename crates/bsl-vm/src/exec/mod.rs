mod call;
mod frame;
mod handler;
mod iter;
mod member;
mod modes;
mod run;
pub(crate) use call::{arg_initial, is_omitted};

pub(crate) use call::{METHOD_NOT_FOUND, Reentry, call_index};
pub(crate) use frame::{Frame, MethodRef, code_of};
pub(crate) use run::{pop, push_fragment, push_frame, run_until_return};

use crate::error::info::Position;
use crate::error::{Exception, FatalError, FatalKind, StackEntry, catalog_text};
use crate::host::{ExecFlags, ModuleContext, ModuleInstanceRef};
use crate::runtime::Execution;
use crate::value::Value;
use bsl_compiler::CompiledModule;
use bsl_prim::Str;
use std::rc::Rc;
use std::sync::Arc;

#[derive(Default)]
pub(crate) struct ExecState {
    pub stack: Vec<Value>,
    pub frames: Vec<Frame>,
    pub steps: u64,
    pub nesting: u32,
    pub entry_weight: u64,
    pub frame_weight: Option<u64>,
    pub fragments: crate::fragment::FragmentCache,
}

pub(crate) fn fatal(kind: FatalKind, text: String) -> Exception {
    Exception::Fatal(Rc::new(FatalError {
        kind,
        text,
        cause: None,
    }))
}

pub(crate) fn internal(text: impl Into<String>) -> Exception {
    fatal(FatalKind::Internal, text.into())
}

impl Execution {
    #[doc(hidden)]
    pub fn bare_instance(
        &mut self,
        module: Arc<CompiledModule>,
        ctx: ModuleContext,
    ) -> ModuleInstanceRef {
        ModuleInstanceRef::new(module, ctx)
    }

    #[doc(hidden)]
    pub fn run_body_with_locals(
        &mut self,
        inst: &ModuleInstanceRef,
    ) -> Result<Vec<(String, Value)>, Exception> {
        let module = inst.module().clone();
        let floor = self.exec.frames.len();
        let flags = self.frame_flags();
        run::push_frame(
            self,
            module.clone(),
            Some(inst.clone()),
            MethodRef::Body,
            0,
            flags,
        )?;
        let (_, slots) = run::run_frames(self, floor, true)?;
        let names = module
            .body
            .as_ref()
            .map(|b| &b.slot_names[..])
            .unwrap_or(&[]);
        Ok(names
            .iter()
            .zip(slots)
            .filter(|(n, _)| !n.name.is_empty())
            .map(|(n, v)| (n.name.to_string(), v))
            .collect())
    }

    pub(crate) fn frame_flags(&self) -> ExecFlags {
        self.exec.frames.last().map_or(
            ExecFlags {
                safe_mode: false,
                privileged: self.opts.privileged,
            },
            Frame::mode_flags,
        )
    }

    pub(crate) fn enter_weight(&mut self, weight: u32) {
        self.exec.entry_weight = if self.exec.frames.is_empty() {
            0
        } else {
            u64::from(weight.max(crate::runtime::StackWeights::MIN_REENTRY))
        };
    }

    pub(crate) fn enter_full_weight(&mut self, weight: u32) {
        self.exec.entry_weight = 0;
        self.exec.frame_weight = (!self.exec.frames.is_empty()).then_some(u64::from(weight));
    }

    pub(crate) fn stack_overflow(&self) -> Exception {
        fatal(
            FatalKind::StackOverflow,
            catalog_text("backend:IDS_ERR_STACKOVERFLOW", &[], "", self.lang()),
        )
    }

    pub(crate) fn frames_position(&self) -> Position {
        let mut stack = Vec::with_capacity(self.exec.frames.len());
        for f in self.exec.frames.iter().rev() {
            let (line, source_line) = match f.code() {
                Some(code) => {
                    let line = frame::line_at(code, f.pc);
                    let src = usize::try_from(line)
                        .ok()
                        .and_then(|l| l.checked_sub(1))
                        .and_then(|l| f.module.source_lines.get(l))
                        .map_or_else(Str::new, |u| Str::from_units(u));
                    (line, src)
                }
                None => (0, Str::new()),
            };
            stack.push(StackEntry {
                module_name: Str::from(&*f.module.name),
                line,
                source_line,
            });
        }
        match stack.first() {
            Some(top) => Position {
                module_name: top.module_name.clone(),
                line: top.line,
                source_line: top.source_line.clone(),
                stack,
            },
            None => Position::default(),
        }
    }
}
