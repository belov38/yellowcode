use crate::Application;
use crate::session::{AppKind, call_ctx};
use bsl_vm::{
    Arg, CallResult, Execution, HostError, HostObject, MemberId, Registry, TypeId, Value,
};
use platform_catalog::{TypeIdx, catalog};
use std::{any::Any, rc::Rc, sync::Arc};

struct SessionInfo {
    ty: TypeIdx,
    app: AppKind,
}

impl HostObject for SessionInfo {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.ty)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, _: &mut Execution, member: MemberId) -> CallResult<Value> {
        let property = catalog()
            .types()
            .get(usize::from(self.ty.0))
            .and_then(|t| t.properties.get(member.0 as usize));
        match property.map(|p| p.name_en.as_str()) {
            Some("ApplicationName") => match self.app {
                AppKind::HttpService => Ok(Value::from("HTTPServiceConnection")),
                AppKind::BackgroundJob => Ok(Value::from("BackgroundJob")),
                AppKind::Cli => Err(HostError::not_implemented(
                    "InfoBaseSession",
                    "ApplicationName (CLI)",
                )
                .into()),
            },
            _ => Err(HostError::not_implemented(
                "InfoBaseSession",
                property.map_or("?", |p| p.name_en.as_str()),
            )
            .into()),
        }
    }
    fn set(&self, _: &mut Execution, _: MemberId, _: Value) -> CallResult<()> {
        Err(HostError::key(bsl_stdlib::bind::msg::PROP_NOT_WRITABLE)
            .raw()
            .into())
    }
    fn call(&self, _: &mut Execution, member: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        match crate::meta::member_name(catalog(), self.ty, member) {
            Some((true, _, "GetBackgroundJob")) => background_job(self.app),
            other => Err(HostError::not_implemented(
                "InfoBaseSession",
                other.map_or("?", |(_, _, en)| en),
            )
            .into()),
        }
    }
}

fn background_job(app: AppKind) -> CallResult<Value> {
    match app {
        AppKind::HttpService => Ok(Value::Undefined),
        AppKind::BackgroundJob => {
            Err(HostError::not_implemented("InfoBaseSession", "GetBackgroundJob").into())
        }
        AppKind::Cli => {
            Err(HostError::not_implemented("InfoBaseSession", "GetBackgroundJob (CLI)").into())
        }
    }
}

fn number(ex: &mut Execution, _: &mut [Arg]) -> CallResult<Value> {
    let context = call_ctx(ex).ok_or_else(outside_call)?;
    Ok(Value::Number(bsl_prim::Number::from_i64(i64::from(
        context.sess.number(),
    ))))
}

pub(crate) fn outside_call() -> HostError {
    HostError::text(
        "internal: session information outside of a call",
        "internal: session information outside of a call",
    )
}

fn run_mode(ex: &mut Execution, _: &mut [Arg]) -> CallResult<Value> {
    match call_ctx(ex).ok_or_else(outside_call)?.sess.app() {
        AppKind::HttpService | AppKind::BackgroundJob => Ok(Value::Undefined),
        AppKind::Cli => Err(HostError::not_implemented(
            "Глобальный контекст",
            "ТекущийРежимЗапуска (CLI)",
        )
        .into()),
    }
}

fn refused(name: &str, what: &str) -> HostError {
    HostError::not_implemented("Глобальный контекст", &format!("{name} ({what})"))
}

fn set_safe_mode_disabled(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    const NAME: &str = "УстановитьОтключениеБезопасногоРежима";
    let value = match args.first() {
        None | Some(Arg::Omitted) => return Err(refused(NAME, "без параметра").into()),
        Some(Arg::Val(v)) => v.clone(),
        Some(Arg::Place(p)) => p.get(ex)?,
    };
    let Value::Bool(disabled) = value else {
        return Err(refused(NAME, "параметр не Булево").into());
    };
    call_ctx(ex)
        .ok_or_else(outside_call)?
        .sess
        .safe_mode_disabled = disabled;
    Ok(Value::Undefined)
}

fn get_safe_mode_disabled(ex: &mut Execution, _: &mut [Arg]) -> CallResult<Value> {
    let flag = call_ctx(ex)
        .ok_or_else(outside_call)?
        .sess
        .safe_mode_disabled;
    Ok(Value::Bool(flag && !ex.effective_flags().safe_mode))
}

pub(crate) fn register(app: &Application, reg: &mut Registry) {
    reg.global_fn("ТекущийРежимЗапуска", "CurrentRunMode", Arc::new(run_mode));
    reg.global_fn(
        "УстановитьОтключениеБезопасногоРежима",
        "SetSafeModeDisabled",
        Arc::new(set_safe_mode_disabled),
    );
    reg.global_fn(
        "ПолучитьОтключениеБезопасногоРежима",
        "GetSafeModeDisabled",
        Arc::new(get_safe_mode_disabled),
    );
    let connection_string = app.options().connection_string.clone();
    reg.global_fn(
        "СтрокаСоединенияИнформационнойБазы",
        "InfoBaseConnectionString",
        Arc::new(move |_, _| match &connection_string {
            Some(text) => Ok(Value::from(text.as_str())),
            None => Err(HostError::not_implemented(
                "Глобальный контекст",
                "СтрокаСоединенияИнформационнойБазы",
            )
            .into()),
        }),
    );
    reg.global_fn(
        "НомерСеансаИнформационнойБазы",
        "InfoBaseSessionNumber",
        Arc::new(number),
    );
    reg.global_fn(
        "НомерСоединенияИнформационнойБазы",
        "InfoBaseConnectionNumber",
        Arc::new(number),
    );
    let Some(ty) = catalog().find_type("InfoBaseSession") else {
        return;
    };
    reg.global_fn(
        "ПолучитьТекущийСеансИнформационнойБазы",
        "GetCurrentInfoBaseSession",
        Arc::new(move |ex, _| {
            let app = call_ctx(ex).ok_or_else(outside_call)?.sess.app();
            Ok(Value::Object(Rc::new(SessionInfo { ty, app })))
        }),
    );
}
