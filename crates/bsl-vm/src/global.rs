use crate::error::info::{self, CallKind};
use crate::error::{Exception, HostError};
use crate::object::CallError;
use crate::place::Arg;
use crate::registry::{GlobalProp, Registry};
use crate::runtime::Execution;
use crate::value::Value;
use platform_catalog::{TypeIdx, messages::Lang};

const GLOBAL_CONTEXT: &str = "Глобальный контекст";
const METHOD_NOT_FOUND: &str = "bsl:IDS_RTE_VALMETHOD_NOTFOUND";
const PROP_NOT_FOUND: &str = "bsl:IDS_RTE_VALPROP_NOTFOUND";

impl Registry {
    pub fn system_enum(&mut self, t: TypeIdx, manager: GlobalProp) {
        debug_assert!(
            usize::from(t.0) < self.catalog.types().len(),
            "system enumeration {} is not a catalog type",
            t.0
        );
        let old = self.system_enums.insert(t, manager);
        debug_assert!(
            old.is_none(),
            "system enumeration {} is registered twice",
            t.0
        );
    }
}

impl Execution {
    pub fn call_global(&mut self, name: &str, args: &mut [Arg]) -> Result<Value, Exception> {
        let catalog = self.runtime().registry().catalog;
        let Some(m) = catalog.find_global_method(name) else {
            return Err(self.error(HostError::key(METHOD_NOT_FOUND).with_arg(name)));
        };
        let kind = CallKind::Global { builtin: m.builtin };
        let r = match self.runtime().registry().find_global_fn(name) {
            Some(f) => f(self, args),
            None => Err(HostError::not_implemented(GLOBAL_CONTEXT, &m.info.name_ru).into()),
        };
        let shown = if self.script_variant() == Lang::Ru {
            m.info.name_ru.clone()
        } else {
            m.info.name_en.clone()
        };
        self.finish(r, kind, &shown)
    }

    pub fn global_property(&mut self, name: &str) -> Result<Value, Exception> {
        let catalog = self.runtime().registry().catalog;
        let Some(p) = catalog.find_global_property(name) else {
            return Err(self.error(HostError::key(PROP_NOT_FOUND).with_arg(name)));
        };
        let r = match self.runtime().registry().find_global_prop(name) {
            Some(f) => f(self),
            None => Err(HostError::not_implemented(GLOBAL_CONTEXT, &p.name_ru).into()),
        };
        let shown = if self.script_variant() == Lang::Ru {
            p.name_ru.clone()
        } else {
            p.name_en.clone()
        };
        self.finish(r, CallKind::GetAttr, &shown)
    }

    pub fn system_enum(&mut self, t: TypeIdx) -> Result<Value, Exception> {
        if let Some(v) = self.system_enums.get(&t) {
            return Ok(v.clone());
        }
        let Some(f) = self.runtime().registry().system_enums.get(&t).cloned() else {
            return Ok(Value::Undefined);
        };
        let catalog = self.runtime().registry().catalog;
        let name = catalog.types().get(usize::from(t.0)).map_or("", |info| {
            if self.script_variant() == Lang::Ru {
                info.name_ru.as_str()
            } else {
                info.name_en.as_str()
            }
        });
        let r = f(self);
        let v = self.finish(r, CallKind::GetAttr, name)?;
        self.system_enums.insert(t, v.clone());
        Ok(v)
    }

    pub(crate) fn finish(
        &mut self,
        r: Result<Value, CallError>,
        kind: CallKind,
        name: &str,
    ) -> Result<Value, Exception> {
        match r {
            Ok(v) => Ok(v),
            Err(CallError::Exc(e)) => Err(e),
            Err(CallError::Host(e)) => {
                Err(Exception::Catchable(info::from_host(self, e, kind, name)))
            }
        }
    }
}
