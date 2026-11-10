use super::object::read_only;
use super::{MdCtx, fold, member_name, no_field};
use bsl_prim::Str;
use bsl_vm::{
    Arg, CallResult, Execution, HostError, HostObject, MemberId, MemberKind, TypeId, Value,
};
use platform_catalog::{Catalog, TypeIdx, TypeKind, messages::Lang};
use std::any::Any;
use std::rc::Rc;
use std::sync::Arc;

const HOLDER: &str = "ПеречислимыеСвойстваОбъектовМетаданных";

pub(super) const PROPERTY_NAME_RU: &str = "ПеречислимоеСвойствоОбъектовМетаданных";
pub(super) const PROPERTY_NAME_EN: &str = "MetadataObjectEnumeratedProperty";

const ABSENT_ON_SERVER: [&str; 1] = ["ИспользованиеХранилищаДвоичныхДанных"];

const EXTRA: [&str; 1] = ["НаправлениеСообщенияКаналаСервисаИнтеграции"];

const EXTRA_BASE: u32 = 1 << 24;

pub(super) fn root(cx: &Arc<MdCtx>) -> CallResult<Value> {
    let Some(ty) = cx.catalog.find_type(HOLDER) else {
        return Err(
            HostError::not_implemented("ОбъектМетаданныхКонфигурация", "СвойстваОбъектов").into(),
        );
    };
    Ok(Value::Object(Rc::new(Properties { cx: cx.clone(), ty })))
}

fn enumeration(catalog: &Catalog, holder: TypeIdx, ru: &str) -> Option<TypeIdx> {
    let property = catalog
        .type_info(holder)
        .properties
        .iter()
        .find(|p| p.name_ru == ru)?;
    let declared = match property.types.as_slice() {
        [] => catalog.find_type(ru),
        [one] => catalog.find_type(&one.name_ru),
        _ => None,
    };
    declared.filter(|t| catalog.type_info(*t).kind == TypeKind::Enum)
}

fn served(catalog: &Catalog, ty: TypeIdx) -> bool {
    catalog
        .type_info(ty)
        .values
        .iter()
        .any(|v| bsl_stdlib::enums::enum_value(ty, &v.name_ru).is_some())
}

fn value_member(catalog: &Catalog, ty: TypeIdx, name: &str) -> Option<MemberId> {
    let i = catalog.find_enum_value(ty, name)?;
    let value = catalog.type_info(ty).values.get(i)?;
    bsl_stdlib::enums::enum_value(ty, &value.name_ru)?;
    u32::try_from(i).ok().map(MemberId)
}

fn hash_of(ty: TypeIdx) -> i32 {
    0x6f70_0000 | i32::from(ty.0)
}

fn extra_member(catalog: &Catalog, m: MemberId) -> Option<(&'static str, TypeIdx)> {
    let k = usize::try_from(m.0.checked_sub(EXTRA_BASE)?).ok()?;
    let ru = *EXTRA.get(k)?;
    let ty = catalog
        .find_type(ru)
        .filter(|t| catalog.type_info(*t).kind == TypeKind::Enum)?;
    Some((ru, ty))
}

struct Properties {
    cx: Arc<MdCtx>,
    ty: TypeIdx,
}

impl Properties {
    fn property(&self, ru: &str, ty: Option<TypeIdx>) -> CallResult<Value> {
        match ty.filter(|t| served(self.cx.catalog, *t)) {
            Some(ty) => Ok(Value::Object(Rc::new(Property {
                cx: self.cx.clone(),
                ty,
            }))),
            None => Err(HostError::not_implemented(HOLDER, ru).into()),
        }
    }
}

impl HostObject for Properties {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.ty)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        if let Some((ru, ty)) = extra_member(self.cx.catalog, m) {
            return self.property(ru, Some(ty));
        }
        let Some((false, ru, _)) = member_name(self.cx.catalog, self.ty, m) else {
            return Err(no_field(&m.0.to_string()).into());
        };
        if ABSENT_ON_SERVER.contains(&ru) {
            return Err(no_field(ru).into());
        }
        self.property(ru, enumeration(self.cx.catalog, self.ty, ru))
    }
    fn set(&self, _: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        let name = match extra_member(self.cx.catalog, m) {
            Some((ru, _)) => ru,
            None => member_name(self.cx.catalog, self.ty, m).map_or("", |n| n.1),
        };
        Err(read_only(name).into())
    }
    fn call(&self, _: &mut Execution, m: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        Err(no_field(&m.0.to_string()).into())
    }
    fn find_member(&self, name: &str) -> Option<MemberId> {
        let want = fold(name);
        EXTRA.iter().enumerate().find_map(|(k, ru)| {
            let ty = self.cx.catalog.find_type(ru)?;
            let en = fold(&self.cx.catalog.type_info(ty).name_en);
            let id = EXTRA_BASE.checked_add(u32::try_from(k).ok()?)?;
            (fold(ru) == want || en == want).then_some(MemberId(id))
        })
    }
    fn member_kind(&self, m: MemberId) -> Option<MemberKind> {
        extra_member(self.cx.catalog, m).map(|_| MemberKind::Property {
            readable: true,
            writable: false,
        })
    }
    fn presentation(&self, _: &mut Execution) -> CallResult<Option<Str>> {
        let info = self.cx.catalog.type_info(self.ty);
        Ok(Some(Str::from(match self.cx.script_lang() {
            Lang::Ru => info.name_ru.as_str(),
            Lang::En => info.name_en.as_str(),
        })))
    }
    fn boost_hash(&self) -> Option<i32> {
        Some(hash_of(self.ty))
    }
    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        Some(
            other
                .as_any()
                .downcast_ref::<Properties>()
                .is_some_and(|o| Arc::ptr_eq(&o.cx.app, &self.cx.app)),
        )
    }
}

struct Property {
    cx: Arc<MdCtx>,
    ty: TypeIdx,
}

impl Property {
    fn value(&self, m: MemberId) -> CallResult<Value> {
        let name = usize::try_from(m.0)
            .ok()
            .and_then(|i| self.cx.catalog.type_info(self.ty).values.get(i))
            .map(|v| v.name_ru.as_str());
        name.and_then(|n| bsl_stdlib::enums::enum_value(self.ty, n))
            .ok_or_else(|| no_field(&m.0.to_string()).into())
    }
}

impl HostObject for Property {
    fn type_id(&self) -> TypeId {
        self.cx.t_prop
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        self.value(m)
    }
    fn set(&self, _: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        let name = usize::try_from(m.0)
            .ok()
            .and_then(|i| self.cx.catalog.type_info(self.ty).values.get(i))
            .map_or("", |v| v.name_ru.as_str());
        Err(read_only(name).into())
    }
    fn call(&self, _: &mut Execution, m: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        Err(no_field(&m.0.to_string()).into())
    }
    fn find_member(&self, name: &str) -> Option<MemberId> {
        value_member(self.cx.catalog, self.ty, name)
    }
    fn member_kind(&self, _: MemberId) -> Option<MemberKind> {
        Some(MemberKind::Property {
            readable: true,
            writable: false,
        })
    }
    fn index_get(&self, _: &mut Execution, key: &Value) -> CallResult<Value> {
        let Value::String(s) = key else {
            let info = self.cx.catalog.type_info(self.ty);
            return Err(HostError::not_implemented(
                HOLDER,
                &format!("{}[ключ не строка]", info.name_ru),
            )
            .into());
        };
        let name = s.to_string_lossy();
        match value_member(self.cx.catalog, self.ty, &name) {
            Some(m) => self.value(m),
            None => Err(no_field(&name).into()),
        }
    }
    fn presentation(&self, _: &mut Execution) -> CallResult<Option<Str>> {
        Ok(Some(Str::from(match self.cx.script_lang() {
            Lang::Ru => PROPERTY_NAME_RU,
            Lang::En => PROPERTY_NAME_EN,
        })))
    }
    fn boost_hash(&self) -> Option<i32> {
        Some(hash_of(self.ty))
    }
    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        Some(
            other
                .as_any()
                .downcast_ref::<Property>()
                .is_some_and(|o| Arc::ptr_eq(&o.cx.app, &self.cx.app) && o.ty == self.ty),
        )
    }
}
