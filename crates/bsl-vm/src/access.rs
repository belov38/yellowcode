use crate::error::info::CallKind;
use crate::error::{Exception, HostError};
use crate::place::Arg;
use crate::runtime::Execution;
use crate::types::{MemberId, MemberKind, TypeId};
use crate::value::{ObjRef, Value};
use platform_catalog::TypeIdx;

const NOT_OBJECT: &str = "bsl:IDS_RTE_NOTOBJECT";
const METHOD_NOT_FOUND: &str = "bsl:IDS_RTE_VALMETHOD_NOTFOUND";
const PROP_NOT_FOUND: &str = "bsl:IDS_RTE_VALPROP_NOTFOUND";
const PROC_AS_FUNC: &str = "bsl:IDS_RTE_PROP_PROCASFUNC";
const TOO_MANY: &str = "bsl:IDS_RTE_PARAMS_TOOMANY";
const NOT_ENOUGH: &str = "bsl:IDS_RTE_PARAMS_NOTENOUGH";
const NOT_READABLE: &str = "bsl:IDS_RTE_PROP_NONREADABLE";
const NOT_WRITABLE: &str = "bsl:IDS_RTE_PROP_NONWRITABLE";
const NOT_INDEXED: &str = "bsl:IDS_RTE_NOTINDEXED";
const TYPE_NOT_DEFINED: &str = "bsl:IDS_RTE_TYPENOTDEFINED";
const CTOR_NOT_FOUND: &str = "bsl:IDS_RTE_CTORNOTFOUND";

const CTOR_MEMBER: &str = "Новый";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Access {
    Get,
    Set,
}

impl Execution {
    pub fn call_method(
        &mut self,
        obj: &Value,
        name: &str,
        args: &mut [Arg],
        want: bool,
    ) -> Result<Value, Exception> {
        let o = self.receiver(obj, name)?;
        let types = self.runtime().types();
        let found = match types.method(obj.type_id(), name) {
            Some((m, kind)) => Some((m, Some(kind))),
            None if types.member(obj.type_id(), name).is_some() => None,
            None => o
                .find_member_guarded(self, name)?
                .map(|m| (m, o.member_kind(m))),
        };
        let (m, kind) = match found {
            Some((m, kind)) if !matches!(kind, Some(MemberKind::Property { .. })) => (m, kind),
            _ => return Err(self.vm_error(METHOD_NOT_FOUND, Some(name))),
        };
        if let Some(MemberKind::Method {
            is_function,
            min_args,
            max_args,
        }) = kind
        {
            if want && !is_function {
                return Err(self.vm_error(PROC_AS_FUNC, Some(name)));
            }
            if max_args.is_some_and(|max| args.len() > usize::from(max)) {
                return Err(self.vm_error(TOO_MANY, None));
            }
            if args.len() < usize::from(min_args) {
                return Err(self.vm_error(NOT_ENOUGH, None));
            }
        }
        let r = o.call(self, m, args);
        let shown = self.member_in_variant(obj.type_id(), name, true);
        self.finish(r, CallKind::Method, &shown)
    }

    pub fn get_member(&mut self, obj: &Value, name: &str) -> Result<Value, Exception> {
        let o = self.receiver(obj, name)?;
        let m = self.field(o, obj.type_id(), name, Access::Get)?;
        let r = o.get(self, m);
        let shown = self.member_in_variant(obj.type_id(), name, false);
        self.finish(r, CallKind::GetAttr, &shown)
    }

    pub fn set_member(&mut self, obj: &Value, name: &str, v: Value) -> Result<(), Exception> {
        let o = self.receiver(obj, name)?;
        let m = self.field(o, obj.type_id(), name, Access::Set)?;
        let r = o.set(self, m, v).map(|()| Value::Undefined);
        let shown = self.member_in_variant(obj.type_id(), name, false);
        self.finish(r, CallKind::SetAttr, &shown).map(|_| ())
    }

    pub fn index_get(&mut self, obj: &Value, key: &Value) -> Result<Value, Exception> {
        let Value::Object(o) = obj else {
            return Err(self.vm_error(NOT_INDEXED, None));
        };
        let r = o.index_get(self, key);
        self.finish(r, CallKind::Vm, "")
    }

    pub fn index_set(&mut self, obj: &Value, key: &Value, v: Value) -> Result<(), Exception> {
        let Value::Object(o) = obj else {
            return Err(self.vm_error(NOT_INDEXED, None));
        };
        let r = o.index_set(self, key, v).map(|()| Value::Undefined);
        self.finish(r, CallKind::Vm, "").map(|_| ())
    }

    pub fn construct(&mut self, t: TypeIdx, args: &mut [Arg]) -> Result<Value, Exception> {
        let catalog = self.runtime().registry().catalog;
        let type_name = catalog
            .types()
            .get(usize::from(t.0))
            .map_or("", |info| info.name_ru.as_str());
        let r = match self.runtime().registry().find_ctor(t) {
            Some(f) => f(self, args),
            None => Err(HostError::not_implemented(type_name, CTOR_MEMBER).into()),
        };
        let kind = CallKind::Ctor {
            type_name: type_name.to_owned(),
        };
        self.finish(r, kind, type_name)
    }

    pub fn construct_by_name(&mut self, name: &str, args: &mut [Arg]) -> Result<Value, Exception> {
        let types = self.runtime().types();
        let Some(t) = types.by_name(name) else {
            return Err(self.vm_error(TYPE_NOT_DEFINED, Some(name)));
        };
        let platform = match types.app_type(t) {
            Some(info) => {
                let default = info.default_value.clone().filter(|_| args.is_empty());
                if let Some(v) = default {
                    return v.to_value(self);
                }
                None
            }
            None => types.catalog_type(t),
        };
        let catalog = self.runtime().registry().catalog;
        if let Some(idx) = platform {
            let info = catalog.type_info(idx);
            if info.accepts_type_name(name) == Some(false) {
                return Err(self.vm_error(TYPE_NOT_DEFINED, Some(name)));
            }
            match info.new_error.filter(|_| args.is_empty()) {
                Some(platform_catalog::NewError::CtorNotFound) => {
                    return Err(self.vm_error(CTOR_NOT_FOUND, Some(name)));
                }
                Some(platform_catalog::NewError::NotObject) => {
                    return Err(self.vm_error(NOT_OBJECT, Some(name)));
                }
                Some(platform_catalog::NewError::CtorFails) => return self.construct(idx, args),
                None => {}
            }
        }
        let ctor = platform.filter(|&idx| {
            self.runtime().registry().find_ctor(idx).is_some()
                || catalog
                    .types()
                    .get(usize::from(idx.0))
                    .is_some_and(|info| !info.constructors.is_empty())
        });
        match ctor {
            Some(idx) => self.construct(idx, args),
            None => Err(self.vm_error(CTOR_NOT_FOUND, Some(name))),
        }
    }

    fn receiver<'v>(&mut self, obj: &'v Value, name: &str) -> Result<&'v ObjRef, Exception> {
        match obj {
            Value::Object(o) => Ok(o),
            _ => Err(self.vm_error(NOT_OBJECT, Some(name))),
        }
    }

    fn member_in_variant(&self, t: TypeId, name: &str, method: bool) -> String {
        let lang = self.script_variant();
        self.runtime()
            .types()
            .member_name(t, name, method, lang)
            .unwrap_or(name)
            .to_owned()
    }

    fn field(
        &mut self,
        o: &ObjRef,
        t: TypeId,
        name: &str,
        access: Access,
    ) -> Result<MemberId, Exception> {
        let found = match self.runtime().types().member(t, name) {
            Some((m, kind @ MemberKind::Property { .. })) => Some((m, Some(kind))),
            _ => o
                .find_member_guarded(self, name)?
                .map(|m| (m, o.member_kind(m))),
        };
        match found {
            None | Some((_, Some(MemberKind::Method { .. }))) => {
                Err(self.vm_error(PROP_NOT_FOUND, Some(name)))
            }
            Some((
                _,
                Some(MemberKind::Property {
                    readable: false, ..
                }),
            )) if access == Access::Get => Err(self.vm_error(NOT_READABLE, Some(name))),
            Some((
                _,
                Some(MemberKind::Property {
                    writable: false, ..
                }),
            )) if access == Access::Set => Err(self.vm_error(NOT_WRITABLE, Some(name))),
            Some((m, _)) => Ok(m),
        }
    }

    pub(crate) fn vm_error(&mut self, key: &'static str, arg: Option<&str>) -> Exception {
        let e = HostError::key(key);
        self.error(match arg {
            Some(a) => e.with_arg(a),
            None => e,
        })
    }
}
