use crate::{QueryHost, QueryHostProvider, VmExecHost, errors};
use bsl_prim::Str;
use bsl_stdlib::bind::{arg_str, arg_value};
use bsl_vm::{Arg, CallResult, Execution, HostError, HostObject, MemberId, TypeId, TypeIdx, Value};
use query_exec::{RunMode, TempTables, run_batch};
use query_ir::{ParamConst, ParamShape, ParamTypes, ParamValue, ParamValues, QType, QValue};
use std::{any::Any, cell::RefCell, collections::HashMap, rc::Rc, sync::Arc};

pub(crate) fn member(t: TypeIdx, m: MemberId, method: bool) -> &'static str {
    let info = platform_catalog::catalog().type_info(t);
    let Ok(i) = usize::try_from(m.0) else {
        return "";
    };
    if method {
        i.checked_sub(info.properties.len())
            .and_then(|i| info.methods.get(i))
            .map_or("", |m| m.name_en.as_str())
    } else {
        info.properties.get(i).map_or("", |p| p.name_en.as_str())
    }
}
fn unavailable(t: TypeIdx, m: MemberId, method: bool) -> HostError {
    HostError::not_implemented(
        &platform_catalog::catalog().type_info(t).name_ru,
        member(t, m, method),
    )
}
fn busy() -> HostError {
    HostError::text(
        "Менеджер временных таблиц уже используется",
        "Temporary tables manager is already in use",
    )
}

pub(crate) struct Manager {
    ty: TypeIdx,
    tables: RefCell<TempTables>,
}
impl Manager {
    pub(crate) fn construct(ty: TypeIdx) -> Value {
        Value::Object(Rc::new(Self {
            ty,
            tables: RefCell::new(TempTables::new()),
        }))
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
        Err(unavailable(self.ty, m, false).into())
    }
    fn set(&self, _: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        Err(unavailable(self.ty, m, false).into())
    }
    fn call(&self, _: &mut Execution, m: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        if member(self.ty, m, true) == "Close" {
            self.tables.try_borrow_mut().map_err(|_| busy())?.close();
            return Ok(Value::Undefined);
        }
        Err(unavailable(self.ty, m, true).into())
    }
}

pub(crate) struct Query {
    ty: TypeIdx,
    text: RefCell<Str>,
    params: Value,
    manager: RefCell<Value>,
    provider: Arc<dyn QueryHostProvider>,
}
impl Query {
    pub(crate) fn construct(
        ex: &mut Execution,
        args: &mut [Arg],
        ty: TypeIdx,
        provider: Arc<dyn QueryHostProvider>,
    ) -> CallResult<Value> {
        let text = arg_str(ex, args.first())?;
        Ok(Value::Object(Rc::new(Self {
            ty,
            text: RefCell::new(text),
            params: bsl_stdlib::new_structure(vec![]),
            manager: RefCell::new(Value::Undefined),
            provider,
        })))
    }
    fn execute(&self, ex: &mut Execution, mode: RunMode) -> CallResult<Value> {
        let text = self.text.borrow().to_string_lossy();
        let fields = bsl_stdlib::structure_fields(&self.params)
            .ok_or_else(|| errors::internal("query parameters are not a structure"))?;
        let manager = self.manager.borrow().clone();
        let mut result = None;
        let mut calls = 0;
        self.provider.with_host(ex, &mut |ex, host| {
            calls += 1;
            if calls != 1 {
                return;
            }
            result = Some((|| {
                host.prepare_execution(ex)
                    .map_err(|e| errors::batch_error(ex, &text, query_exec::BatchError::Exec(e)))?;
                let params = Params::new(ex, host, &fields)?;
                let outcome = {
                    let adapter = VmExecHost::new(ex, host);
                    match &manager {
                        Value::Object(m) => {
                            let manager = m
                                .as_any()
                                .downcast_ref::<Manager>()
                                .ok_or_else(|| errors::internal("invalid query manager"))?;
                            let mut tables = manager.tables.try_borrow_mut().map_err(|_| busy())?;
                            run_batch(&adapter, &text, &params, &mut tables, mode)
                        }
                        _ => run_batch(&adapter, &text, &params, &mut TempTables::new(), mode),
                    }
                };
                outcome.map_err(|e| errors::batch_error(ex, &text, e))?;
                Err(HostError::not_implemented("Запрос", "РезультатЗапроса (Q3 Task 14)").into())
            })());
        });
        if calls != 1 {
            return Err(errors::internal(
                "QueryHostProvider must call its callback exactly once",
            ));
        }
        result.ok_or_else(|| errors::internal("query provider returned no outcome"))?
    }
}
impl HostObject for Query {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.ty)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        match member(self.ty, m, false) {
            "Text" => Ok(Value::String(self.text.borrow().clone())),
            "Parameters" => Ok(self.params.clone()),
            "TempTablesManager" => Ok(self.manager.borrow().clone()),
            _ => Err(unavailable(self.ty, m, false).into()),
        }
    }
    fn set(&self, ex: &mut Execution, m: MemberId, v: Value) -> CallResult<()> {
        match member(self.ty, m, false) {
            "Text" => {
                let text = arg_str(ex, Some(&Arg::Val(v)))?;
                *self.text.borrow_mut() = text;
                Ok(())
            }
            "TempTablesManager" => {
                if !matches!(&v, Value::Undefined)
                    && !matches!(&v, Value::Object(m) if m.as_any().is::<Manager>())
                {
                    return Err(HostError::key("core85:IDS_PROPERTY_TYPE_MISMATCH").into());
                }
                *self.manager.borrow_mut() = v;
                Ok(())
            }
            _ => Err(unavailable(self.ty, m, false).into()),
        }
    }
    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        match member(self.ty, m, true) {
            "SetParameter" => {
                let name = arg_str(ex, args.first())?;
                let value = arg_value(ex, args.get(1))?;
                ex.call_method(
                    &self.params,
                    "Insert",
                    &mut [Arg::Val(Value::String(name)), Arg::Val(value)],
                    false,
                )?;
                Ok(Value::Undefined)
            }
            "Execute" => self.execute(ex, RunMode::One),
            "ExecuteBatch" => self.execute(ex, RunMode::Batch),
            "ExecuteBatchWithIntermediateData" => self.execute(ex, RunMode::Intermediate),
            _ => Err(unavailable(self.ty, m, true).into()),
        }
    }
}

struct Params(HashMap<String, (QValue, ParamShape)>);
impl Params {
    fn new(
        ex: &mut Execution,
        host: &dyn QueryHost,
        fields: &[(String, Value)],
    ) -> CallResult<Self> {
        let mut out = HashMap::new();
        for (name, v) in fields {
            let value = match v {
                Value::Undefined => QValue::Undefined,
                Value::Null => QValue::Null,
                Value::Bool(b) => QValue::Bool(*b),
                Value::Number(n) => QValue::Number(n.clone()),
                Value::String(s) => QValue::String(query_ir::QStr(s.units().into())),
                Value::Date(d) => QValue::Date(*d),
                _ => host.from_value(ex, v)?.ok_or_else(|| {
                    HostError::not_implemented("Запрос", "параметры этого типа (Q3 Task 13)")
                })?,
            };
            let ty = match &value {
                QValue::Undefined => QType::undefined(),
                QValue::Null => QType::null_only(),
                QValue::Bool(_) => QType::boolean(),
                QValue::Number(_) => QType::num(query_ir::NumQ {
                    digits: 0,
                    scale: 0,
                    nonneg: false,
                }),
                QValue::String(_) => QType::str_(query_ir::StrQ {
                    len: 0,
                    fixed: false,
                }),
                QValue::Date(_) => QType::date(query_ir::DateQ::DateTime),
                QValue::Ref { table, .. } => host
                    .storage()
                    .ref_atoms()
                    .iter()
                    .find(|(n, _)| n == table)
                    .map(|(_, r)| QType::reference(*r))
                    .ok_or_else(|| {
                        HostError::not_implemented("Запрос", "неизвестный тип ссылки")
                    })?,
                _ => {
                    return Err(HostError::not_implemented(
                        "Запрос",
                        "параметры этого типа (Q3 Task 13)",
                    )
                    .into());
                }
            };
            out.insert(name.to_lowercase(), (value, ParamShape::Value(ty)));
        }
        Ok(Self(out))
    }
}
impl ParamTypes for Params {
    fn shape(&self, name: &str) -> Option<ParamShape> {
        self.0.get(&name.to_lowercase()).map(|(_, s)| s.clone())
    }
    fn constant(&self, name: &str) -> Option<ParamConst> {
        self.0
            .get(&name.to_lowercase())
            .map(|(v, _)| ParamConst::Value(v.clone()))
    }
}
impl ParamValues for Params {
    fn get(&self, name: &str) -> Option<ParamValue> {
        self.0
            .get(&name.to_lowercase())
            .map(|(v, _)| ParamValue::Value(v.clone()))
    }
}
