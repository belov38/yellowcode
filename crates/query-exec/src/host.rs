use crate::source::{DataSource, ExecEnv};
use ibschema::storage::{StorageMap, TableId};
use query_ir::host::{AccessPolicy, HostError, PresentationInput, SessionValues, TxnId};
use query_ir::{BindHost, QStr, QuerySchema, StorageBinding};
use query_syntax::Lang;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

pub trait ExecHost: BindHost {
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
    fn presentation(&self, p: &PresentationInput) -> Result<QStr, HostError>;
    fn lock_for_update(&self, tables: &[TableId]) -> Result<(), HostError>;
    fn lock_for_read(&self, tables: &[TableId]) -> Result<(), HostError>;
    fn lang(&self) -> Lang;
    fn cancelled(&self) -> &AtomicBool;
    fn env(&self) -> ExecEnv<'_>;
    #[doc(hidden)]
    fn plan_options(&self) -> crate::plan::PlanOptions {
        crate::plan::PlanOptions::default()
    }
}
