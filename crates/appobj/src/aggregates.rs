use crate::Application;
use crate::meta::member_name;
use crate::model::{ObjDef, ObjId};
use crate::read::{read_view, store_call_error, store_error};
use crate::session::call_ctx;
use crate::totals::{service, std_value};
use crate::txn::CallTxn;
use bsl_prim::Str;
use bsl_vm::{Arg, CallResult, Execution, HostError, HostObject, MemberId, TypeId, TypeIdx, Value};
use ibschema::fields::FieldValue;
use ibschema::names::{StdField, role};
use ibschema::store::ReadTxn;
use platform_catalog::catalog;
use platform_catalog::messages::Lang;
use std::any::Any;
use std::rc::Rc;
use std::sync::Arc;

pub(crate) fn serves(ru: &str) -> bool {
    matches!(
        ru,
        "ПолучитьРежимАгрегатов" | "ПолучитьИспользованиеАгрегатов" | "ПолучитьАгрегаты"
    )
}

pub(crate) fn manager_call(
    ex: &mut Execution,
    app: &Arc<Application>,
    obj: ObjId,
    ru: &str,
) -> CallResult<Value> {
    let def = app.model().obj(obj);
    let refuse = |what: &str| {
        HostError::not_implemented(
            &format!("{}Менеджер", def.kind.name(Lang::Ru)),
            &format!("{ru}: {what}"),
        )
    };
    if !call_ctx(ex).is_some_and(|c| c.sess.user().no_users) {
        return Err(refuse("сеанс с пользователями").into());
    }
    let settings = read_settings(ex, app, def, &refuse)?;
    match ru {
        "ПолучитьРежимАгрегатов" => {
            if settings.agg_mode {
                return Err(refuse("режим агрегатов включен").into());
            }
            Ok(Value::Bool(false))
        }
        "ПолучитьИспользованиеАгрегатов" => {
            if !settings.enable_use {
                return Err(refuse("использование агрегатов выключено").into());
            }
            Ok(Value::Bool(true))
        }
        _ => {
            if settings.agg_mode {
                return Err(refuse("режим агрегатов включен").into());
            }
            if !settings.enable_use {
                return Err(refuse("использование агрегатов выключено").into());
            }
            if settings.listed {
                return Err(refuse("список агрегатов не пуст").into());
            }
            let t = catalog()
                .find_type("ИнформацияОбАгрегатах")
                .ok_or_else(|| refuse("нет типа ИнформацияОбАгрегатах"))?;
            Ok(Value::Object(Rc::new(AggregatesInfo { ty: t })))
        }
    }
}

struct Settings {
    agg_mode: bool,
    enable_use: bool,
    listed: bool,
}

fn read_settings(
    ex: &mut Execution,
    app: &Arc<Application>,
    def: &ObjDef,
    refuse: &dyn Fn(&str) -> HostError,
) -> CallResult<Settings> {
    let (Some(opt), Some(grid)) = (
        service(app, def, role::ACCUMULATION_REGISTERS_AGGREGATES_OPTIONS),
        service(app, def, role::ACCUMULATION_REGISTERS_AGGREGATE_LIST),
    ) else {
        return Err(refuse("у регистра нет настроек агрегатов").into());
    };
    let map = app.store().storage();
    let info = map.table(opt);
    let call_txn = CallTxn::of_call(ex);
    let mut view = read_view(ex, call_txn.as_deref(), app)?;
    let txn: &mut dyn ReadTxn = view.txn();
    let mut row = None;
    for r in txn.scan(opt).map_err(|e| store_call_error(ex, e))? {
        let (_, r) = r.map_err(|e| store_error(&e))?;
        if !matches!(std_value(info, &r, StdField::RegId), Some(FieldValue::Uuid(u)) if *u == def.md)
        {
            return Err(refuse("строка настроек агрегатов другого регистра").into());
        }
        if row.is_some() {
            return Err(refuse("несколько строк настроек агрегатов").into());
        }
        row = Some(r);
    }
    let row = row.ok_or_else(|| refuse("нет строки настроек агрегатов"))?;
    let flag = |std: StdField, name: &str| match std_value(info, &row, std) {
        Some(FieldValue::Bool(b)) => Ok(*b),
        _ => Err(refuse(&format!("поле {name} настроек агрегатов"))),
    };
    let agg_mode = flag(StdField::AggMode, "AggMode")?;
    let enable_use = flag(StdField::EnableUse, "EnableUse")?;
    let listed = match txn.scan(grid).map_err(|e| store_call_error(ex, e))?.next() {
        None => false,
        Some(r) => {
            r.map_err(|e| store_error(&e))?;
            true
        }
    };
    Ok(Settings {
        agg_mode,
        enable_use,
        listed,
    })
}

struct AggregatesInfo {
    ty: TypeIdx,
}

impl AggregatesInfo {
    fn refuse(&self, m: MemberId) -> HostError {
        let name = member_name(catalog(), self.ty, m).map_or("член", |(_, ru, _)| ru);
        HostError::not_implemented("ИнформацияОбАгрегатах", name)
    }
}

impl HostObject for AggregatesInfo {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.ty)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        Err(self.refuse(m).into())
    }

    fn set(&self, _: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        Err(self.refuse(m).into())
    }

    fn call(&self, _: &mut Execution, m: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        Err(self.refuse(m).into())
    }

    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<Str>> {
        Ok(Some(Str::from(
            ex.runtime()
                .types()
                .presentation(TypeId::of_catalog(self.ty), ex.lang()),
        )))
    }
}
