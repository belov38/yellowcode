use super::{
    MdCtx, MdKey, MdPath, arg_value, fold, key_with_app, member_name, no_field, not_implemented,
    value_of,
};
use crate::model::ObjId;
use bsl_vm::{
    Arg, CallResult, Exception, Execution, HostError, HostObject, MemberId, RetainedMemory, TypeId,
    Value,
};
use platform_catalog::{TypeIdx, catalog};
use std::any::Any;
use std::rc::Rc;
use std::sync::Arc;

type Item = (MdPath, bool);

fn items(cx: &MdCtx, plan: ObjId) -> CallResult<Vec<Item>> {
    let refuse = || not_implemented("Состав");
    let md = cx.model().obj(plan).md;
    let object = cx
        .cfg_index
        .get(&md)
        .and_then(|i| cx.model().config.objects.get(*i))
        .ok_or_else(refuse)?;
    let list = object.exchange_content().ok_or_else(refuse)?;
    let mut out = Vec::with_capacity(list.len());
    for item in list {
        let path = super::props::resolve_ref(cx, &item.object).ok_or_else(refuse)?;
        out.push((path, item.auto_record));
    }
    Ok(out)
}

pub(super) fn content(cx: &Arc<MdCtx>, plan: ObjId) -> CallResult<Value> {
    let (Some(ty), Some(item_ty)) = (
        catalog().find_type("СоставПланаОбмена"),
        catalog().find_type("ЭлементСоставаПланаОбмена"),
    ) else {
        return Err(not_implemented("Состав").into());
    };
    Ok(Value::Object(Rc::new(PlanContent {
        cx: cx.clone(),
        items: items(cx, plan)?,
        ty,
        item_ty,
    })))
}

struct PlanContent {
    cx: Arc<MdCtx>,
    items: Vec<Item>,
    ty: TypeIdx,
    item_ty: TypeIdx,
}

impl PlanContent {
    fn item(&self, i: usize) -> Option<Value> {
        let &(path, auto) = self.items.get(i)?;
        Some(Value::Object(Rc::new(PlanItem {
            cx: self.cx.clone(),
            path,
            auto,
            ty: self.item_ty,
        })))
    }

    fn position(&self, v: &Value) -> CallResult<Option<usize>> {
        let (key, app) = key_with_app(v)
            .filter(|(key, _)| *key != MdKey::Path(MdPath::Configuration))
            .ok_or_else(|| super::props::type_mismatch(1))?;
        Ok(if Arc::ptr_eq(app, &self.cx.app) {
            self.items.iter().position(|(p, _)| MdKey::Path(*p) == key)
        } else {
            None
        })
    }

    fn call_method(&self, ex: &mut Execution, ru: &str, args: &[Arg]) -> CallResult<Value> {
        match fold(ru).as_str() {
            "количество" => Ok(Value::from(
                i64::try_from(self.items.len()).unwrap_or(i64::MAX),
            )),
            "найти" => {
                let v = arg_value(ex, args, 0)?;
                Ok(self
                    .position(&v)?
                    .and_then(|i| self.item(i))
                    .unwrap_or(Value::Undefined))
            }
            "содержит" => {
                let v = arg_value(ex, args, 0)?;
                Ok(Value::Bool(self.position(&v)?.is_some()))
            }
            _ => Err(not_implemented(&format!("Состав.{ru}")).into()),
        }
    }
}

impl HostObject for PlanContent {
    fn retained_memory(&self, visit: &mut dyn RetainedMemory) -> Result<bool, Exception> {
        let bytes = self.items.len() as u64 * std::mem::size_of::<Item>() as u64;
        visit.bytes(bytes)?;
        Ok(true)
    }
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.ty)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        Err(no_field(&m.0.to_string()).into())
    }
    fn set(&self, _: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        Err(no_field(&m.0.to_string()).into())
    }
    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        let Some((true, ru, en)) = member_name(self.cx.catalog, self.ty, m) else {
            return Err(no_field(&m.0.to_string()).into());
        };
        let wrapped = match self.cx.script_lang() {
            platform_catalog::messages::Lang::Ru => ru,
            platform_catalog::messages::Lang::En => en,
        };
        self.call_method(ex, ru, args).map_err(|e| match e {
            bsl_vm::CallError::Host(h) => crate::session::wrap_method(wrapped, h).into(),
            e => e,
        })
    }
    fn iter_len(&self, _: &mut Execution) -> Option<usize> {
        Some(self.items.len())
    }
    fn iter_get(&self, _: &mut Execution, _: usize) -> CallResult<Value> {
        Err(not_implemented("Состав: обход").into())
    }
}

struct PlanItem {
    cx: Arc<MdCtx>,
    path: MdPath,
    auto: bool,
    ty: TypeIdx,
}

impl HostObject for PlanItem {
    fn retained_memory(&self, _: &mut dyn RetainedMemory) -> Result<bool, Exception> {
        Ok(true)
    }
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.ty)
    }
    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<bsl_prim::Str>> {
        let info = self.cx.catalog.type_info(self.ty);
        Ok(Some(bsl_prim::Str::from(match ex.lang() {
            platform_catalog::messages::Lang::Ru => info.name_ru.as_str(),
            platform_catalog::messages::Lang::En => info.name_en.as_str(),
        })))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let Some((false, ru, _)) = member_name(self.cx.catalog, self.ty, m) else {
            return Err(no_field(&m.0.to_string()).into());
        };
        match fold(ru).as_str() {
            "метаданные" => Ok(value_of(&self.cx, ex, MdKey::Path(self.path))),
            "авторегистрация" => catalog()
                .find_type("АвтоРегистрацияИзменений")
                .and_then(|t| {
                    bsl_stdlib::enums::enum_value(t, if self.auto { "Allow" } else { "Deny" })
                })
                .ok_or_else(|| not_implemented("Состав.АвтоРегистрация").into()),
            _ => Err(no_field(ru).into()),
        }
    }
    fn set(&self, _: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        let name = member_name(self.cx.catalog, self.ty, m)
            .map_or_else(String::new, |(_, ru, _)| ru.to_string());
        Err(HostError::key(bsl_stdlib::bind::msg::PROP_NOT_WRITABLE)
            .with_arg(name)
            .raw()
            .into())
    }
    fn call(&self, _: &mut Execution, m: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        Err(no_field(&m.0.to_string()).into())
    }
}
