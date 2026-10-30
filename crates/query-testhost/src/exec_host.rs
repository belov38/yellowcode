use crate::batch_memory::BatchMemory;
use crate::host::TestBase;
use ibschema::storage::{StorageMap, TableId};
use ibschema::store::ReadTxn;
use query_exec::{DataSource, ExecEnv, ExecHost, StoreSource};
use query_ir::host::{
    Access, AccessPolicy, FieldSet, HostError, PredefinedName, PresentationInput, SeparatorFilter,
    SessionValues, SourceTable, TxnId, UserRef,
};
use query_ir::{BindHost, QStr, QTableId, QValue, QuerySchema, StorageBinding};
use query_syntax::Lang;
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

const LEGACY_MEM_LIMIT: u64 = 1 << 30;
const MAX_NUMBER_LIMBS: u64 = 4096;

struct Unrestricted;

impl AccessPolicy for Unrestricted {
    fn read_policy(&self, _: &SourceTable, _: &FieldSet) -> Result<Access, HostError> {
        Ok(Access::Unrestricted)
    }
}

struct NoSession;

impl SessionValues for NoSession {
    fn session_param(&self, name: &str) -> Result<QValue, HostError> {
        Err(HostError::new(format!(
            "session parameter {name} is not set"
        )))
    }
    fn separators(&self) -> Result<Vec<SeparatorFilter>, HostError> {
        Ok(Vec::new())
    }
    fn user(&self) -> Option<&UserRef> {
        None
    }
}

pub struct TestHost<'b> {
    pub base: &'b TestBase,
    snapshot: Box<dyn ReadTxn + Send>,
    pub txn: Cell<Option<TxnId>>,
    pub rolled_back: RefCell<HashSet<TxnId>>,
    pub poisoned: Cell<bool>,
    pub log: RefCell<Vec<String>>,
    next_txn: Cell<u64>,
    lang: Lang,
    cancel: AtomicBool,
    outer_cancel: Option<&'b AtomicBool>,
    memory: Option<&'b BatchMemory>,
    logging: bool,
    access: Unrestricted,
    session: NoSession,
    plan: query_exec::plan::PlanOptions,
    pub movements: bool,
}

impl<'b> TestHost<'b> {
    pub fn new(base: &'b TestBase, lang: Lang) -> Result<TestHost<'b>, String> {
        Self::make(base, lang, None)
    }

    pub fn with_cancel(
        base: &'b TestBase,
        lang: Lang,
        cancel: &'b AtomicBool,
    ) -> Result<TestHost<'b>, String> {
        Self::make(base, lang, Some(cancel))
    }

    pub fn with_memory(
        base: &'b TestBase,
        lang: Lang,
        cancel: &'b AtomicBool,
        memory: &'b BatchMemory,
    ) -> Result<TestHost<'b>, String> {
        let mut host = Self::make(base, lang, Some(cancel))?;
        host.memory = Some(memory);
        host.logging = false;
        Ok(host)
    }

    fn make(
        base: &'b TestBase,
        lang: Lang,
        outer_cancel: Option<&'b AtomicBool>,
    ) -> Result<TestHost<'b>, String> {
        let snapshot = base
            .store
            .read()
            .map_err(|e| format!("read transaction: {e}"))?;
        Ok(TestHost {
            base,
            snapshot,
            txn: Cell::new(None),
            rolled_back: RefCell::new(HashSet::new()),
            poisoned: Cell::new(false),
            log: RefCell::new(Vec::new()),
            next_txn: Cell::new(1),
            lang,
            cancel: AtomicBool::new(false),
            outer_cancel,
            memory: None,
            logging: true,
            access: Unrestricted,
            session: NoSession,
            plan: query_exec::plan::PlanOptions::default(),
            movements: false,
        })
    }

    #[doc(hidden)]
    pub fn with_plan(mut self, plan: query_exec::plan::PlanOptions) -> Self {
        self.plan = plan;
        self
    }

    pub fn begin_txn(&self) -> TxnId {
        let id = TxnId(self.next_txn.get());
        self.next_txn.set(id.0 + 1);
        self.txn.set(Some(id));
        id
    }

    pub fn rollback(&self) {
        if let Some(t) = self.txn.take() {
            self.rolled_back.borrow_mut().insert(t);
        }
        self.poisoned.set(false);
    }

    pub fn commit(&self) {
        self.txn.set(None);
        self.poisoned.set(false);
    }

    pub fn calls(&self) -> Vec<String> {
        self.log.borrow().clone()
    }

    fn cancel_flag(&self) -> &AtomicBool {
        self.outer_cancel.unwrap_or(&self.cancel)
    }

    fn note(&self, s: String) {
        if self.logging {
            self.log.borrow_mut().push(s);
        }
    }
}

impl BindHost for TestHost<'_> {
    fn predefined_ref(&self, p: &PredefinedName) -> Result<QValue, HostError> {
        self.base.predefined_ref(p)
    }

    fn week_start(&self) -> Result<u32, HostError> {
        self.base.week_start()
    }

    fn touch_table(&self, t: TableId) -> Result<(), HostError> {
        let name = self
            .base
            .map()
            .try_table(t)
            .map_or("?", |i| i.query_name.as_str());
        if self.logging {
            self.note(format!("touch_table({name})"));
        }
        self.base.touch_table(t)
    }

    fn predefined_names(&self, table: QTableId) -> Result<Vec<([u8; 16], String)>, HostError> {
        self.base.predefined_names(table)
    }
}

impl ExecHost for TestHost<'_> {
    fn schema(&self) -> &Arc<QuerySchema> {
        &self.base.schema
    }
    fn storage(&self) -> &StorageBinding {
        &self.base.storage
    }
    fn map(&self) -> &StorageMap {
        self.base.map()
    }
    fn access(&self) -> &dyn AccessPolicy {
        &self.access
    }
    fn session(&self) -> &dyn SessionValues {
        &self.session
    }
    fn read_source(&self) -> Box<dyn DataSource + '_> {
        self.note("read_source".to_string());
        let src = Box::new(StoreSource::new(&*self.snapshot, self.base.map()));
        if self.movements {
            let mut o = query_exec::register::SettingsOverride::owning(src);
            o.movements_everywhere();
            return Box::new(o);
        }
        src
    }
    fn txn(&self) -> Option<TxnId> {
        self.txn.get()
    }
    fn txn_rolled_back(&self, t: TxnId) -> bool {
        self.rolled_back.borrow().contains(&t)
    }
    fn check_txn(&self) -> Result<(), HostError> {
        self.note("check_txn".to_string());
        if self.poisoned.get() {
            return Err(HostError::new("Ошибка выполнения запроса"));
        }
        Ok(())
    }
    fn runtime_failed(&self) {
        self.note("runtime_failed".to_string());
        if self.txn.get().is_some() {
            self.poisoned.set(true);
        }
    }
    fn presentation(&self, p: &PresentationInput) -> Result<QStr, HostError> {
        self.note("presentation".to_string());
        self.base.presentation(p, self.lang)
    }
    fn lock_for_update(&self, _: &[TableId]) -> Result<(), HostError> {
        self.note("lock_for_update".to_string());
        Ok(())
    }
    fn lock_for_read(&self, _: &[TableId]) -> Result<(), HostError> {
        self.note("lock_for_read".to_string());
        Ok(())
    }
    fn lang(&self) -> Lang {
        self.lang
    }
    fn cancelled(&self) -> &AtomicBool {
        self.cancel_flag()
    }
    fn plan_options(&self) -> query_exec::plan::PlanOptions {
        self.plan
    }
    fn env(&self) -> ExecEnv<'_> {
        ExecEnv {
            collation: &*self.base.collation,
            native: self.memory.and_then(|m| {
                query_exec::NativeCollation::when_bound(
                    self.base.collation_locale.as_cstr(),
                    &m.memory.budget,
                )
            }),
            cancel: self.cancel_flag(),
            stop: None,
            mem_limit: if self.memory.is_some() {
                u64::MAX
            } else {
                LEGACY_MEM_LIMIT
            },
            max_number_limbs: MAX_NUMBER_LIMBS,
            memory: self.memory.map(|m| &m.memory),
            lease: self.memory.map(|m| &*m.exec),
            evictor: self.memory.map(BatchMemory::evictor),
        }
    }
}
