use bsl_vm::{CallError, Cause, Exception, Execution, FatalError, FatalKind, HostError};
use query_exec::{BatchError, ExecError};
use query_syntax::ScriptVariant;
use std::rc::Rc;

pub(crate) fn internal(text: &str) -> CallError {
    fatal(FatalKind::Internal, text.to_owned())
}
fn fatal(kind: FatalKind, text: String) -> CallError {
    Exception::Fatal(Rc::new(FatalError {
        kind,
        text,
        cause: None,
    }))
    .into()
}
fn chain(messages: impl IntoIterator<Item = String>) -> HostError {
    let mut out = None;
    let items: Vec<_> = messages.into_iter().collect();
    for message in items.into_iter().rev() {
        let mut e = HostError::text(&message, &message);
        if let Some(c) = out {
            e = e.caused_by(Cause::Host(Box::new(c)));
        }
        out = Some(e);
    }
    out.unwrap_or_else(|| HostError::text("Ошибка выполнения запроса", "Query execution error"))
}
pub(crate) fn host_error(e: query_ir::HostError) -> HostError {
    let mut messages = vec![e.message];
    let mut next = e.cause;
    while let Some(e) = next {
        messages.push(e.message);
        next = e.cause;
    }
    chain(messages)
}
pub(crate) fn batch_error(ex: &Execution, text: &str, e: BatchError) -> CallError {
    let lang = match ex.lang() {
        bsl_vm::Lang::Ru => query_syntax::Lang::Ru,
        bsl_vm::Lang::En => query_syntax::Lang::En,
    };
    let script = match ex.script_variant() {
        bsl_vm::Lang::Ru => ScriptVariant::Russian,
        bsl_vm::Lang::En => ScriptVariant::English,
    };
    match e {
        BatchError::Compile(e) => {
            let s = e.render(text, lang, script);
            HostError::text(&s, &s).into()
        }
        BatchError::Exec(ExecError::Runtime(e) | ExecError::Rights(e)) => {
            chain(e.messages(lang, script)).into()
        }
        BatchError::Exec(ExecError::Host(e)) => {
            if e.payload
                .as_ref()
                .is_some_and(|p| p.is::<crate::host::PresentationBorrowConflict>())
            {
                internal(&e.message)
            } else {
                host_error(e).into()
            }
        }
        BatchError::Exec(ExecError::Later { what, .. }) => {
            HostError::not_implemented("Запрос", what).into()
        }
        BatchError::Exec(ExecError::Fatal(s)) => fatal(FatalKind::Internal, s),
        BatchError::Exec(e @ (ExecError::Memory(_) | ExecError::Structure(_))) => {
            fatal(FatalKind::Memory, e.to_string())
        }
    }
}
