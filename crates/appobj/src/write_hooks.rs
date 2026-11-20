use crate::events::{self, Event};
use crate::objects::ObjectInstance;
use bsl_vm::{Arg, CallResult, Execution, Value};
use std::rc::Rc;

pub(crate) struct WriteCtx<'a> {
    pub(crate) inst: &'a Rc<ObjectInstance>,
    pub(crate) is_new: bool,
    pub(crate) loading: bool,
    pub(crate) document: Option<DocumentModes>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DocWrite {
    Write,
    Post,
    Unpost,
    Mark(bool),
}

pub(crate) struct DocumentModes {
    pub(crate) write: Value,
    pub(crate) posting: Value,
    pub(crate) op: DocWrite,
}

impl DocumentModes {
    pub(crate) fn post(&self) -> bool {
        self.op == DocWrite::Post
    }

    pub(crate) fn real_time(&self) -> bool {
        self.post()
            && bsl_stdlib::enums::as_enum_value(&self.posting)
                .is_some_and(|e| e.name_ru() == "Оперативный")
    }

    pub(crate) fn unposts(&self) -> bool {
        self.op == DocWrite::Unpost || self.op == DocWrite::Mark(true)
    }

    pub(crate) fn undoing(op: DocWrite) -> Result<DocumentModes, bsl_vm::HostError> {
        let value = |t: &str, v: &str| {
            bsl_stdlib::api::system_enum_value(t, v).ok_or_else(|| {
                bsl_vm::HostError::text(
                    &format!("internal: no system enumeration value {t}.{v}"),
                    &format!("internal: no system enumeration value {t}.{v}"),
                )
            })
        };
        Ok(DocumentModes {
            write: value("РежимЗаписиДокумента", "ОтменаПроведения")?,
            posting: value("РежимПроведенияДокумента", "Неоперативный")?,
            op,
        })
    }
}

pub(crate) fn post(
    ex: &mut Execution,
    ctx: &WriteCtx<'_>,
    r: crate::values::refs::RefValue,
) -> CallResult<Vec<Value>> {
    match &ctx.document {
        Some(m) if m.post() => crate::posting::post(ex, ctx.inst, &m.posting, r),
        _ => Ok(Vec::new()),
    }
}

pub(crate) fn before_write(ex: &mut Execution, ctx: &WriteCtx<'_>) -> CallResult<()> {
    match &ctx.document {
        Some(m) => events::writing_document(ex, ctx.inst, Event::BeforeWrite, &m.write, &m.posting),
        None => events::writing(ex, ctx.inst, Event::BeforeWrite),
    }
}

pub(crate) fn assign_code(ex: &mut Execution, ctx: &WriteCtx<'_>) -> CallResult<()> {
    if ctx.document.is_some() {
        return crate::codes::assign_number(ex, ctx);
    }
    let layout = crate::codes::Layout::of(&ctx.inst.app, ctx.inst.obj)?;
    crate::write_checks::check_predefined_name(ex, ctx, &layout)?;
    crate::codes::assign(ex, ctx)
}

pub(crate) fn check_write(ex: &mut Execution, ctx: &WriteCtx<'_>) -> CallResult<()> {
    if ctx.document.is_some() {
        crate::write_checks::check_document_date(ex, ctx)?;
        return crate::codes::check_unique_number(ex, ctx);
    }
    let layout = crate::codes::Layout::of(&ctx.inst.app, ctx.inst.obj)?;
    crate::codes::check_unique(ex, ctx)?;
    crate::write_checks::check_owner(ex, ctx, &layout)?;
    crate::write_checks::check_hierarchy(ex, ctx, &layout)
}

pub(crate) fn on_write(ex: &mut Execution, ctx: &WriteCtx<'_>) -> CallResult<()> {
    match &ctx.document {
        Some(m) => events::writing_document(ex, ctx.inst, Event::OnWrite, &m.write, &m.posting),
        None => events::writing(ex, ctx.inst, Event::OnWrite),
    }
}

pub(crate) fn after_copy(
    ex: &mut Execution,
    copy: &Rc<ObjectInstance>,
    source: &ObjectInstance,
) -> CallResult<()> {
    events::on_copy(ex, copy, &source.rc()?)
}

pub(crate) fn delete(
    ex: &mut Execution,
    inst: &ObjectInstance,
    member: &str,
    args: &[Arg],
) -> CallResult<Value> {
    if member == "Удалить" {
        crate::delete::delete(ex, inst)
    } else {
        crate::delete::set_mark(ex, inst, args)
    }
}
