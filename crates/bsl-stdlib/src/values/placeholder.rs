use crate::collections::not_implemented;
use bsl_prim::Str;
use bsl_vm::{
    Arg, CallResult, Execution, HostObject, MemberId, Registry, TypeId, TypeIdx, TypeImpl, Value,
};
use platform_catalog::catalog;
use std::{any::Any, rc::Rc, sync::Arc};

pub struct Placeholder {
    t: TypeIdx,
    hash: i32,
}
impl HostObject for Placeholder {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.t)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        Err(not_implemented(self.t, m).raw().into())
    }
    fn set(&self, _: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        Err(not_implemented(self.t, m).raw().into())
    }
    fn call(&self, _: &mut Execution, m: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        Err(not_implemented(self.t, m).raw().into())
    }
    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<Str>> {
        Ok(Some(Str::from(
            ex.runtime()
                .types()
                .presentation(TypeId::of_catalog(self.t), ex.lang()),
        )))
    }
    fn boost_hash(&self) -> Option<i32> {
        Some(self.hash)
    }
}

pub(crate) fn register(reg: &mut Registry) {
    for (i, info) in catalog().types().iter().enumerate() {
        if info.new_no_args != Some(true) {
            continue;
        }
        let Ok(i) = u16::try_from(i) else { break };
        let t = TypeIdx(i);
        reg.fallback_type_impl(
            t,
            TypeImpl {
                ctor: Some(Arc::new(move |_, _| {
                    Ok(Value::Object(Rc::new(Placeholder {
                        t,
                        hash: bsl_vm::creation_number() as i32,
                    })))
                })),
            },
        );
    }
}
