use crate::Application;
use crate::meta::{arg_value, member_name, no_field};
use crate::session::call_ctx;
use bsl_prim::fold_name;
use bsl_vm::{
    Arg, CallResult, Execution, HostError, HostObject, MemberId, Registry, TypeId, TypeIdx, Value,
};
use platform_catalog::catalog;
use std::any::Any;
use std::rc::Rc;
use std::sync::Arc;

const MANAGER: &str = "ФоновыеЗадания";

pub(crate) fn register(app: &Arc<Application>, reg: &mut Registry) {
    let c = catalog();
    let (Some(manager), Some(state)) = (
        c.find_type("МенеджерФоновыхЗаданий"),
        c.find_type("СостояниеФоновогоЗадания"),
    ) else {
        return;
    };
    let app = app.clone();
    reg.global_prop(
        MANAGER,
        "BackgroundJobs",
        Arc::new(move |_| {
            Ok(Value::Object(Rc::new(Manager {
                app: app.clone(),
                ty: manager,
                state,
            })))
        }),
    );
}

struct Manager {
    app: Arc<Application>,
    ty: TypeIdx,
    state: TypeIdx,
}

impl Manager {
    fn refuse(&self, what: &str) -> HostError {
        HostError::not_implemented(MANAGER, what)
    }

    fn admit(&self, ex: &mut Execution, member: &str) -> CallResult<()> {
        if call_ctx(ex).is_some_and(|c| c.sess.user().no_users) {
            Ok(())
        } else {
            Err(self
                .refuse(&format!("{member}: сеанс с пользователями"))
                .into())
        }
    }

    fn get_jobs(&self, ex: &mut Execution, args: &[Arg], member: &str) -> CallResult<Value> {
        if !matches!(args.first(), None | Some(Arg::Omitted)) {
            let v = arg_value(ex, args, 0)?;
            if !crate::recordset::is_structure(&v) {
                return Err(self.refuse(&format!("{member}: Отбор не Структура")).into());
            }
            let fields = bsl_stdlib::structure_fields(&v)
                .ok_or_else(|| self.refuse(&format!("{member}: Отбор не Структура")))?;
            if fields.len() != 1 {
                return Err(self
                    .refuse(&format!("{member}: Отбор с {} ключами", fields.len()))
                    .into());
            }
            for (key, value) in fields {
                match fold_name(&key).as_str() {
                    "состояние" | "state" => {
                        if value.type_id() != TypeId::of_catalog(self.state) {
                            return Err(self
                                .refuse(&format!("{member}: Отбор.{key}: значение"))
                                .into());
                        }
                    }
                    _ => return Err(self.refuse(&format!("{member}: Отбор.{key}")).into()),
                }
            }
        }
        Ok(bsl_stdlib::new_array(Vec::new()))
    }

    fn find_by_id(&self, ex: &mut Execution, args: &[Arg], member: &str) -> CallResult<Value> {
        let v = arg_value(ex, args, 0)?;
        if bsl_stdlib::values::uuid_bytes(&v).is_none() {
            return Err(self
                .refuse(&format!("{member}: значение не УникальныйИдентификатор"))
                .into());
        }
        Ok(Value::Undefined)
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
        Err(no_field(&m.0.to_string()).into())
    }

    fn set(&self, _: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        Err(no_field(&m.0.to_string()).into())
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        let Some((true, ru, en)) = member_name(catalog(), self.ty, m) else {
            return Err(no_field(&m.0.to_string()).into());
        };
        self.admit(ex, ru)?;
        match en {
            "GetBackgroundJobs" => self.get_jobs(ex, args, ru),
            "FindByUUID" => self.find_by_id(ex, args, ru),
            _ => Err(self.refuse(ru).into()),
        }
    }

    fn boost_hash(&self) -> Option<i32> {
        Some(0x6267_6a00 | i32::from(self.ty.0 as u8))
    }

    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        Some(
            other
                .as_any()
                .downcast_ref::<Manager>()
                .is_some_and(|o| Arc::ptr_eq(&o.app, &self.app)),
        )
    }
}
