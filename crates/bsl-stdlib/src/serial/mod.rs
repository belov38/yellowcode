pub(crate) mod boundary;
pub mod codec;
pub(crate) mod depth;
pub(crate) mod errinfo;
pub mod internal;
pub(crate) mod kind;
mod nodes;
pub(crate) mod oldxml;
pub(crate) mod s4_codecs;
pub mod storage;
pub(crate) mod textdoc;
mod typedesc;
mod valuelist;
pub(crate) mod valuelist_json;
pub(crate) mod valuelist_xml;
mod valuetable;
pub(crate) mod valuetable_xml;
mod valuetree;
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
