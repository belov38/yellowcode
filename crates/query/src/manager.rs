use crate::{QueryHost, QueryHostProvider, columns, errors, query_obj::member};
use bsl_vm::{Arg, CallResult, Execution, HostError, HostObject, MemberId, TypeId, TypeIdx, Value};
use query_exec::{TempTableId, TempTables};
use query_ir::ColumnInfo;
use std::{any::Any, cell::RefCell, rc::Rc, sync::Arc};

pub(crate) fn busy() -> HostError {
    HostError::text(
        "Менеджер временных таблиц уже используется",
        "Temporary tables manager is already in use",
    )
}
fn missing() -> HostError {
    HostError::text("Таблица не найдена", "Table not found")
}
pub(crate) struct ManagerState {
    pub tables: RefCell<TempTables>,
    pub provider: RefCell<Option<Arc<dyn QueryHostProvider>>>,
}
pub(crate) struct Manager {
    ty: TypeIdx,
    pub state: Rc<ManagerState>,
}
impl Manager {
    pub(crate) fn construct(ty: TypeIdx) -> Value {
        Value::Object(Rc::new(Self {
            ty,
            state: Rc::new(ManagerState {
                tables: RefCell::new(TempTables::new()),
                provider: RefCell::new(None),
            }),
        }))
    }
}
pub(crate) fn with_host<T>(
    ex: &mut Execution,
    provider: &dyn QueryHostProvider,
    mut f: impl FnMut(&mut Execution, &dyn QueryHost) -> CallResult<T>,
) -> CallResult<T> {
    let mut calls = 0;
    let mut result = None;
    provider.with_host(ex, &mut |ex, host| {
        calls += 1;
        if calls == 1 {
            result = Some(f(ex, host));
        }
    });
    if calls != 1 {
        return Err(errors::internal(
            "QueryHostProvider must call its callback exactly once",
        ));
    }
    result.ok_or_else(|| errors::internal("query provider returned no outcome"))?
}
impl ManagerState {
    fn sync(&self, ex: &mut Execution) -> CallResult<()> {
        let provider = self.provider.borrow().clone();
        if let Some(p) = provider {
            with_host(ex, &*p, |_, h| {
                self.tables
                    .try_borrow_mut()
                    .map_err(|_| busy())?
                    .forget_rolled_back(&|t| h.txn_rolled_back(t));
                Ok(())
            })?;
        }
        Ok(())
    }
    fn names(&self, ex: &mut Execution) -> CallResult<Vec<String>> {
        self.sync(ex)?;
        let t = self.tables.try_borrow().map_err(|_| busy())?;
        Ok(t.names()
            .iter()
            .filter_map(|n| t.table(n).map(|t| t.schema.name.clone()))
            .collect())
    }
    fn handle(self: &Rc<Self>, ex: &mut Execution, key: &Value) -> CallResult<Value> {
        let names = self.names(ex)?;
        let i = columns::position(key, &names)?;
        let name = names.get(i).ok_or_else(columns::bounds)?;
        let id = self
            .tables
            .try_borrow()
            .map_err(|_| busy())?
            .id_of(name)
            .ok_or_else(missing)?;
        Ok(Value::Object(Rc::new(Table {
            ty: columns::idx("QueryTempTable")?,
            state: self.clone(),
            id,
        })))
    }
}
impl HostObject for Manager {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.ty)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        match member(self.ty, m, false) {
            "Tables" => Ok(Value::Object(Rc::new(Tables {
                ty: columns::idx("QueryTempTables")?,
                state: self.state.clone(),
            }))),
            _ => Err(columns::unavailable(self.ty, m, false).into()),
        }
    }
    fn set(&self, _: &mut Execution, _: MemberId, _: Value) -> CallResult<()> {
        Err(columns::readonly().into())
    }
    fn call(&self, _: &mut Execution, m: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        match member(self.ty, m, true) {
            "Close" => {
                self.state
                    .tables
                    .try_borrow_mut()
                    .map_err(|_| busy())?
                    .close();
                Ok(Value::Undefined)
            }
            _ => Err(columns::unavailable(self.ty, m, true).into()),
        }
    }
}
struct Tables {
    ty: TypeIdx,
    state: Rc<ManagerState>,
}
impl HostObject for Tables {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.ty)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        Err(columns::unavailable(self.ty, m, false).into())
    }
    fn set(&self, _: &mut Execution, _: MemberId, _: Value) -> CallResult<()> {
        Err(columns::readonly().into())
    }
    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        match member(self.ty, m, true) {
            "Count" => Ok(Value::from(self.state.names(ex)?.len() as i64)),
            "Get" => {
                let key = bsl_stdlib::bind::arg_value(ex, args.first())?;
                self.state.handle(ex, &key)
            }
            "Find" => {
                let key = bsl_stdlib::bind::arg_value(ex, args.first())?;
                let names = self.state.names(ex)?;
                if columns::position(&key, &names).is_err() {
                    Ok(Value::Undefined)
                } else {
                    self.state.handle(ex, &key)
                }
            }
            _ => Err(columns::unavailable(self.ty, m, true).into()),
        }
    }
    fn index_get(&self, ex: &mut Execution, key: &Value) -> CallResult<Value> {
        self.state.handle(ex, key)
    }
    fn iter_len(&self, ex: &mut Execution) -> Option<usize> {
        self.state.names(ex).ok().map(|n| n.len())
    }
    fn iter_get(&self, ex: &mut Execution, i: usize) -> CallResult<Value> {
        self.state.handle(ex, &Value::from(i as i64))
    }
}
struct Table {
    ty: TypeIdx,
    state: Rc<ManagerState>,
    id: TempTableId,
}
impl Table {
    fn schema(&self, ex: &mut Execution) -> CallResult<Arc<query_ir::TempSchema>> {
        self.state.sync(ex)?;
        let tables = self.state.tables.try_borrow().map_err(|_| busy())?;
        let table = tables.table_by_id(self.id).ok_or_else(missing)?;
        Ok(table.schema.clone())
    }
    fn snapshot(
        &self,
        ex: &mut Execution,
    ) -> CallResult<(Arc<query_ir::TempSchema>, Vec<Vec<query_ir::QValue>>)> {
        self.state.sync(ex)?;
        let tables = self.state.tables.try_borrow().map_err(|_| busy())?;
        let table = tables.table_by_id(self.id).ok_or_else(missing)?;
        let bytes = table.rows.iter().fold(
            query_exec::model::vec_block(
                table.rows.len(),
                std::mem::size_of::<Vec<query_ir::QValue>>(),
            ),
            |bytes, row| bytes.saturating_add(query_exec::ops::row_bytes(row)),
        );
        ex.reserve(bytes)?;
        ex.charge(table.rows.len() as u64)?;
        Ok((table.schema.clone(), table.rows.clone()))
    }
    fn columns(schema: &query_ir::TempSchema) -> Vec<ColumnInfo> {
        schema
            .columns
            .iter()
            .map(|c| ColumnInfo {
                name: c.name.clone(),
                ty: c.ty.clone().unwrap_or_default().without_null(),
                width: 0,
                nested: None,
            })
            .collect()
    }
    fn read(&self, ex: &mut Execution, data: bool) -> CallResult<Value> {
        let (schema, rows) = if data {
            self.snapshot(ex)?
        } else {
            (self.schema(ex)?, Vec::new())
        };
        let cols = Self::columns(&schema);
        let p = self
            .state
            .provider
            .borrow()
            .clone()
            .ok_or_else(|| errors::internal("temporary table host unavailable"))?;
        let mut rows = Some(rows);
        with_host(ex, &*p, |ex, host| {
            if data {
                crate::result_obj::materialized(
                    ex,
                    host,
                    Arc::new(query_exec::QueryResult {
                        columns: cols.clone(),
                        rows: rows
                            .take()
                            .ok_or_else(|| errors::internal("query snapshot already consumed"))?,
                        totals: None,
                    }),
                    None,
                )
            } else {
                columns::columns(ex, host, &cols, true)
            }
        })
    }
}
impl HostObject for Table {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.ty)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        match member(self.ty, m, false) {
            "FullName" => Ok(Value::from(self.schema(ex)?.name.as_str())),
            "Columns" => self.read(ex, false),
            _ => Err(columns::unavailable(self.ty, m, false).into()),
        }
    }
    fn set(&self, _: &mut Execution, _: MemberId, _: Value) -> CallResult<()> {
        Err(columns::readonly().into())
    }
    fn call(&self, ex: &mut Execution, m: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        match member(self.ty, m, true) {
            "GetData" => self.read(ex, true),
            _ => Err(columns::unavailable(self.ty, m, true).into()),
        }
    }
}
