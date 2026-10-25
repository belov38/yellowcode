mod coll;
mod enums;
mod object;
mod points;

use crate::Application;
use crate::model::{KINDS, ObjId, ObjKind};
use crate::session::call_ctx;
use bsl_prim::fold_name;
use bsl_vm::{CallError, Exception, Execution, ObjRef, Registry, Value};
use platform_catalog::messages::Lang;
use std::rc::Rc;
use std::sync::Arc;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum MgrKey {
    Collection(ObjKind),
    Object(ObjId),
}

pub(crate) fn register(app: &Arc<Application>, reg: &mut Registry) {
    let catalog = platform_catalog::catalog();
    for kind in KINDS {
        let (ru, en) = (kind.collection(Lang::Ru), kind.collection(Lang::En));
        if catalog.find_global_property(ru).is_none() {
            continue;
        }
        let app = app.clone();
        reg.global_prop(
            ru,
            en,
            Arc::new(move |ex| collection(&app, ex, kind).map_err(CallError::Exc)),
        );
    }
}

pub fn collection(
    app: &Arc<Application>,
    ex: &mut Execution,
    kind: ObjKind,
) -> Result<Value, Exception> {
    app.modules()
        .compile_managers(app.model(), kind, ex.lang())?;
    cached(ex, MgrKey::Collection(kind), || {
        coll::CollMgr::new(app, kind).map(|c| Rc::new(c) as ObjRef)
    })
}

pub fn of_object(
    app: &Arc<Application>,
    ex: &mut Execution,
    obj: ObjId,
) -> Result<Value, Exception> {
    app.modules()
        .compile_managers(app.model(), app.model().obj(obj).kind, ex.lang())?;
    cached(ex, MgrKey::Object(obj), || {
        object::ObjMgr::new(app, obj).map(|o| Rc::new(o) as ObjRef)
    })
}

fn cached(
    ex: &mut Execution,
    key: MgrKey,
    make: impl FnOnce() -> Result<ObjRef, bsl_vm::HostError>,
) -> Result<Value, Exception> {
    if let Some(o) = call_ctx(ex).and_then(|c| c.mgr_cache.get(&key).cloned()) {
        return Ok(Value::Object(o));
    }
    let o = make().map_err(|e| ex.error(e))?;
    if let Some(c) = call_ctx(ex) {
        c.mgr_cache.insert(key, o.clone());
    }
    Ok(Value::Object(o))
}

pub(crate) fn enum_value(
    app: &Application,
    obj: ObjId,
    name: &str,
) -> Result<Option<Value>, bsl_vm::HostError> {
    match enums::find(app, obj, name) {
        Some(i) => enums::value_at(app, obj, i),
        None => Ok(None),
    }
}

fn same_name(a: &str, b: &str) -> bool {
    fold_name(a) == fold_name(b)
}
