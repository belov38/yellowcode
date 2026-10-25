pub mod codec;
pub(crate) mod depth;
pub(crate) mod errinfo;
pub mod internal;
pub(crate) mod kind;
pub(crate) mod oldxml;
pub mod storage;
pub(crate) mod xmlvalue;

use bsl_vm::{Exception, Execution, FatalError, FatalKind};
use platform_catalog::messages::text_key;
use std::rc::Rc;

pub(crate) fn cycle_fatal(ex: &Execution) -> Exception {
    let text = text_key("backend:IDS_ERR_STACKOVERFLOW", ex.lang()).unwrap_or("stack overflow");
    Exception::Fatal(Rc::new(FatalError {
        kind: FatalKind::StackOverflow,
        text: text.to_string(),
        cause: None,
    }))
}

pub(crate) fn register(reg: &mut bsl_vm::Registry) {
    internal::register(reg);
    storage::register(reg);
    oldxml::register(reg);
}
