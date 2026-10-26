use bsl_vm::{Execution, HostError as VmHostError, Value};
use ibschema::storage::{StorageMap, TableId};
use query_exec::{DataSource, ExecEnv};
use query_ir::host::{AccessPolicy, HostError, PresentationInput, SessionValues, TxnId};
use query_ir::{BindHost, QStr, QValue, QuerySchema, StorageBinding};
use query_syntax::Lang;
use std::sync::{Arc, atomic::AtomicBool};

pub trait QueryHost: BindHost {
    fn prepare_execution(&self, _: &mut Execution) -> Result<(), query_ir::ExecError> {
        Ok(())
    }
    fn admit_batch(
        &self,
        _: &query_syntax::ast::Batch,
        _: &query_exec::TempTables,
    ) -> Result<(), query_ir::ExecError> {
        Ok(())
    }
    fn schema(&self) -> &Arc<QuerySchema>;
    fn storage(&self) -> &StorageBinding;
    fn map(&self) -> &StorageMap;
    fn access(&self) -> &dyn AccessPolicy;
    fn session(&self) -> &dyn SessionValues;
    fn read_source(&self) -> Box<dyn DataSource + '_>;
    fn txn(&self) -> Option<TxnId>;
    fn txn_rolled_back(&self, t: TxnId) -> bool;
    fn check_txn(&self) -> Result<(), HostError>;
    fn runtime_failed(&self);
    fn presentation(&self, ex: &mut Execution, p: &PresentationInput) -> Result<QStr, HostError>;
    fn lock_for_update(&self, tables: &[TableId]) -> Result<(), HostError>;
    fn lock_for_read(&self, tables: &[TableId]) -> Result<(), HostError>;
    fn lang(&self) -> Lang;
    fn cancelled(&self) -> &AtomicBool;
    fn env(&self) -> ExecEnv<'_>;
    fn to_value(&self, ex: &mut Execution, v: &QValue) -> Result<Value, VmHostError>;
    #[allow(clippy::wrong_self_convention)]
    fn from_value(&self, ex: &mut Execution, v: &Value) -> Result<Option<QValue>, VmHostError>;
}

pub trait QueryHostProvider: Send + Sync {
    fn with_host(&self, ex: &mut Execution, f: &mut dyn FnMut(&mut Execution, &dyn QueryHost));
}

use query_exec::ExecHost;
use query_ir::{PredefinedName, QTableId};
use std::cell::RefCell;

pub(crate) struct PresentationBorrowConflict;

pub struct VmExecHost<'a> {
    ex: RefCell<&'a mut Execution>,
    host: &'a dyn QueryHost,
}
impl<'a> VmExecHost<'a> {
    pub fn new(ex: &'a mut Execution, host: &'a dyn QueryHost) -> Self {
        Self {
            ex: RefCell::new(ex),
            host,
        }
    }
}
impl BindHost for VmExecHost<'_> {
    fn predefined_ref(&self, p: &PredefinedName) -> Result<QValue, HostError> {
        self.host.predefined_ref(p)
    }
    fn touch_table(&self, t: TableId) -> Result<(), HostError> {
        self.host.touch_table(t)
    }
    fn predefined_names(&self, t: QTableId) -> Result<Vec<([u8; 16], String)>, HostError> {
        self.host.predefined_names(t)
    }
    fn week_start(&self) -> Result<u32, HostError> {
        self.host.week_start()
    }
}
impl ExecHost for VmExecHost<'_> {
    fn admit_batch(
        &self,
        batch: &query_syntax::ast::Batch,
        temp: &query_exec::TempTables,
    ) -> Result<(), query_ir::ExecError> {
        self.host.admit_batch(batch, temp)
    }
    fn schema(&self) -> &Arc<QuerySchema> {
        self.host.schema()
    }
    fn storage(&self) -> &StorageBinding {
        self.host.storage()
    }
    fn map(&self) -> &StorageMap {
        self.host.map()
    }
    fn access(&self) -> &dyn AccessPolicy {
        self.host.access()
    }
    fn session(&self) -> &dyn SessionValues {
        self.host.session()
    }
    fn read_source(&self) -> Box<dyn DataSource + '_> {
        self.host.read_source()
    }
    fn txn(&self) -> Option<TxnId> {
        self.host.txn()
    }
    fn txn_rolled_back(&self, t: TxnId) -> bool {
        self.host.txn_rolled_back(t)
    }
    fn check_txn(&self) -> Result<(), HostError> {
        self.host.check_txn()
    }
    fn runtime_failed(&self) {
        self.host.runtime_failed();
    }
    fn presentation(&self, p: &PresentationInput) -> Result<QStr, HostError> {
        let mut ex = self.ex.try_borrow_mut().map_err(|_| {
            let mut error = HostError::new("internal: reentrant query presentation");
            error.payload = Some(Arc::new(PresentationBorrowConflict));
            error
        })?;
        self.host.presentation(&mut ex, p)
    }
    fn lock_for_update(&self, t: &[TableId]) -> Result<(), HostError> {
        self.host.lock_for_update(t)
    }
    fn lock_for_read(&self, t: &[TableId]) -> Result<(), HostError> {
        self.host.lock_for_read(t)
    }
    fn lang(&self) -> Lang {
        self.host.lang()
    }
    fn cancelled(&self) -> &AtomicBool {
        self.host.cancelled()
    }
    fn env(&self) -> ExecEnv<'_> {
        self.host.env()
    }
}
