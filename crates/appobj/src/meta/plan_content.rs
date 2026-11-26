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
use std::cell::OnceCell;
use std::rc::Rc;
use std::sync::Arc;

type Item = (MdPath, bool);

const WALK_MAX: usize = 512;
const BUCKETS: usize = 8;

fn container_walk(keys: &[u32]) -> Vec<usize> {
    struct Container {
        buckets: Vec<Vec<usize>>,
        order: Vec<usize>,
    }
    impl Container {
        fn new(buckets: usize) -> Container {
            Container {
                buckets: vec![Vec::new(); buckets],
                order: Vec::new(),
            }
        }
        fn insert(&mut self, key: u32, at: usize) {
            let b = key as usize & (self.buckets.len() - 1);
            if let Some(chain) = self.buckets.get_mut(b) {
                if chain.is_empty() {
                    self.order.push(b);
                }
                chain.insert(0, at);
            }
        }
        fn walk(&self) -> Vec<usize> {
            self.order
                .iter()
                .filter_map(|b| self.buckets.get(*b))
                .flatten()
                .copied()
                .collect()
        }
    }
    let mut c = Container::new(BUCKETS);
    for (at, &key) in keys.iter().enumerate() {
        if at >= c.buckets.len() {
            let walk = c.walk();
            c = Container::new(c.buckets.len() * BUCKETS);
            for again in walk {
                if let Some(&k) = keys.get(again) {
                    c.insert(k, again);
                }
            }
        }
        c.insert(key, at);
    }
    c.walk()
}

fn items(cx: &MdCtx, plan: ObjId) -> CallResult<Vec<Item>> {
    let refuse = || not_implemented("Состав");
    let object = plan_object(cx, plan).ok_or_else(refuse)?;
    let list = object.exchange_content().ok_or_else(refuse)?;
    let mut out = Vec::with_capacity(list.len());
    for item in list {
        let path = super::props::resolve_ref(cx, &item.object).ok_or_else(refuse)?;
        out.push((path, item.auto_record));
    }
    Ok(out)
}

fn plan_object(cx: &MdCtx, plan: ObjId) -> Option<&md_model::MdObject> {
    let md = cx.model().obj(plan).md;
    cx.cfg_index
        .get(&md)
        .and_then(|i| cx.model().config.objects.get(*i))
}

fn walk_order(cx: &MdCtx, plan: ObjId) -> Result<Vec<usize>, String> {
    let object = plan_object(cx, plan).ok_or("нет плана в модели")?;
    let (Some(list), Some(stream)) = (
        object.exchange_content(),
        object.exchange_content_stream_order(),
    ) else {
        return Err("нет порядка потока состава".into());
    };
    if list.len() > WALK_MAX {
        return Err(format!("более {WALK_MAX} элементов"));
    }
    let mut keys = Vec::with_capacity(stream.len());
    for &at in stream {
        let id = usize::try_from(at)
            .ok()
            .and_then(|at| list.get(at))
            .and_then(|item| item.object.id)
            .ok_or("нет порядка потока состава")?;
        let b = id.as_bytes();
        keys.push(u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
    }
    let mut sorted = keys.clone();
    sorted.sort_unstable();
    if sorted.windows(2).any(|w| w.first() == w.last()) {
        return Err("совпадение Data1 у элементов".into());
    }
    match crate::extensions::changed_object(&cx.app, object) {
        Ok(None | Some(false)) => {}
        Ok(Some(true)) => return Err("план изменён расширением".into()),
        Err(why) => return Err(format!("расширения: {why}")),
    }
    Ok(container_walk(&keys)
        .into_iter()
        .filter_map(|j| stream.get(j))
        .filter_map(|&at| usize::try_from(at).ok())
        .collect())
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
        plan,
        walk: OnceCell::new(),
        ty,
        item_ty,
    })))
}

struct PlanContent {
    cx: Arc<MdCtx>,
    items: Vec<Item>,
    plan: ObjId,
    walk: OnceCell<Result<Vec<usize>, String>>,
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
        let walk = self
            .walk
            .get()
            .and_then(|w| w.as_ref().ok())
            .map_or(0, Vec::len);
        let bytes = self.items.len() as u64 * std::mem::size_of::<Item>() as u64
            + walk as u64 * std::mem::size_of::<usize>() as u64;
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
    fn iter_get(&self, _: &mut Execution, i: usize) -> CallResult<Value> {
        let order = self
            .walk
            .get_or_init(|| walk_order(&self.cx, self.plan))
            .as_ref()
            .map_err(|why| not_implemented(&format!("Состав: обход, {why}")))?;
        order
            .get(i)
            .and_then(|&at| self.item(at))
            .ok_or_else(|| not_implemented("Состав: обход").into())
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
