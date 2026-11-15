use super::{MdCtx, MdKey, value_of};
use crate::Application;
use crate::session::internal;
use bsl_vm::{Exception, Execution, SessionExport, SessionImports, SessionObject, Value};
use std::sync::{Arc, Weak};

struct Moved {
    app: Weak<Application>,
    key: MdKey,
}

pub(super) fn export(
    cx: &MdCtx,
    key: MdKey,
    out: &mut dyn SessionExport,
) -> Result<Option<Box<dyn SessionObject>>, Exception> {
    out.reserve(size_of::<Moved>() as u64)?;
    Ok(Some(Box::new(Moved {
        app: Arc::downgrade(&cx.app),
        key,
    })))
}

impl SessionObject for Moved {
    fn retained_memory(&self, _: &mut dyn bsl_vm::RetainedMemory) -> Result<(), Exception> {
        Ok(())
    }

    fn create(&self, ex: &mut Execution, _: &mut SessionImports) -> Result<Value, Exception> {
        let Some(cx) = self.app.upgrade().and_then(|app| app.md_ctx()) else {
            return Err(ex.error(internal("the metadata of a cached value is gone")));
        };
        Ok(value_of(&cx, ex, self.key))
    }

    fn fill(&self, ex: &mut Execution, _: &Value, children: Vec<Value>) -> Result<(), Exception> {
        if children.is_empty() {
            Ok(())
        } else {
            Err(ex.out_of_memory())
        }
    }

    fn clear(&self, _: &Value) {}

    fn restore_bytes(&self, _: usize) -> u64 {
        size_of::<super::object::MdObj>()
            .max(size_of::<super::object::MdStd>())
            .max(size_of::<super::object::MdRoot>()) as u64
            + 2 * size_of::<usize>() as u64
    }
}
