use crate::manager::{Manager, busy};
use crate::{QueryHost, QueryHostProvider, VmExecHost, errors};
use bsl_prim::Str;
use bsl_stdlib::bind::{arg_str, arg_value};
use bsl_vm::{Arg, CallResult, Execution, HostError, HostObject, MemberId, TypeId, TypeIdx, Value};
use query_exec::{RunMode, TempTables, run_batch};
use query_ir::{ParamConst, ParamShape, ParamTypes, ParamValue, ParamValues};
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
        crate::manager::with_host(ex, &*self.provider, |ex, host| {
            host.prepare_execution(ex)
                .map_err(|e| errors::batch_error(ex, &text, query_exec::BatchError::Exec(e)))?;
            let params = Params::new(ex, host, &fields)?;
            if matches!(manager, Value::Undefined) && mode == RunMode::One {
                let batch =
                    query_ir::compile_batch(&text, query_syntax::ParseMode::Query, host.schema())
                        .map_err(|e| {
                        errors::batch_error(ex, &text, query_exec::BatchError::Compile(e))
                    })?;
                if batch.stmts.iter().any(|s|matches!(s,query_syntax::ast::Stmt::Query(q) if q.query.parts.iter().any(|p|p.into.is_some()))) {
                    return Err(HostError::text("Не установлен менеджер временных таблиц","Temporary tables manager is not set").into());
                }
            }
            let outcome = {
                let adapter = VmExecHost::new(ex, host);
                if let Value::Object(m) = &manager {
                    let manager = m
                        .as_any()
                        .downcast_ref::<Manager>()
                        .ok_or_else(|| errors::internal("invalid query manager"))?;
                    *manager.state.provider.borrow_mut() = Some(self.provider.clone());
                    let mut tables = manager.state.tables.try_borrow_mut().map_err(|_| busy())?;
                    run_batch(&adapter, &text, &params, &mut tables, mode)
                } else {
                    run_batch(&adapter, &text, &params, &mut TempTables::new(), mode)
                }
            }
            .map_err(|e| errors::batch_error(ex, &text, e))?;
            let mut values = Vec::new();
            for item in outcome {
                values.push(crate::result_obj::adapt(
                    ex,
                    host,
                    item,
                    self.provider.clone(),
                )?);
            }
            if mode == RunMode::One {
                Ok(values.pop().unwrap_or(Value::Undefined))
            } else {
                Ok(bsl_stdlib::new_array(values))
            }
        })
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
            "FindParameters" => {
                let text = self.text.borrow().to_string_lossy();
                let manager = self.manager.borrow().clone();
                crate::manager::with_host(ex, &*self.provider, |ex, host| {
                    let params = {
                        let adapter = VmExecHost::new(ex, host);
                        if let Value::Object(m) = &manager {
                            let m = m
                                .as_any()
                                .downcast_ref::<Manager>()
                                .ok_or_else(|| errors::internal("invalid manager"))?;
                            let mut tables = m.state.tables.try_borrow_mut().map_err(|_| busy())?;
                            tables.forget_rolled_back(&|t| host.txn_rolled_back(t));
                            query_exec::find_parameters(&adapter, &text, &tables)
                        } else {
                            query_exec::find_parameters(&adapter, &text, &TempTables::new())
                        }
                    }
                    .map_err(|e| {
                        errors::batch_error(ex, &text, query_exec::BatchError::Compile(e))
                    })?;
                    crate::params_desc::new(ex, host, &params)
                })
            }
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

struct Params(HashMap<String, crate::values::ConvertedParam>);
impl Params {
    fn new(
        ex: &mut Execution,
        host: &dyn QueryHost,
        fields: &[(String, Value)],
    ) -> CallResult<Self> {
        let mut out = HashMap::new();
        for (name, v) in fields {
            out.insert(name.to_lowercase(), crate::values::from_value(ex, host, v)?);
        }
        Ok(Self(out))
    }
}
impl ParamTypes for Params {
    fn shape(&self, name: &str) -> Option<ParamShape> {
        self.0.get(&name.to_lowercase()).map(|p| p.shape.clone())
    }
    fn constant(&self, name: &str) -> Option<ParamConst> {
        match self.0.get(&name.to_lowercase())?.value.as_ref()? {
            ParamValue::Value(v) => Some(ParamConst::Value(v.clone())),
            ParamValue::List(v) => Some(ParamConst::List(v.clone())),
            ParamValue::Table { .. } => None,
        }
    }
}
impl ParamValues for Params {
    fn get(&self, name: &str) -> Option<ParamValue> {
        self.0
            .get(&name.to_lowercase())
            .and_then(|p| p.value.clone())
    }
}
