use crate::{TestBase, TestHost};
use bsl_prim::{Str, fold_name};
use bsl_vm::{
    Arg, CallResult, Execution, HostError, HostObject, MemberId, Registry, Runtime, TypeId, Value,
};
use ibschema::storage::{StorageMap, TableId};
use query_exec::{DataSource, ExecEnv, ExecHost};
use query_ir::host::{AccessPolicy, PredefinedName, PresentationInput, SessionValues};
use query_ir::{BindHost, QStr, QValue, QuerySchema, StorageBinding, TxnId};
use query_syntax::Lang;
use std::{
    any::Any,
    cell::{Cell, RefCell},
    collections::HashSet,
    rc::Rc,
    sync::{Arc, atomic::AtomicBool},
};

#[derive(Default)]
struct Session {
    txn: Cell<Option<TxnId>>,
    next: Cell<u64>,
    rolled_back: RefCell<HashSet<TxnId>>,
    poisoned: Cell<bool>,
}
fn session(ex: &mut Execution) -> Result<Rc<Session>, HostError> {
    if ex.host_data_ref().is_none() {
        ex.set_host_data(Box::new(Rc::new(Session::default())));
    }
    ex.host_data_ref()
        .and_then(|s| s.downcast_ref::<Rc<Session>>())
        .cloned()
        .ok_or_else(|| HostError::not_implemented("query-testhost", "foreign host_data"))
}
pub struct TestProvider {
    base: Arc<TestBase>,
}
impl TestProvider {
    pub fn new(base: Arc<TestBase>) -> Self {
        Self { base }
    }
    pub fn runtime(self: Arc<Self>) -> Runtime {
        let mut reg = Registry::new(platform_catalog::catalog());
        bsl_stdlib::register(&mut reg);
        for r in self.base.schema.refs() {
            reg.register_app_type(bsl_vm::AppTypeInfo {
                name_ru: r.xml_name.clone(),
                name_en: r.xml_name.clone(),
                presentation_ru: r.xml_name.clone(),
                presentation_en: r.xml_name.clone(),
                guid: *r.type_uuid.as_bytes(),
                xdto: None,
                template: None,
                default_value: None,
            });
        }
        for (ru, en) in [
            ("НачатьТранзакцию", "BeginTransaction"),
            ("ОтменитьТранзакцию", "RollbackTransaction"),
            ("ЗафиксироватьТранзакцию", "CommitTransaction"),
        ] {
            reg.global_fn(
                ru,
                en,
                Arc::new(move |ex, _| {
                    let s = session(ex)?;
                    match en {
                        "BeginTransaction" => {
                            let n = s.next.get().saturating_add(1);
                            s.next.set(n);
                            s.txn.set(Some(TxnId(n)));
                        }
                        "RollbackTransaction" => {
                            if let Some(t) = s.txn.take() {
                                s.rolled_back.borrow_mut().insert(t);
                            }
                            s.poisoned.set(false);
                        }
                        _ => {
                            s.txn.set(None);
                            s.poisoned.set(false);
                        }
                    }
                    Ok(Value::Undefined)
                }),
            );
        }
        query::register(&mut reg, self);
        Runtime::new(platform_catalog::catalog(), reg, Arc::new(TestApp))
    }
}
impl query::QueryHostProvider for TestProvider {
    fn with_host(
        &self,
        ex: &mut Execution,
        f: &mut dyn FnMut(&mut Execution, &dyn query::QueryHost),
    ) {
        let state = session(ex);
        let lang = match ex.lang() {
            bsl_vm::Lang::Ru => Lang::Ru,
            bsl_vm::Lang::En => Lang::En,
        };
        let host = TestHost::new(&self.base, lang);
        let local = VmHost {
            base: &self.base,
            inner: host,
            state,
        };
        f(ex, &local);
    }
}
struct VmHost<'a> {
    base: &'a TestBase,
    inner: Result<TestHost<'a>, String>,
    state: Result<Rc<Session>, HostError>,
}
impl VmHost<'_> {
    fn host(&self) -> &TestHost<'_> {
        match &self.inner {
            Ok(h) => h,
            Err(_) => unreachable_host(),
        }
    }
    fn state(&self) -> Option<&Session> {
        self.state.as_ref().ok().map(|s| &**s)
    }
}
#[allow(
    clippy::panic,
    reason = "engine hooks are reachable only after prepare_execution accepted the snapshot"
)]
fn unreachable_host() -> ! {
    panic!("test query host used before successful preparation")
}
impl BindHost for VmHost<'_> {
    fn predefined_ref(&self, p: &PredefinedName) -> Result<QValue, query_ir::HostError> {
        self.base.predefined_ref(p)
    }
    fn touch_table(&self, t: TableId) -> Result<(), query_ir::HostError> {
        self.host().touch_table(t)
    }
    fn predefined_names(
        &self,
        t: query_ir::QTableId,
    ) -> Result<Vec<([u8; 16], String)>, query_ir::HostError> {
        self.base.predefined_names(t)
    }
    fn week_start(&self) -> Result<u32, query_ir::HostError> {
        self.base.week_start()
    }
}
impl query::QueryHost for VmHost<'_> {
    fn prepare_execution(&self, _: &mut Execution) -> Result<(), query_ir::ExecError> {
        if let Err(e) = &self.inner {
            return Err(query_ir::ExecError::Host(query_ir::HostError::new(e)));
        }
        if self.state.is_err() {
            return Err(query_ir::ExecError::Host(query_ir::HostError::new(
                "query-testhost: foreign host_data",
            )));
        }
        Ok(())
    }
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
        self.host().access()
    }
    fn session(&self) -> &dyn SessionValues {
        self.host().session()
    }
    fn read_source(&self) -> Box<dyn DataSource + '_> {
        self.host().read_source()
    }
    fn txn(&self) -> Option<TxnId> {
        self.state().and_then(|s| s.txn.get())
    }
    fn txn_rolled_back(&self, t: TxnId) -> bool {
        self.state()
            .is_some_and(|s| s.rolled_back.borrow().contains(&t))
    }
    fn check_txn(&self) -> Result<(), query_ir::HostError> {
        if self.state().is_some_and(|s| s.poisoned.get()) {
            Err(query_ir::HostError::new("Ошибка выполнения запроса"))
        } else {
            Ok(())
        }
    }
    fn runtime_failed(&self) {
        if let Some(s) = self.state()
            && s.txn.get().is_some()
        {
            s.poisoned.set(true);
        }
    }
    fn presentation(
        &self,
        _: &mut Execution,
        p: &PresentationInput,
    ) -> Result<QStr, query_ir::HostError> {
        self.host().presentation(p)
    }
    fn lock_for_update(&self, t: &[TableId]) -> Result<(), query_ir::HostError> {
        self.host().lock_for_update(t)
    }
    fn lock_for_read(&self, t: &[TableId]) -> Result<(), query_ir::HostError> {
        self.host().lock_for_read(t)
    }
    fn lang(&self) -> Lang {
        self.host().lang()
    }
    fn cancelled(&self) -> &AtomicBool {
        self.host().cancelled()
    }
    fn env(&self) -> ExecEnv<'_> {
        self.host().env()
    }
    fn to_value(&self, ex: &mut Execution, v: &QValue) -> Result<Value, HostError> {
        match v {
            QValue::Ref { table, id } => {
                let atom = self.base.by_number.get(table).ok_or_else(|| {
                    HostError::not_implemented("query-testhost", "reference number")
                })?;
                let name = &self.base.schema.ref_info(*atom).xml_name;
                let ty = ex.runtime().types().by_name(name).ok_or_else(|| {
                    HostError::not_implemented("query-testhost", "reference type")
                })?;
                Ok(Value::Object(Rc::new(TestRef {
                    ty,
                    table: *table,
                    id: *id,
                    name: name.clone(),
                })))
            }
            QValue::Type(t) if t.refs.len() == 1 => {
                let atom = t.refs.first().ok_or_else(|| {
                    HostError::not_implemented("query-testhost", "empty reference type")
                })?;
                ex.runtime()
                    .types()
                    .by_name(&self.base.schema.ref_info(*atom).xml_name)
                    .map(Value::Type)
                    .ok_or_else(|| HostError::not_implemented("query-testhost", "reference type"))
            }
            QValue::Type(t) => {
                let Some(query_ir::Other::SysEnum(t)) = t.others.first() else {
                    return Err(HostError::not_implemented("query-testhost", "applied type"));
                };
                Ok(Value::Type(TypeId::of_catalog(*t)))
            }
            QValue::SysEnum(t, i) => {
                let name = platform_catalog::catalog()
                    .type_info(*t)
                    .values
                    .get(*i as usize)
                    .map(|v| v.name_en.as_str())
                    .ok_or_else(|| HostError::not_implemented("query-testhost", "enum value"))?;
                bsl_stdlib::enums::enum_value(*t, name)
                    .ok_or_else(|| HostError::not_implemented("query-testhost", "enum value"))
            }
            _ => Err(HostError::not_implemented(
                "query-testhost",
                "applied result value",
            )),
        }
    }
    fn from_value(&self, ex: &mut Execution, v: &Value) -> Result<Option<QValue>, HostError> {
        if let Some(r) = v
            .as_object()
            .and_then(|o| o.as_any().downcast_ref::<TestRef>())
        {
            return Ok(Some(QValue::Ref {
                table: r.table,
                id: r.id,
            }));
        }
        if let Some(e) = bsl_stdlib::enums::as_enum_value(v) {
            return Ok(Some(QValue::SysEnum(e.type_idx(), e.index() as u32)));
        }
        if let Value::Type(t) = v {
            let name = ex.runtime().types().name(*t, bsl_vm::Lang::En);
            if let Some(i) = self
                .base
                .schema
                .refs()
                .iter()
                .position(|r| r.xml_name == name)
            {
                return Ok(Some(QValue::Type(query_ir::QType::reference(
                    query_ir::RefAtom(i as u32),
                ))));
            }
        }
        Ok(None)
    }
}
pub struct TestRef {
    ty: TypeId,
    table: u32,
    id: [u8; 16],
    name: String,
}
impl HostObject for TestRef {
    fn type_id(&self) -> TypeId {
        self.ty
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, _: &mut Execution, _: MemberId) -> CallResult<Value> {
        Err(HostError::not_implemented("TestRef", "property").into())
    }
    fn set(&self, _: &mut Execution, _: MemberId, _: Value) -> CallResult<()> {
        Err(HostError::not_implemented("TestRef", "property").into())
    }
    fn call(&self, _: &mut Execution, _: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        Err(HostError::not_implemented("TestRef", "method").into())
    }
    fn presentation(&self, _: &mut Execution) -> CallResult<Option<Str>> {
        Ok(Some(Str::from(format!(
            "{}({})",
            self.name,
            query_ir::uuid_of(&self.id)
        ))))
    }
    fn equals(&self, o: &dyn HostObject) -> Option<bool> {
        Some(
            o.as_any()
                .downcast_ref::<Self>()
                .is_some_and(|o| o.ty == self.ty && o.table == self.table && o.id == self.id),
        )
    }
    fn boost_hash(&self) -> Option<i32> {
        let word = |at: usize| {
            let mut bytes = [0u8; 4];
            for (i, b) in bytes.iter_mut().enumerate() {
                *b = self.id.get(at + i).copied().unwrap_or(0);
            }
            u32::from_le_bytes(bytes)
        };
        Some(
            self.table
                .wrapping_add(word(0).wrapping_mul(5))
                .wrapping_add(word(12).wrapping_mul(25)) as i32,
        )
    }
    fn is_empty_value(&self) -> bool {
        self.id == [0; 16]
    }
}
struct TestApp;
impl bsl_vm::AppHost for TestApp {
    fn names(&self) -> &dyn bsl_vm::AppNames {
        self
    }
    fn global_value(
        &self,
        _: &mut Execution,
        _: &bsl_vm::AppGlobal,
    ) -> Result<Value, bsl_vm::Exception> {
        Ok(bsl_testhost::stand_object())
    }
    fn module(
        &self,
        ex: &mut Execution,
        _: bsl_vm::ModuleRef,
    ) -> Result<bsl_vm::ModuleInstanceRef, bsl_vm::Exception> {
        Err(ex.error(HostError::not_implemented("testhost", "module")))
    }
    fn module_ref(&self, _: &str) -> Option<bsl_vm::ModuleRef> {
        None
    }
}
impl bsl_vm::AppNames for TestApp {
    fn global_value(&self, name: &str, _: bsl_compiler::ExecContext) -> Option<bsl_vm::AppGlobal> {
        (fold_name(name) == fold_name(bsl_testhost::wrapper::STAND_GLOBAL))
            .then(|| bsl_vm::AppGlobal::CommonModule { name: name.into() })
    }
    fn global_method(&self, _: &str, _: bsl_compiler::ExecContext) -> Option<bsl_vm::AppMethodSig> {
        None
    }
    fn context_value(&self, _: &str, _: &str) -> Option<bsl_vm::ContextValue> {
        None
    }
    fn context_method(&self, _: &str, _: &str) -> bool {
        false
    }
}
