use crate::Application;
use crate::txn::{CallTxn, Commit};
use bsl_vm::{CallError, CallResult, Execution, HostError, Value};
use ibschema::store::StoreError;
use query_ir::convert::date_from_q;

const TABLE: &str = "FILES";
const NAME: &str = "fo_version";

fn new_version() -> [u8; 16] {
    crate::history::random_v4().to_bytes_le()
}

pub(crate) fn commit(
    ex: &mut Execution,
    app: &Application,
    txn: &CallTxn,
) -> Result<Commit, CallError> {
    if txn.depth() == 1 && !txn.poisoned() && txn.options_pending() {
        if let Err(e) = rewrite(ex, app, txn) {
            let _ = txn.rollback(app);
            return Err(e);
        }
        txn.options_written();
    }
    txn.commit(app).map_err(|e| e.call_error(ex))
}

fn rewrite(ex: &mut Execution, app: &Application, txn: &CallTxn) -> CallResult<()> {
    let now = match ex.call_global("ТекущаяДата", &mut [])? {
        Value::Date(d) => d,
        _ => return Err(HostError::text("internal: ТекущаяДата", "internal: ТекущаяДата").into()),
    };
    let Some(modified) = date_from_q(now) else {
        return Err(HostError::text("internal: ТекущаяДата", "internal: ТекущаяДата").into());
    };
    let version = new_version();
    let written = txn
        .with_write(app, |w| {
            match w.rewrite_system_file(TABLE, NAME, &version, modified) {
                Ok(()) => Ok(None),
                Err(StoreError::NotFound(_)) => Ok(Some(format!("нет файла {TABLE}/{NAME}"))),
                Err(StoreError::Unsupported(why)) => Ok(Some(why)),
                Err(e) => Err(e),
            }
        })
        .map_err(|e| e.call_error(ex))?;
    if let Some(why) = written {
        txn.refuse_commit(format!("версия функциональных опций: {why}"));
    }
    Ok(())
}
