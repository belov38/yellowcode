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
    fn call(&self, _: &mut Execution, _: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        Err(HostError::not_implemented("InfoBaseSession", "method").into())
    }
}

fn number(ex: &mut Execution, _: &mut [Arg]) -> CallResult<Value> {
    let context = call_ctx(ex).ok_or_else(outside_call)?;
    Ok(Value::Number(bsl_prim::Number::from_i64(i64::from(
        context.sess.number(),
    ))))
}

fn outside_call() -> HostError {
    HostError::text(
        "internal: session information outside of a call",
        "internal: session information outside of a call",
    )
}

pub(crate) fn register(reg: &mut Registry) {
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
