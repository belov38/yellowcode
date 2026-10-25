use crate::error::{
    Category, CategorySet, ErrorInfo, Exception, FatalError, FatalKind, HostError, catalog_text,
};
use crate::exec::{self, MethodRef};
use crate::host::{ExecFlags, ModuleContext, ModuleInstanceRef, ModuleRef};
use crate::object::{CallError, CallResult, HostObject};
use crate::place::Arg;
use crate::runtime::Execution;
use crate::types::{MemberId, MemberKind, TypeId};
use crate::value::Value;
use bsl_compiler::{AppGlobal, CompileError, CompiledModule, MethodCode};
use bsl_prim::Str;
use bsl_syntax::ast::MethodKind;
use platform_catalog::messages::Lang;
use std::any::Any;
use std::rc::Rc;
use std::sync::Arc;

const MODULE_INIT: &str = "backbas:IDS_ERR_BSL_MODULE_INIT_1";
const PROP_NOT_FOUND: &str = "bsl:IDS_RTE_VALPROP_NOTFOUND";

pub fn module_init_error(module_name: &str, cause: &CompileError, lang: Lang) -> Exception {
    let text = catalog_text(MODULE_INIT, &[module_name.to_owned()], "", lang);
    let cause = ErrorInfo {
        description: Str::from(cause.cause_text(module_name, lang)),
        module_name: Str::new(),
        line: 0,
        source_line: Str::new(),
        cause: None,
        code: Str::new(),
        additional: Value::Undefined,
        categories: CategorySet::default().with(Category::AllErrors),
        stack: Vec::new(),
        key: None,
        wire: None,
    };
    Exception::Fatal(Rc::new(FatalError {
        kind: FatalKind::ModuleInit,
        text,
        cause: Some(Rc::new(cause)),
    }))
}

pub(crate) fn required_args(code: &MethodCode) -> usize {
    code.params
        .iter()
        .rposition(|p| p.default.is_none())
        .map_or(0, |i| i + 1)
}

pub(crate) fn exported<'m>(
    module: &'m CompiledModule,
    name: &str,
) -> Option<(u32, &'m MethodCode)> {
    module.method(name).filter(|(_, m)| m.export)
}

pub(crate) struct ModuleValue {
    inst: ModuleInstanceRef,
    ty: TypeId,
}

impl ModuleValue {
    pub(crate) fn instance(&self) -> &ModuleInstanceRef {
        &self.inst
    }
}

fn kind_of(code: &MethodCode) -> MemberKind {
    let clamp = |n: usize| u16::try_from(n).unwrap_or(u16::MAX);
    MemberKind::Method {
        is_function: code.kind == MethodKind::Function,
        min_args: clamp(required_args(code)),
        max_args: Some(clamp(code.params.len())),
    }
}

impl HostObject for ModuleValue {
    fn type_id(&self) -> TypeId {
        self.ty
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, _: &mut Execution, _: MemberId) -> CallResult<Value> {
        Err(HostError::key(PROP_NOT_FOUND).into())
    }
    fn set(&self, _: &mut Execution, _: MemberId, _: Value) -> CallResult<()> {
        Err(HostError::key(PROP_NOT_FOUND).into())
    }
    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        let flags = ex.frame_flags();
        let w = ex.options().weights.reentry;
        exec::call_index(
            ex,
            &self.inst,
            m.0,
            args,
            flags,
            exec::Reentry::OnTopOfFrame(w),
        )
        .map_err(CallError::Exc)
    }
    fn find_member(&self, name: &str) -> Option<MemberId> {
        exported(self.inst.module(), name).map(|(i, _)| MemberId(i))
    }
    fn member_kind(&self, m: MemberId) -> Option<MemberKind> {
        let code = self.inst.module().methods.get(m.0 as usize)?;
        code.export.then(|| kind_of(code))
    }
    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<Str>> {
        let lang = ex.lang();
        Ok(Some(Str::from(
            ex.runtime().types().presentation(self.ty, lang),
        )))
    }
    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        let other = other.as_any().downcast_ref::<ModuleValue>()?;
        Some(Rc::ptr_eq(&self.inst.0, &other.inst.0))
    }
}

impl Execution {
    pub fn instantiate(
        &mut self,
        m: &Arc<CompiledModule>,
        ctx: ModuleContext,
    ) -> Result<ModuleInstanceRef, Exception> {
        let w = exec::Reentry::OnTopOfFrame(self.opts.weights.reentry);
        self.instantiate_with(m, ctx, w)
    }

    pub fn instantiate_weighted(
        &mut self,
        m: &Arc<CompiledModule>,
        ctx: ModuleContext,
        weight: u32,
    ) -> Result<ModuleInstanceRef, Exception> {
        self.instantiate_with(m, ctx, exec::Reentry::FullLevel(weight))
    }

    fn instantiate_with(
        &mut self,
        m: &Arc<CompiledModule>,
        ctx: ModuleContext,
        reentry: exec::Reentry,
    ) -> Result<ModuleInstanceRef, Exception> {
        let inst = ModuleInstanceRef::new(m.clone(), ctx);
        if m.body.as_ref().is_some_and(|b| !b.code.is_empty()) {
            let floor = self.exec.frames.len();
            let flags = self.frame_flags();
            match reentry {
                exec::Reentry::OnTopOfFrame(w) => self.enter_weight(w),
                exec::Reentry::FullLevel(w) => self.enter_full_weight(w),
            }
            exec::push_frame(
                self,
                m.clone(),
                Some(inst.clone()),
                MethodRef::Body,
                0,
                flags,
            )?;
            exec::run_until_return(self, floor)?;
        }
        Ok(inst)
    }

    pub fn call(
        &mut self,
        inst: &ModuleInstanceRef,
        method: &str,
        args: &mut [Arg],
    ) -> Result<Value, Exception> {
        let flags = self.frame_flags();
        let w = exec::Reentry::OnTopOfFrame(self.opts.weights.reentry);
        self.call_with(inst, method, args, flags, w)
    }

    pub fn call_weighted(
        &mut self,
        inst: &ModuleInstanceRef,
        method: &str,
        args: &mut [Arg],
        weight: u32,
    ) -> Result<Value, Exception> {
        let flags = self.frame_flags();
        self.call_with(inst, method, args, flags, exec::Reentry::FullLevel(weight))
    }

    pub fn call_privileged(
        &mut self,
        inst: &ModuleInstanceRef,
        method: &str,
        args: &mut [Arg],
    ) -> Result<Value, Exception> {
        let flags = ExecFlags {
            privileged: true,
            ..self.frame_flags()
        };
        let w = exec::Reentry::OnTopOfFrame(self.opts.weights.reentry);
        self.call_with(inst, method, args, flags, w)
    }

    pub fn call_privileged_weighted(
        &mut self,
        inst: &ModuleInstanceRef,
        method: &str,
        args: &mut [Arg],
        weight: u32,
    ) -> Result<Value, Exception> {
        let flags = ExecFlags {
            privileged: true,
            ..self.frame_flags()
        };
        self.call_with(inst, method, args, flags, exec::Reentry::FullLevel(weight))
    }

    fn call_with(
        &mut self,
        inst: &ModuleInstanceRef,
        method: &str,
        args: &mut [Arg],
        flags: ExecFlags,
        weight: exec::Reentry,
    ) -> Result<Value, Exception> {
        let Some((idx, _)) = inst.module().method(method) else {
            return Err(self.vm_error(exec::METHOD_NOT_FOUND, Some(method)));
        };
        exec::call_index(self, inst, idx, args, flags, weight)
    }

    pub fn module_value(&mut self, m: ModuleRef) -> Result<Value, Exception> {
        let app = self.runtime().app().clone();
        let inst = app.module(self, m)?;
        Ok(Value::Object(Rc::new(ModuleValue {
            inst,
            ty: self.runtime().module_type(),
        })))
    }

    pub fn execute_text(&mut self, inst: &ModuleInstanceRef, code: &str) -> Result<(), Exception> {
        let real = inst.module().clone();
        let text = Str::from(code);
        let compiled = crate::fragment::compile(self, &real, &[], &text, false)?;
        let floor = self.exec.frames.len();
        let flags = self.frame_flags();
        self.enter_weight(self.opts.weights.reentry);
        exec::push_fragment(
            self,
            compiled.module.clone(),
            Some(inst.clone()),
            real,
            flags,
            None,
            false,
        )?;
        exec::run_until_return(self, floor).map(drop)
    }

    pub(crate) fn app_global(&mut self, g: &AppGlobal) -> Result<Value, Exception> {
        let app = self.runtime().app().clone();
        let AppGlobal::CommonModule { name } = g;
        match app.module_ref(name) {
            Some(r) => self.module_value(r),
            None => app.global_value(self, g),
        }
    }
}
