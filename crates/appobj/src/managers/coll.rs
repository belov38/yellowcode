use super::{of_object, same_name};
use crate::Application;
use crate::meta::{member_name, no_field};
use crate::model::{ObjId, ObjKind};
use bsl_vm::{
    Arg, CallError, CallResult, Execution, HostError, HostObject, MemberId, MemberKind, TypeId,
    Value,
};
use platform_catalog::{TypeIdx, messages::Lang};
use std::any::Any;
use std::sync::Arc;

pub(crate) struct CollMgr {
    app: Arc<Application>,
    kind: ObjKind,
    ty: TypeId,
    tidx: TypeIdx,
}

impl CollMgr {
    pub fn new(app: &Arc<Application>, kind: ObjKind) -> Result<CollMgr, HostError> {
        let name = format!("{}Менеджер", kind.collection(Lang::Ru));
        let tidx = platform_catalog::catalog().find_type(&name);
        let ty = app.types().and_then(|t| t.collection_manager(kind));
        match (tidx, ty) {
            (Some(tidx), Some(ty)) => Ok(CollMgr {
                app: app.clone(),
                kind,
                ty,
                tidx,
            }),
            _ => Err(HostError::text(
                &format!("internal: no type {name}"),
                &format!("internal: no type {name}"),
            )),
        }
    }

    fn ids(&self) -> impl Iterator<Item = ObjId> + '_ {
        self.app.model().of_kind(self.kind).map(|o| o.id)
    }

    fn at(&self, i: usize) -> Option<ObjId> {
        self.ids().nth(i)
    }

    fn position(&self, name: &str) -> Option<usize> {
        self.app
            .model()
            .of_kind(self.kind)
            .position(|o| same_name(&o.name, name))
    }

    fn object(&self, ex: &mut Execution, i: usize) -> CallResult<Value> {
        let id = self.at(i).ok_or_else(|| no_field(&i.to_string()))?;
        of_object(&self.app, ex, id).map_err(CallError::Exc)
    }
}

impl HostObject for CollMgr {
    fn type_id(&self) -> TypeId {
        self.ty
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        self.object(ex, m.0 as usize)
    }

    fn set(&self, _: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        let name = self
            .at(m.0 as usize)
            .map_or_else(String::new, |o| self.app.model().obj(o).name.clone());
        Err(HostError::key(bsl_stdlib::bind::msg::PROP_NOT_WRITABLE)
            .with_arg(name)
            .raw()
            .into())
    }

    fn call(&self, _: &mut Execution, m: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        let platform = platform_catalog::catalog();
        let ru = member_name(platform, self.tidx, m).map_or("", |n| n.1);
        let type_name = platform.type_info(self.tidx).name_ru.as_str();
        Err(HostError::not_implemented(type_name, ru).into())
    }

    fn find_member(&self, name: &str) -> Option<MemberId> {
        self.position(name)
            .and_then(|i| u32::try_from(i).ok())
            .map(MemberId)
    }

    fn member_kind(&self, m: MemberId) -> Option<MemberKind> {
        self.at(m.0 as usize)
            .is_some()
            .then_some(MemberKind::Property {
                readable: true,
                writable: false,
            })
    }

    fn index_get(&self, ex: &mut Execution, key: &Value) -> CallResult<Value> {
        let Value::String(s) = key else {
            return Err(HostError::key(bsl_stdlib::bind::msg::INDEX_OFFBOUND)
                .raw()
                .into());
        };
        let name = s.to_string_lossy();
        match self.position(&name) {
            Some(i) => self.object(ex, i),
            None => Err(no_field(&name).into()),
        }
    }

    fn iter_len(&self, _: &mut Execution) -> Option<usize> {
        Some(self.ids().count())
    }

    fn iter_get(&self, ex: &mut Execution, i: usize) -> CallResult<Value> {
        self.object(ex, i)
    }

    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        Some(
            other
                .as_any()
                .downcast_ref::<CollMgr>()
                .is_some_and(|o| o.kind == self.kind),
        )
    }
}
