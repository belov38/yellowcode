use super::{CallBindHost, to_value};
use crate::app::QueryCtx;
use crate::session::CallContext;
use crate::txn::CallTxn;
use crate::types::Category;
use crate::{AppError, Application, SessionUser};
use bsl_vm::{Execution, Value};
use ibschema::storage::{IndexId, StorageMap, TableId};
use ibschema::store::{ReadTxn, StoreError, WriteTxn};
use query::{QueryHost, QueryHostProvider};
use query_exec::{DataSource, ExecEnv, Row, RowIter, StoreSource, TempTables};
use query_ir::host::{PresentationInput, TxnId};
use query_ir::{
    Access, AccessPolicy, BindHost, ExecError, FieldSet, HostError, PredefinedName, QStr, QTableId,
    QType, QValue, SessionValues, SourceTable, StorageBinding,
};
use std::cell::{Cell, OnceCell, Ref};
use std::ops::Bound;
use std::rc::Rc;
use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, Ordering},
};

pub(crate) struct QueryCallState {
    app: Weak<Application>,
    active: Cell<bool>,
    cancel: Arc<AtomicBool>,
    txn: Rc<CallTxn>,
}
impl QueryCallState {
    pub(crate) fn new(app: Weak<Application>, txn: Rc<CallTxn>) -> Rc<Self> {
        Rc::new(Self {
            app,
            active: Cell::new(true),
            cancel: Arc::new(AtomicBool::new(false)),
            txn,
        })
    }
    pub(crate) fn end(&self) {
        self.active.set(false);
        self.cancel.store(true, Ordering::Relaxed);
    }
}

pub struct CallQueryProvider {
    app: Arc<Application>,
    ctx: Arc<QueryCtx>,
}
impl CallQueryProvider {
    pub fn new(app: Arc<Application>) -> crate::Result<Self> {
        let ctx = app
            .query_ctx_text()
            .map_err(|e| AppError::Unsupported(e.to_owned()))?
            .clone();
        Ok(Self { app, ctx })
    }
}
impl QueryHostProvider for CallQueryProvider {
    fn with_host(&self, ex: &mut Execution, f: &mut dyn FnMut(&mut Execution, &dyn QueryHost)) {
        let (call, user) = ex
            .host_data_ref()
            .and_then(|c| c.downcast_ref::<CallContext>())
            .map(|c| (Some(c.query.clone()), Some(c.sess.user().clone())))
            .unwrap_or((None, None));
        let cancel = call
            .as_ref()
            .map(|c| c.cancel.clone())
            .unwrap_or_else(|| Arc::new(AtomicBool::new(true)));
        let txn = call.as_ref().map(|c| c.txn.clone());
        let host = CallQueryHost {
            provider: self,
            call,
            session: SessionView { user },
            cancel,
            mem_limit: ex
                .options()
                .memory_limit
                .unwrap_or(u64::MAX)
                .min(self.app.options().query_mem_limit),
            lang: match ex.lang() {
                bsl_vm::Lang::Ru => query_syntax::Lang::Ru,
                bsl_vm::Lang::En => query_syntax::Lang::En,
            },
            snapshot: OnceCell::new(),
            txn: txn.as_deref(),
            own: OnceCell::new(),
            touched: Cell::new(false),
        };
        f(ex, &host);
    }
}

struct SessionView {
    user: Option<SessionUser>,
}
impl SessionView {
    fn admitted(&self) -> bool {
        self.user.as_ref().is_some_and(|u| u.no_users)
    }
}
impl AccessPolicy for SessionView {
    fn read_policy(&self, _: &SourceTable, _: &FieldSet) -> Result<Access, HostError> {
        if self.admitted() {
            Ok(Access::Unrestricted)
        } else {
            Err(HostError::new("Q7e-SessionSecurity / P6"))
        }
    }
}
impl SessionValues for SessionView {
    fn session_param(&self, _: &str) -> Result<QValue, HostError> {
        Err(HostError::new("Q7e-BindDemandResume / P0 + P6"))
    }
    fn separators(&self) -> Result<Vec<query_ir::SeparatorFilter>, HostError> {
        Ok(Vec::new())
    }
    fn user(&self) -> Option<&query_ir::UserRef> {
        None
    }
}
struct CallQueryHost<'a> {
    provider: &'a CallQueryProvider,
    call: Option<Rc<QueryCallState>>,
    session: SessionView,
    cancel: Arc<AtomicBool>,
    mem_limit: u64,
    lang: query_syntax::Lang,
    snapshot: OnceCell<Result<Box<dyn ReadTxn + Send>, StoreError>>,
    txn: Option<&'a CallTxn>,
    own: OnceCell<Option<Ref<'a, Box<dyn WriteTxn>>>>,
    touched: Cell<bool>,
}
fn later(what: &'static str, stage: &'static str) -> ExecError {
    ExecError::Later { what, stage }
}
impl<'a> CallQueryHost<'a> {
    fn own_writer(&self) -> Option<&Ref<'a, Box<dyn WriteTxn>>> {
        let txn = self.txn?;
        self.own.get_or_init(|| txn.writer_ref()).as_ref()
    }
}
impl CallQueryHost<'_> {
    fn lock_tables(&self, tables: &[TableId], for_update: bool) -> Result<(), HostError> {
        let Some(txn) = self.txn else {
            return Ok(());
        };
        crate::data_lock::lock_tables(&self.provider.app, txn, tables, for_update).map_err(|_| {
            let lang = match self.lang {
                query_syntax::Lang::Ru => platform_catalog::messages::Lang::Ru,
                query_syntax::Lang::En => platform_catalog::messages::Lang::En,
            };
            HostError::new(crate::errors::render(
                crate::errors::keys::DB_LOCKING,
                &[],
                lang,
            ))
        })
    }

    fn ready(&self) -> Result<(), ExecError> {
        if !self.call.as_ref().is_some_and(|c| {
            c.active.get()
                && c.app
                    .upgrade()
                    .is_some_and(|a| Arc::ptr_eq(&a, &self.provider.app))
        }) {
            return Err(later("Q7e-CallContext", "P1"));
        }
        if !self.session.admitted() {
            return Err(later("Q7e-SessionSecurity", "P6"));
        }
        if self
            .provider
            .ctx
            .map
            .tables()
            .iter()
            .any(|t| !t.separators.is_empty())
            && self.provider.app.options().area().is_none()
        {
            return Err(later("Q7e-SessionSeparators", "P6 + Q7c"));
        }
        Ok(())
    }

    fn reference_type(&self, t: &QType) -> Option<bsl_vm::TypeId> {
        let [atom] = t.refs.as_slice() else {
            return None;
        };
        if *t != QType::reference(*atom) {
            return None;
        }
        let app = &self.provider.app;
        let obj = app.model().by_uuid(self.schema().ref_info(*atom).object)?;
        app.types()?.type_of(obj, Category::Ref)
    }
}
impl BindHost for CallQueryHost<'_> {
    fn week_start(&self) -> Result<u32, HostError> {
        CallBindHost::new(&self.provider.app).week_start()
    }
    fn predefined_ref(&self, p: &PredefinedName) -> Result<QValue, HostError> {
        CallBindHost::in_call(&self.provider.app, self.txn).predefined_ref(p)
    }
    fn predefined_names(&self, t: QTableId) -> Result<Vec<([u8; 16], String)>, HostError> {
        CallBindHost::new(&self.provider.app).predefined_names(t)
    }
    fn touch_table(&self, t: TableId) -> Result<(), HostError> {
        self.ready().map_err(|e| HostError::new(e.to_string()))?;
        CallBindHost::new(&self.provider.app).touch_table(t)?;
        self.touched.set(true);
        Ok(())
    }
}
impl QueryHost for CallQueryHost<'_> {
    fn prepare_execution(&self, _: &mut Execution) -> Result<(), ExecError> {
        self.ready()
    }
    fn admit_batch(
        &self,
        batch: &query_syntax::ast::Batch,
        temp: &TempTables,
    ) -> Result<(), ExecError> {
        self.ready()?;
        if batch.stmts.len() > 1
            && self.provider.app.options().read_only_scope.is_none()
            && !super::admission::scalar_batch(batch, temp, &self.provider.ctx.schema)
        {
            return Err(later("Q7e-BatchSnapshot", "P4 native/ruling"));
        }
        Ok(())
    }
    fn schema(&self) -> &query_ir::schema::SchemaOwner {
        &self.provider.ctx.schema
    }
    fn storage(&self) -> &StorageBinding {
        &self.provider.ctx.binding
    }
    fn map(&self) -> &StorageMap {
        &self.provider.ctx.map
    }
    fn access(&self) -> &dyn AccessPolicy {
        &self.session
    }
    fn session(&self) -> &dyn SessionValues {
        &self.session
    }
    fn read_source(&self) -> Box<dyn DataSource + '_> {
        if let Err(error) = self.ready() {
            return Box::new(FailedSource {
                map: self.map(),
                error,
            });
        }
        if !self.touched.get() {
            return Box::new(FailedSource {
                map: self.map(),
                error: later("Q7e-SnapshotBeforeTouch", "P4 native/ruling"),
            });
        }
        if let Some(own) = self.own_writer() {
            let txn: &dyn ReadTxn = &***own;
            return Box::new(StoreSource::new(txn, self.map()));
        }
        match self
            .snapshot
            .get_or_init(|| self.provider.app.store().read())
        {
            Ok(txn) => Box::new(StoreSource::new(&**txn, self.map())),
            Err(e) => {
                let error = match e {
                    StoreError::Memory(m) => ExecError::Memory(*m),
                    StoreError::SnapshotTooOld => ExecError::Runtime(query_ir::RuntimeError {
                        chain: vec![query_syntax::QueryError::new(
                            query_syntax::MsgId::SELECT_ERROR_RUNTIMEERROR,
                            None,
                        )],
                    }),
                    _ => ExecError::Fatal(format!("query snapshot: {e}")),
                };
                Box::new(FailedSource {
                    map: self.map(),
                    error,
                })
            }
        }
    }
    fn txn(&self) -> Option<TxnId> {
        self.txn.and_then(CallTxn::id).map(TxnId)
    }
    fn txn_rolled_back(&self, t: TxnId) -> bool {
        self.txn.is_some_and(|txn| txn.was_rolled_back(t.0))
    }
    fn check_txn(&self) -> Result<(), HostError> {
        self.ready().map_err(|e| HostError::new(e.to_string()))?;
        if self.txn.is_some_and(CallTxn::poisoned) {
            let failure =
                query_syntax::QueryError::new(query_syntax::MsgId::SELECT_ERROR_RUNTIMEERROR, None);
            return Err(HostError::new(
                failure.message(self.lang, query_syntax::ScriptVariant::Russian),
            ));
        }
        Ok(())
    }
    fn runtime_failed(&self) {
        if let Some(txn) = self.txn {
            txn.poison();
        }
    }
    fn presentation(&self, ex: &mut Execution, p: &PresentationInput) -> Result<QStr, HostError> {
        super::presentation::text(&self.provider.app, ex, p)
            .ok_or_else(|| HostError::new("Q7e-PresentationSnapshot / Q7c + P6"))
    }
    fn lock_for_update(&self, tables: &[TableId]) -> Result<(), HostError> {
        self.lock_tables(tables, true)
    }
    fn lock_for_read(&self, tables: &[TableId]) -> Result<(), HostError> {
        self.lock_tables(tables, false)
    }
    fn lang(&self) -> query_syntax::Lang {
        self.lang
    }
    fn cancelled(&self) -> &AtomicBool {
        &self.cancel
    }
    fn env(&self) -> ExecEnv<'_> {
        ExecEnv {
            collation: &*self.provider.ctx.collation,
            native: None,
            cancel: &self.cancel,
            stop: None,
            mem_limit: self.mem_limit,
            max_number_limbs: 4096,
            memory: None,
            lease: None,
            evictor: None,
        }
    }
    fn to_value(&self, _: &mut Execution, v: &QValue) -> Result<Value, bsl_vm::HostError> {
        if let QValue::Type(t) = v
            && let Some(ty) = self.reference_type(t)
        {
            return Ok(Value::Type(ty));
        }
        to_value(&self.provider.app, v, None)
    }
    fn from_value(
        &self,
        _: &mut Execution,
        v: &Value,
    ) -> Result<Option<QValue>, bsl_vm::HostError> {
        let Value::Object(obj) = v else {
            return Ok(None);
        };
        let Some(r) = obj
            .as_any()
            .downcast_ref::<crate::values::refs::RefObject>()
        else {
            return Ok(None);
        };
        if !r.belongs_to(&self.provider.app) {
            return Err(bsl_vm::HostError::not_implemented(
                "Запрос",
                "Q7e-ForeignReference",
            ));
        }
        let table = r
            .table_number()
            .ok_or_else(|| bsl_vm::HostError::not_implemented("Запрос", "Q7e-ReferenceTable"))?;
        Ok(Some(QValue::Ref {
            table,
            id: r.r.id.0,
        }))
    }
}

struct FailedSource<'a> {
    map: &'a StorageMap,
    error: ExecError,
}
impl FailedSource<'_> {
    fn error(&self) -> ExecError {
        self.error.clone()
    }
}
impl DataSource for FailedSource<'_> {
    fn map(&self) -> &StorageMap {
        self.map
    }
    fn scan(&self, _: TableId) -> Result<RowIter<'_>, ExecError> {
        Err(self.error())
    }
    fn range(
        &self,
        _: TableId,
        _: IndexId,
        _: Bound<&[QValue]>,
        _: Bound<&[QValue]>,
        _: bool,
    ) -> Result<RowIter<'_>, ExecError> {
        Err(self.error())
    }
    fn get_by_id(&self, _: TableId, _: &QValue) -> Result<Option<Row>, ExecError> {
        Err(self.error())
    }
    fn row_count_estimate(&self, _: TableId) -> u64 {
        0
    }
}
