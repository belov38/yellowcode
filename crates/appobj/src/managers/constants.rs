use crate::Application;
use crate::exchange_params::ExchangeParams;
use crate::model::{FieldDef, ObjId, TypeAtom};
use crate::read::{internal, read_view, store_call_error};
use crate::read_budget as budget;
use crate::txn::{CallTxn, Commit};
use crate::values::field::from_field;
use crate::values::refs::empty_ref;
use bsl_prim::{BslDate, Number, Str};
use bsl_vm::{
    Arg, CallResult, Exception, Execution, HostError, HostObject, MemberId, ObjRef, RetainedMemory,
    TypeId, Value,
};
use ibschema::fields::FieldValue;
use ibschema::store::StoreResult;
use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

fn refusal(what: &str) -> HostError {
    HostError::not_implemented("КонстантаМенеджер", &format!("Получить: {what}"))
}

fn default_value(ex: &mut Execution, app: &Application, field: &FieldDef) -> CallResult<Value> {
    let types = &field.types;
    Ok(match types.single() {
        Some(TypeAtom::Bool) => Value::Bool(false),
        Some(TypeAtom::Number) => Value::Number(Number::zero()),
        Some(TypeAtom::String) => {
            let pad = types
                .string
                .filter(|q| q.fixed)
                .map_or(0, |q| q.length as usize);
            budget::reserve(ex, (pad as u64).saturating_mul(4))?;
            Value::String(Str::from(" ".repeat(pad).as_str()))
        }
        Some(TypeAtom::Date) => Value::Date(BslDate::EMPTY),
        Some(TypeAtom::Uuid) => bsl_stdlib::values::new_uuid([0; 16]),
        Some(TypeAtom::Ref(obj)) => empty_ref(app, *obj)?,
        Some(TypeAtom::ValueStorage) => bsl_stdlib::new_value_storage(ex, &Value::Undefined, None)?,
        _ => return Err(refusal("составной тип без строки").into()),
    })
}

pub(crate) fn get(ex: &mut Execution, app: &Application, obj: ObjId) -> CallResult<Value> {
    let md = app.model().obj(obj).md;
    if crate::rights::data_read(ex, app, md)? == crate::rights::DataRead::Denied {
        return Err(crate::rights::access_violation().into());
    }
    read_value(ex, app, obj)
}

fn get_by_manager(ex: &mut Execution, app: &Application, obj: ObjId) -> CallResult<Value> {
    let md = app.model().obj(obj).md;
    if crate::rights::data_read(ex, app, md)? == crate::rights::DataRead::Denied {
        return Err(HostError::not_implemented(
            "P6",
            "МенеджерЗначенияКонстанты.Прочитать: без права Чтение в сеансе с пользователями",
        )
        .into());
    }
    read_value(ex, app, obj)
}

fn read_value(ex: &mut Execution, app: &Application, obj: ObjId) -> CallResult<Value> {
    let model = app.model();
    let (Some(st), Some(field)) = (model.storage_of(obj), model.obj(obj).value.as_ref()) else {
        return Err(internal("the constant is not bound to a table").into());
    };
    let Some(value) = st.value else {
        return Err(internal("the table of the constant has no field of the value").into());
    };
    let table = app
        .store()
        .storage()
        .try_table(st.main)
        .ok_or_else(|| internal("no table"))?;
    if !table.separators.is_empty() && app.options().area().is_none() {
        return Err(refusal("Разделение данных").into());
    }

    let call_txn = CallTxn::of_call(ex);
    let mut view = read_view(ex, call_txn.as_deref(), app)?;
    let remaining = ex
        .options()
        .memory_limit
        .unwrap_or(u64::MAX)
        .saturating_sub(ex.retained_workspace_bytes());
    let memory = view
        .read_memory(remaining)
        .map_err(|e| store_call_error(ex, e))?;
    let txn = view.txn();
    let mut rows = txn.scan(st.main).map_err(|e| store_call_error(ex, e))?;
    let Some(first) = rows.next() else {
        return if table.separators.is_empty() {
            Ok(Value::Undefined)
        } else {
            default_value(ex, app, field)
        };
    };
    let (_, row) = first.map_err(|e| store_call_error(ex, e))?;
    match rows.next() {
        None => {}
        Some(Ok(_)) => {
            return Err(internal("the table of the constant holds more than one row").into());
        }
        Some(Err(e)) => return Err(store_call_error(ex, e)),
    }

    let cell = row
        .get(usize::from(value.0))
        .ok_or_else(|| internal("the row is shorter than its table"))?;
    if let FieldValue::Storage(bytes) = cell {
        budget::admit(
            ex,
            &memory,
            budget::stored_row(&row).saturating_add(budget::storage_copy(bytes)),
        )?;
        if let Some(e) = crate::values::storage_check::storage_refusal(app, bytes) {
            return Err(e.into());
        }
        return Ok(bsl_stdlib::value_storage_from_bytes(Arc::from(
            bytes.as_slice(),
        )));
    }
    budget::admit(
        ex,
        &memory,
        budget::stored_row(&row).saturating_add(budget::conversion_peak(cell)),
    )?;
    Ok(from_field(app, field, cell)?)
}

fn write_refusal(member: &str, what: &str) -> HostError {
    HostError::not_implemented("КонстантаМенеджер", &format!("{member}: {what}"))
}

pub(crate) fn write(
    ex: &mut Execution,
    app: &Application,
    obj: ObjId,
    member: &str,
    value: Value,
    params: &Rc<ExchangeParams>,
) -> CallResult<()> {
    let model = app.model();
    let def = model.obj(obj);
    let (Some(st), Some(field)) = (model.storage_of(obj), def.value.as_ref()) else {
        return Err(internal("the constant is not bound to a table").into());
    };
    let Some(vfield) = st.value else {
        return Err(internal("the table of the constant has no field of the value").into());
    };
    if app.options().read_only_scope.is_some() {
        return Err(write_refusal(member, "приложение только для чтения (read_scope)").into());
    }
    if !crate::session::call_ctx(ex).is_some_and(|c| c.sess.user().no_users) {
        return Err(HostError::not_implemented("P6", "application write security provider").into());
    }
    let table = app
        .store()
        .storage()
        .try_table(st.main)
        .ok_or_else(|| internal("no table"))?;
    if !table.separators.is_empty() {
        return Err(write_refusal(member, "разделённая константа").into());
    }
    if app
        .modules()
        .of_object(obj, md_model::ModuleKind::ValueManagerModule)
        .is_some()
        || crate::events::has_write_subscriptions(app, def, "ValueManager")
    {
        return Err(write_refusal(member, "обработчики записи").into());
    }
    let cast = cast_value(ex, app, field, value)?;
    let cell = crate::values::field::to_field(app, field, &cast)?;
    let txn =
        CallTxn::of_call(ex).ok_or_else(|| internal("a write of a constant outside a call"))?;
    txn.check().map_err(|e| e.host())?;
    let registration =
        crate::changes::Registration::for_write(app, obj, params, params.auto_fill())?;
    let change_key = match &registration {
        Some(r) => r.changes.key_constant(def.md)?,
        None => Vec::new(),
    };
    txn.begin();
    let mut nodes = Vec::new();
    let result = (|| -> CallResult<()> {
        if crate::data_lock::lock_for_write(app, &txn, def).is_err() {
            return Err(crate::data_lock::conflict().into());
        }
        let found = txn
            .with_write(app, |w| -> StoreResult<bool> {
                let rows = w
                    .scan(st.main)?
                    .collect::<StoreResult<Vec<(ibschema::store::RowHandle, ibschema::Row)>>>()?;
                let [(h, row)] = rows.as_slice() else {
                    return Ok(false);
                };
                let mut row = row.clone();
                if let Some(slot) = row.get_mut(usize::from(vfield.0)) {
                    *slot = cell.clone();
                }
                w.update(st.main, *h, &row)?;
                if let Some(r) = &registration {
                    nodes = r.nodes(&*w)?;
                    r.apply(w, &nodes, &change_key)?;
                }
                Ok(true)
            })
            .map_err(|e| e.call_error(ex))?;
        if !found {
            return Err(write_refusal(member, "в таблице константы не одна строка").into());
        }
        crate::commit_guard::guard(app, &txn, obj);
        Ok(())
    })();
    let ended = match result {
        Ok(()) => crate::fo_version::commit(ex, app, &txn),
        Err(e) => {
            let _ = txn.rollback(app);
            Err(e)
        }
    };
    if matches!(ended, Ok(Commit::Committed | Commit::Inner))
        && let Some(r) = &registration
    {
        r.done(&nodes);
    }
    ended.map(drop)
}

fn cast_value(
    ex: &mut Execution,
    app: &Application,
    field: &FieldDef,
    v: Value,
) -> CallResult<Value> {
    if matches!(field.types.single(), Some(TypeAtom::ValueStorage))
        && bsl_stdlib::value_storage_bytes(&v).is_some()
    {
        return Ok(v);
    }
    crate::values::typedesc::cast_to_field(app, ex, field, v)
}

pub(crate) fn set(
    ex: &mut Execution,
    app: &Application,
    obj: ObjId,
    args: &[Arg],
) -> CallResult<Value> {
    let v = crate::meta::arg_value(ex, args, 0)?;
    let params = ExchangeParams::new()?;
    write(ex, app, obj, "Установить", v, &params)?;
    Ok(Value::Undefined)
}

pub(crate) struct ValueManager {
    app: Arc<Application>,
    obj: ObjId,
    ty: TypeId,
    value: RefCell<Option<Value>>,
    exchange: RefCell<Option<Rc<ExchangeParams>>>,
}

impl ValueManager {
    pub(crate) fn create(app: &Arc<Application>, obj: ObjId) -> CallResult<Value> {
        let ty = app
            .types()
            .and_then(|t| t.type_of(obj, crate::types::Category::ValueManager))
            .ok_or_else(|| internal("the constant has no type of its value manager"))?;
        Ok(Value::Object(Rc::new(ValueManager {
            app: app.clone(),
            obj,
            ty,
            value: RefCell::new(None),
            exchange: RefCell::new(None),
        })))
    }

    fn refuse(&self, member: &str) -> HostError {
        HostError::not_implemented("КонстантаМенеджерЗначения", member)
    }

    fn member(&self, ex: &Execution, m: MemberId) -> Option<(bool, &'static str)> {
        let t = ex.runtime().types().app_type(self.ty)?.template?;
        crate::meta::member_name(platform_catalog::catalog(), t, m).map(|(f, ru, _)| (f, ru))
    }

    fn exchange(&self) -> Result<Rc<ExchangeParams>, HostError> {
        if let Some(e) = self.exchange.borrow().clone() {
            return Ok(e);
        }
        let e = ExchangeParams::new()?;
        *self.exchange.borrow_mut() = Some(e.clone());
        Ok(e)
    }
}

impl HostObject for ValueManager {
    fn retained_memory(&self, visit: &mut dyn RetainedMemory) -> Result<bool, Exception> {
        visit.work(1)?;
        visit.mutable();
        if let Some(v) = self.value.borrow().as_ref() {
            visit.value(v)?;
        }
        if let Some(e) = self.exchange.borrow().clone() {
            let e: ObjRef = e;
            visit.value(&Value::Object(e))?;
        }
        Ok(true)
    }

    fn type_id(&self) -> TypeId {
        self.ty
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        match self.member(ex, m) {
            Some((false, "Значение")) => self
                .value
                .borrow()
                .clone()
                .ok_or_else(|| self.refuse("Значение: не прочитано и не присвоено").into()),
            Some((false, "ОбменДанными")) => {
                let e: ObjRef = self.exchange()?;
                Ok(Value::Object(e))
            }
            Some((_, ru)) => Err(self.refuse(ru).into()),
            None => Err(self.refuse(&m.0.to_string()).into()),
        }
    }

    fn set(&self, ex: &mut Execution, m: MemberId, v: Value) -> CallResult<()> {
        match self.member(ex, m) {
            Some((false, "Значение")) => {
                let field = self
                    .app
                    .model()
                    .obj(self.obj)
                    .value
                    .as_ref()
                    .ok_or_else(|| internal("a constant without a value field"))?;
                let cast = cast_value(ex, &self.app, field, v)?;
                budget::reserve(ex, budget::value_heap(&cast))?;
                *self.value.borrow_mut() = Some(cast);
                Ok(())
            }
            Some((_, ru)) => Err(self.refuse(ru).into()),
            None => Err(self.refuse(&m.0.to_string()).into()),
        }
    }

    fn call(&self, ex: &mut Execution, m: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        match self.member(ex, m) {
            Some((true, "Прочитать")) => {
                let v = get_by_manager(ex, &self.app, self.obj)?;
                *self.value.borrow_mut() = Some(v);
                Ok(Value::Undefined)
            }
            Some((true, "Записать")) => {
                let v =
                    self.value.borrow().clone().ok_or_else(|| {
                        self.refuse("Записать: значение не прочитано и не присвоено")
                    })?;
                let params = self.exchange()?;
                write(ex, &self.app, self.obj, "Записать", v, &params)?;
                Ok(Value::Undefined)
            }
            Some((_, ru)) => Err(self.refuse(ru).into()),
            None => Err(self.refuse(&m.0.to_string()).into()),
        }
    }
}
