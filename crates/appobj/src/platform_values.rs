use crate::meta::{member_name, member_of, no_field};
use bsl_prim::Str;
use bsl_vm::{
    Arg, CallResult, Exception, Execution, HostError, HostObject, MemberId, Registry,
    RetainedMemory, TypeId, TypeIdx, TypeImpl, Value, creation_number,
};
use platform_catalog::catalog;
use std::any::Any;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

const TYPE: &str = "ОписаниеЗащитыОтОпасныхДействий";
const WARNINGS: &str = "ПредупреждатьОбОпасныхДействиях";

struct UnsafeProtection {
    ty: TypeIdx,
    warnings_id: MemberId,
    warnings: Cell<bool>,
    hash: i32,
}

impl UnsafeProtection {
    fn other(&self, m: MemberId) -> HostError {
        match member_name(catalog(), self.ty, m) {
            Some((_, ru, _)) => HostError::not_implemented(TYPE, ru),
            None => no_field(&m.0.to_string()),
        }
    }
}

impl HostObject for UnsafeProtection {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.ty)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        if m == self.warnings_id {
            Ok(Value::Bool(self.warnings.get()))
        } else {
            Err(self.other(m).into())
        }
    }

    fn set(&self, _: &mut Execution, m: MemberId, v: Value) -> CallResult<()> {
        if m != self.warnings_id {
            return Err(self.other(m).into());
        }
        match v {
            Value::Bool(b) => {
                self.warnings.set(b);
                Ok(())
            }
            _ => Err(crate::session::type_mismatch().into()),
        }
    }

    fn call(&self, _: &mut Execution, m: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        Err(self.other(m).into())
    }

    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<Str>> {
        let name = ex
            .runtime()
            .types()
            .name(TypeId::of_catalog(self.ty), ex.lang());
        Ok(Some(Str::from(name)))
    }

    fn boost_hash(&self) -> Option<i32> {
        Some(self.hash)
    }

    fn retained_memory(&self, _: &mut dyn RetainedMemory) -> Result<bool, Exception> {
        Ok(true)
    }
}

pub(crate) fn register(reg: &mut Registry) {
    let catalog = catalog();
    let Some(ty) = catalog.find_type(TYPE) else {
        return;
    };
    let Some((warnings_id, _)) = member_of(catalog, ty, WARNINGS) else {
        return;
    };
    reg.type_impl(
        ty,
        TypeImpl {
            ctor: Some(Arc::new(move |_, _| {
                Ok(Value::Object(Rc::new(UnsafeProtection {
                    ty,
                    warnings_id,
                    warnings: Cell::new(true),
                    hash: creation_number() as i32,
                })))
            })),
        },
    );
}
