use crate::collections::{ctor_not_implemented, not_implemented, property_name, type_name};
use bsl_prim::Str;
use bsl_vm::{Arg, CallResult, Execution, HostObject, MemberId, Registry, TypeId, TypeImpl, Value};
use platform_catalog::{TypeIdx, catalog};
use std::{any::Any, rc::Rc, sync::Arc};

struct SystemInfo {
    ty: TypeIdx,
    hash: i32,
}

impl HostObject for SystemInfo {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.ty)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, _: &mut Execution, member: MemberId) -> CallResult<Value> {
        match property_name(self.ty, member) {
            Some("AppVersion") => Ok(Value::from(catalog().platform())),
            _ => Err(not_implemented(self.ty, member).raw().into()),
        }
    }
    fn set(&self, _: &mut Execution, member: MemberId, _: Value) -> CallResult<()> {
        Err(not_implemented(self.ty, member).raw().into())
    }
    fn call(&self, _: &mut Execution, member: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        Err(not_implemented(self.ty, member).raw().into())
    }
    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<Str>> {
        Ok(Some(type_name(ex, self.ty)))
    }
    fn boost_hash(&self) -> Option<i32> {
        Some(self.hash)
    }
}

pub(crate) fn register(reg: &mut Registry) {
    let Some(ty) = catalog().find_type("SystemInfo") else {
        return;
    };
    reg.type_impl(
        ty,
        TypeImpl {
            ctor: Some(Arc::new(move |_, args| {
                if !args.is_empty() {
                    return Err(ctor_not_implemented(ty).into());
                }
                Ok(Value::Object(Rc::new(SystemInfo {
                    ty,
                    hash: bsl_vm::creation_number() as i32,
                })))
            })),
        },
    );
}
