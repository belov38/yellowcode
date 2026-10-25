use super::object::{info, number};
use super::{
    MdCtx, MdKey, MdPath, Ty, arg_value, fold, is_model_kind, key_of, member_name, member_of,
    no_field, value_of,
};
use crate::model::{FieldRole, ObjId};
use bsl_vm::{
    Arg, CallResult, Execution, HostError, HostObject, MemberId, MemberKind, ObjRef, TypeId, Value,
};
use md_model::MdKind;
use std::any::Any;
use std::rc::Rc;
use std::sync::Arc;

const ITEM_BASE: u32 = 1 << 20;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CollTy {
    Objects,
    Values,
    Std,
}

pub(crate) struct MdColl {
    pub cx: Arc<MdCtx>,
    pub ty: CollTy,
    pub items: Vec<MdKey>,
}

pub(crate) fn new_coll(cx: &Arc<MdCtx>, ty: CollTy, items: Vec<MdKey>) -> Value {
    let o: ObjRef = Rc::new(MdColl {
        cx: cx.clone(),
        ty,
        items,
    });
    Value::Object(o)
}

pub(crate) fn root_coll(cx: &Arc<MdCtx>, kind: MdKind) -> Value {
    let model = cx.model();
    let items: Vec<MdKey> = if let Some(k) = is_model_kind(kind) {
        model
            .of_kind(k)
            .map(|d| MdKey::Path(MdPath::Object(d.id)))
            .collect()
    } else {
        match kind {
            MdKind::CommonModule => (0..model.common_modules.len())
                .map(|i| MdKey::Path(MdPath::CommonModule(i)))
                .collect(),
            MdKind::SessionParameter => (0..model.session_params.len())
                .map(|i| MdKey::Path(MdPath::SessionParam(i)))
                .collect(),
            MdKind::EventSubscription => (0..model.subscriptions.len())
                .map(|i| MdKey::Path(MdPath::Subscription(i)))
                .collect(),
            _ => model
                .config
                .objects
                .iter()
                .enumerate()
                .filter(|(_, o)| o.kind == kind)
                .map(|(i, _)| MdKey::Path(MdPath::Other(i)))
                .collect(),
        }
    };
    new_coll(cx, CollTy::Objects, items)
}

pub(crate) fn fields_coll(
    cx: &Arc<MdCtx>,
    obj: ObjId,
    tab: Option<usize>,
    role: FieldRole,
) -> Value {
    let def = cx.model().obj(obj);
    let fields = match tab {
        None => Some(&def.fields),
        Some(t) => def.tabular.get(t).map(|t| &t.fields),
    };
    let items = fields
        .into_iter()
        .flatten()
        .enumerate()
        .filter(|(_, f)| f.std.is_none() && f.role == role)
        .map(|(i, _)| MdKey::Path(MdPath::Field(obj, tab, i)))
        .collect();
    new_coll(cx, CollTy::Objects, items)
}

pub(crate) fn std_coll(cx: &Arc<MdCtx>, obj: ObjId, tab: Option<usize>) -> Value {
    let def = cx.model().obj(obj);
    let fields = match tab {
        None => Some(&def.fields),
        Some(t) => def.tabular.get(t).map(|t| &t.fields),
    };
    let items = fields
        .into_iter()
        .flatten()
        .enumerate()
        .filter(|(_, f)| f.std.is_some())
        .map(|(i, _)| MdKey::Std(obj, tab, i))
        .collect();
    new_coll(cx, CollTy::Std, items)
}

pub(crate) fn tabular_coll(cx: &Arc<MdCtx>, obj: ObjId) -> Value {
    let n = cx.model().obj(obj).tabular.len();
    new_coll(
        cx,
        CollTy::Objects,
        (0..n)
            .map(|t| MdKey::Path(MdPath::Tabular(obj, t)))
            .collect(),
    )
}

pub(crate) fn forms_coll(cx: &Arc<MdCtx>, obj: ObjId) -> Value {
    let n = cx
        .cfg_index
        .get(&cx.model().obj(obj).md)
        .and_then(|i| cx.model().config.objects.get(*i))
        .map_or(0, |m| m.forms.len());
    new_coll(
        cx,
        CollTy::Objects,
        (0..n).map(|i| MdKey::Path(MdPath::Form(obj, i))).collect(),
    )
}

pub(crate) fn enum_values_coll(cx: &Arc<MdCtx>, obj: ObjId) -> Value {
    let n = cx.model().obj(obj).enum_values.len();
    new_coll(
        cx,
        CollTy::Objects,
        (0..n)
            .map(|i| MdKey::Path(MdPath::EnumValue(obj, i)))
            .collect(),
    )
}

impl MdColl {
    fn ty_info(&self) -> Ty {
        match self.ty {
            CollTy::Objects => self.cx.coll,
            CollTy::Values => self.cx.vals,
            CollTy::Std => self.cx.stds,
        }
    }

    fn name_of(&self, k: MdKey) -> String {
        match k {
            MdKey::Path(p) => info(&self.cx, p).map_or_else(String::new, |i| i.name.to_string()),
            MdKey::Std(o, t, i) => {
                super::object::field(&self.cx, o, t, i).map_or_else(String::new, |f| f.name.clone())
            }
        }
    }

    fn position_of_name(&self, name: &str) -> Option<usize> {
        let f = fold(name);
        self.items.iter().position(|k| {
            fold(&self.name_of(*k)) == f
                || match k {
                    MdKey::Std(o, t, i) => super::object::field(&self.cx, *o, *t, *i)
                        .and_then(|fd| fd.std)
                        .is_some_and(|a| {
                            let (ru, en) = a.names();
                            fold(ru) == f || fold(en) == f
                        }),
                    MdKey::Path(_) => false,
                }
        })
    }

    fn item(&self, ex: &mut Execution, i: usize) -> Option<Value> {
        self.items.get(i).map(|k| value_of(&self.cx, ex, *k))
    }

    fn call_method(&self, ex: &mut Execution, ru: &str, args: &mut [Arg]) -> CallResult<Value> {
        match fold(ru).as_str() {
            "количество" => Ok(number(self.items.len())),
            "получить" => {
                let v = arg_value(ex, args, 0)?;
                let Value::Number(n) = &v else {
                    return Err(super::props::type_mismatch(1).into());
                };
                let range = || HostError::key(bsl_stdlib::bind::msg::RANGE);
                let i = usize::try_from(n.trunc_to_i64_saturating()).map_err(|_| range())?;
                self.item(ex, i).ok_or_else(|| range().into())
            }
            "найти" => {
                let v = arg_value(ex, args, 0)?;
                let Value::String(s) = &v else {
                    return Ok(Value::Undefined);
                };
                Ok(match self.position_of_name(&s.to_string_lossy()) {
                    Some(i) => self.item(ex, i).unwrap_or(Value::Undefined),
                    None => Value::Undefined,
                })
            }
            "индекс" => {
                let v = arg_value(ex, args, 0)?;
                let k = key_of(&v).ok_or_else(|| super::props::type_mismatch(1))?;
                let pos = self.items.iter().position(|x| *x == k);
                Ok(match pos {
                    Some(i) => number(i),
                    None => Value::from(-1_i64),
                })
            }
            "содержит" => {
                let v = arg_value(ex, args, 0)?;
                let k = key_of(&v).ok_or_else(|| super::props::type_mismatch(1))?;
                Ok(Value::Bool(self.items.contains(&k)))
            }
            _ => Err(no_field(ru).into()),
        }
    }

    fn off_bound() -> HostError {
        HostError::key(bsl_stdlib::bind::msg::INDEX_OFFBOUND)
    }
}

impl HostObject for MdColl {
    fn type_id(&self) -> TypeId {
        self.ty_info().id
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        self.item(ex, (m.0.wrapping_sub(ITEM_BASE)) as usize)
            .ok_or_else(|| no_field(&m.0.to_string()).into())
    }
    fn set(&self, _: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        Err(HostError::key(bsl_stdlib::bind::msg::PROP_NOT_WRITABLE)
            .with_arg(
                self.items
                    .get(m.0.wrapping_sub(ITEM_BASE) as usize)
                    .map(|k| self.name_of(*k))
                    .unwrap_or_default(),
            )
            .raw()
            .into())
    }
    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        let t = self.ty_info().idx;
        let Some((true, ru, en)) = member_name(self.cx.catalog, t, m) else {
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
    fn find_member(&self, name: &str) -> Option<MemberId> {
        self.position_of_name(name)
            .and_then(|i| u32::try_from(i).ok())
            .map(|i| MemberId(ITEM_BASE + i))
    }
    fn member_kind(&self, m: MemberId) -> Option<MemberKind> {
        if m.0 >= ITEM_BASE {
            return Some(MemberKind::Property {
                readable: true,
                writable: false,
            });
        }
        let t = self.ty_info().idx;
        let (_, ru, _) = member_name(self.cx.catalog, t, m)?;
        member_of(self.cx.catalog, t, ru).map(|k| k.1)
    }
    fn presentation(&self, _: &mut Execution) -> CallResult<Option<bsl_prim::Str>> {
        let (ru, en) = match self.ty {
            CollTy::Objects => ("КоллекцияОбъектовМетаданных", "MetadataObjectCollection"),
            CollTy::Values => (
                "КоллекцияЗначенийСвойстваОбъектаМетаданных",
                "MetadataObjectPropertyValueCollection",
            ),
            CollTy::Std => (
                "ОписанияСтандартныхРеквизитов",
                "StandardAttributeDescriptions",
            ),
        };
        Ok(Some(bsl_prim::Str::from(match self.cx.script_lang() {
            platform_catalog::messages::Lang::Ru => ru,
            platform_catalog::messages::Lang::En => en,
        })))
    }
    fn index_get(&self, ex: &mut Execution, key: &Value) -> CallResult<Value> {
        if self.ty == CollTy::Values {
            return Err(HostError::key("bsl:IDS_RTE_NOTINDEXED").into());
        }
        match key {
            Value::Number(n) => {
                let i =
                    usize::try_from(n.trunc_to_i64_saturating()).map_err(|_| Self::off_bound())?;
                self.item(ex, i).ok_or_else(|| Self::off_bound().into())
            }
            Value::String(s) => {
                let name = s.to_string_lossy();
                match self.position_of_name(&name) {
                    Some(i) => Ok(self.item(ex, i).unwrap_or(Value::Undefined)),
                    None => Err(no_field(&name).into()),
                }
            }
            _ => Err(Self::off_bound().into()),
        }
    }
    fn iter_len(&self, _: &mut Execution) -> Option<usize> {
        Some(self.items.len())
    }
    fn iter_get(&self, ex: &mut Execution, i: usize) -> CallResult<Value> {
        self.item(ex, i).ok_or_else(|| Self::off_bound().into())
    }
}
