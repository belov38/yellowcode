use super::picture::library_picture;
use crate::enums::type_by_id;
use bsl_prim::fold_name;
use bsl_vm::{
    Arg, CallResult, Exception, Execution, HostError, HostObject, MemberId, MemberKind,
    RetainedMemory, TypeId, Value,
};
use platform_catalog::{TypeIdx, catalog};
use std::any::Any;
use std::rc::Rc;
use std::sync::Arc;

const PLATFORM: u32 = 1 << 31;

type Entry = (String, [u8; 16]);

pub struct PictureNames(Arc<[Entry]>);

impl PictureNames {
    pub fn new(pictures: impl IntoIterator<Item = (String, [u8; 16])>) -> PictureNames {
        PictureNames(
            pictures
                .into_iter()
                .map(|(name, g)| (fold_name(&name), g))
                .collect(),
        )
    }
}

fn manager_type() -> Option<TypeIdx> {
    type_by_id("PictureLib")
}

pub fn picture_library(names: &PictureNames) -> Option<Value> {
    let t = manager_type()?;
    Some(Value::Object(Rc::new(Library {
        t,
        names: names.0.clone(),
    })))
}

struct Library {
    t: TypeIdx,
    names: Arc<[Entry]>,
}

impl Library {
    fn platform_name(&self, k: u32) -> String {
        catalog()
            .type_info(self.t)
            .values
            .get(k as usize)
            .map_or_else(|| k.to_string(), |v| v.name_ru.clone())
    }
}

impl HostObject for Library {
    fn retained_memory(&self, visit: &mut dyn RetainedMemory) -> Result<bool, Exception> {
        visit.allocation(
            Arc::as_ptr(&self.names).cast::<()>() as usize,
            crate::session::shared_bytes(&*self.names),
        )?;
        Ok(true)
    }

    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.t)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn find_member(&self, name: &str) -> Option<MemberId> {
        if let Some(k) = catalog().find_enum_value(self.t, name) {
            return u32::try_from(k).ok().map(|k| MemberId(PLATFORM | k));
        }
        let folded = fold_name(name);
        let i = self.names.iter().position(|(f, _)| *f == folded)?;
        u32::try_from(i)
            .ok()
            .filter(|i| i & PLATFORM == 0)
            .map(MemberId)
    }

    fn member_kind(&self, _m: MemberId) -> Option<MemberKind> {
        Some(MemberKind::Property {
            readable: true,
            writable: false,
        })
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        if m.0 & PLATFORM != 0 {
            let name = self.platform_name(m.0 & !PLATFORM);
            return Err(
                HostError::not_implemented(&catalog().type_info(self.t).name_ru, &name).into(),
            );
        }
        match self.names.get(m.0 as usize) {
            Some((_, g)) => Ok(library_picture(*g)),
            None => Err(HostError::not_implemented(
                &catalog().type_info(self.t).name_ru,
                &m.0.to_string(),
            )
            .into()),
        }
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        Err(
            HostError::not_implemented(&catalog().type_info(self.t).name_ru, &m.0.to_string())
                .into(),
        )
    }

    fn call(&self, _ex: &mut Execution, m: MemberId, _args: &mut [Arg]) -> CallResult<Value> {
        Err(
            HostError::not_implemented(&catalog().type_info(self.t).name_ru, &m.0.to_string())
                .into(),
        )
    }
}
