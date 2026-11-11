use crate::Application;
use crate::meta::{arg_value, type_mismatch};
use crate::txn::CallTxn;
use bsl_vm::{Arg, CallResult, Execution, HostError, Registry, Value};
use std::rc::Rc;
use std::sync::Arc;

fn outside_call() -> HostError {
    let t = "internal: the transaction functions outside of a call";
    HostError::text(t, t)
}

fn txn_of(ex: &mut Execution) -> Result<Rc<CallTxn>, HostError> {
    CallTxn::of_call(ex).ok_or_else(outside_call)
}

fn begin(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    let control = match arg_value(ex, args, 0)? {
        Value::Undefined => None,
        mode => Some(crate::data_lock::control_of(&mode).ok_or_else(|| type_mismatch(1))?),
    };
    txn_of(ex)?.begin_with(control);
    Ok(Value::Undefined)
}

fn commit(app: &Application, ex: &mut Execution) -> CallResult<Value> {
    let txn = txn_of(ex)?;
    txn.commit(app).map_err(|e| e.call_error(ex))?;
    Ok(Value::Undefined)
}

fn rollback(app: &Application, ex: &mut Execution) -> CallResult<Value> {
    let txn = txn_of(ex)?;
    txn.rollback(app).map_err(|e| e.call_error(ex))?;
    Ok(Value::Undefined)
}

fn active(ex: &mut Execution, _: &mut [Arg]) -> CallResult<Value> {
    Ok(Value::Bool(
        CallTxn::of_call(ex).is_some_and(|txn| txn.active()),
    ))
}

pub(crate) fn register(app: &Arc<Application>, reg: &mut Registry) {
    reg.global_fn("НачатьТранзакцию", "BeginTransaction", Arc::new(begin));
    let on_commit = app.clone();
    reg.global_fn(
        "ЗафиксироватьТранзакцию",
        "CommitTransaction",
        Arc::new(move |ex, _| commit(&on_commit, ex)),
    );
    let on_rollback = app.clone();
    reg.global_fn(
        "ОтменитьТранзакцию",
        "RollbackTransaction",
        Arc::new(move |ex, _| rollback(&on_rollback, ex)),
    );
    reg.global_fn("ТранзакцияАктивна", "TransactionActive", Arc::new(active));
}
