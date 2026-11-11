use crate::events::{self, Event};
use crate::objects::ObjectInstance;
use bsl_vm::{CallResult, Execution, HostError, Value};
use std::rc::Rc;

pub(crate) struct WriteCtx<'a> {
    pub(crate) inst: &'a Rc<ObjectInstance>,
    pub(crate) is_new: bool,
    pub(crate) loading: bool,
}

pub(crate) fn before_write(ex: &mut Execution, ctx: &WriteCtx<'_>) -> CallResult<()> {
    events::writing(ex, ctx.inst, Event::BeforeWrite)
}

pub(crate) fn assign_code(ex: &mut Execution, ctx: &WriteCtx<'_>) -> CallResult<()> {
    let layout = crate::codes::Layout::of(&ctx.inst.app, ctx.inst.obj)?;
    crate::write_checks::check_predefined_name(ex, ctx, &layout)?;
    crate::codes::assign(ex, ctx)
}

pub(crate) fn check_write(ex: &mut Execution, ctx: &WriteCtx<'_>) -> CallResult<()> {
    let layout = crate::codes::Layout::of(&ctx.inst.app, ctx.inst.obj)?;
    crate::codes::check_unique(ex, ctx)?;
    crate::write_checks::check_owner(ex, ctx, &layout)?;
    crate::write_checks::check_hierarchy(ex, ctx, &layout)
}

pub(crate) fn on_write(ex: &mut Execution, ctx: &WriteCtx<'_>) -> CallResult<()> {
    events::writing(ex, ctx.inst, Event::OnWrite)
}

pub(crate) fn after_copy(
    ex: &mut Execution,
    copy: &Rc<ObjectInstance>,
    source: &ObjectInstance,
) -> CallResult<()> {
    events::on_copy(ex, copy, &source.rc()?)
}

pub(crate) fn delete(
    _ex: &mut Execution,
    inst: &ObjectInstance,
    member: &str,
) -> CallResult<Value> {
    Err(HostError::not_implemented(&inst.type_name(), member).into())
}
