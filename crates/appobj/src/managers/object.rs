use super::enums;
use crate::Application;
use crate::meta::{arg_value, member_name, type_mismatch};
use crate::model::{ObjId, ObjKind};
use crate::predefined::{self, AreaKey};
use crate::values::refs::{RefValue, empty_ref, ref_value};
use bsl_syntax::ast::MethodKind;
use bsl_vm::{
    Arg, CallError, CallResult, Execution, HostError, HostObject, MemberId, MemberKind, TypeId,
    Value,
};
use ibschema::values::{RefId, Uuid16};
use platform_catalog::messages::Lang;
use std::any::Any;
use std::sync::Arc;

const VALUE_BASE: u32 = 1 << 20;
const EXPORT_BASE: u32 = 1 << 24;

pub(crate) struct ObjMgr {
    app: Arc<Application>,
    obj: ObjId,
    ty: TypeId,
}

impl ObjMgr {
    pub fn new(app: &Arc<Application>, obj: ObjId) -> Result<ObjMgr, HostError> {
        let ty = app
            .types()
            .and_then(|t| t.type_of(obj, crate::types::Category::Manager))
            .ok_or_else(|| {
                let name = &app.model().obj(obj).name;
                HostError::text(
                    &format!("internal: no manager type for the object {name}"),
                    &format!("internal: no manager type for the object {name}"),
                )
            })?;
        Ok(ObjMgr {
            app: app.clone(),
            obj,
            ty,
        })
    }

    fn kind(&self) -> ObjKind {
        self.app.model().obj(self.obj).kind
    }

    fn param_names(&self, ex: &Execution, m: MemberId) -> Vec<String> {
        let Some(t) = ex
            .runtime()
            .types()
            .app_type(self.ty)
            .and_then(|a| a.template)
        else {
            return Vec::new();
        };
        let info = platform_catalog::catalog().type_info(t);
        (m.0 as usize)
            .checked_sub(info.properties.len())
            .and_then(|i| info.methods.get(i))
            .and_then(|me| me.variants.first())
            .map(|v| v.params.iter().map(|p| p.name_ru.clone()).collect())
            .unwrap_or_default()
    }

    fn has_predefined(&self) -> bool {
        self.app.model().obj(self.obj).behavior.predefined_count > 0
    }

    fn is_enum(&self) -> bool {
        self.app.model().obj(self.obj).kind == ObjKind::Enum
    }

    fn member(&self, ex: &Execution, m: MemberId) -> Option<(bool, &'static str)> {
        let t = ex.runtime().types().app_type(self.ty)?.template?;
        member_name(platform_catalog::catalog(), t, m).map(|(method, ru, _)| (method, ru))
    }

    fn type_name(&self) -> String {
        format!(
            "{}Менеджер",
            self.app.model().obj(self.obj).kind.name(Lang::Ru)
        )
    }

    fn export(&self, name: &str) -> Option<(bsl_vm::ModuleRef, u32)> {
        let mods = self.app.modules();
        let m = mods.of_object(self.obj, md_model::ModuleKind::ManagerModule)?;
        let c = mods.compiled(m).ok()?;
        c.method(name)
            .filter(|(_, code)| code.export)
            .map(|(i, _)| (m, i))
    }

    fn get_ref(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let v = match args.first() {
            None | Some(Arg::Omitted) => Value::Undefined,
            Some(_) => arg_value(ex, args, 0)?,
        };
        let id = if matches!(v, Value::Undefined) {
            self.app.uuid1().next_ref()
        } else {
            let b = bsl_stdlib::values::uuid_bytes(&v).ok_or_else(|| type_mismatch(1))?;
            RefId::from_uuid(Uuid16::from_bytes(b))
        };
        Ok(ref_value(&self.app, RefValue { obj: self.obj, id })?)
    }

    fn off_bound() -> HostError {
        HostError::key(bsl_stdlib::bind::msg::RANGE)
    }
}

impl HostObject for ObjMgr {
    fn type_id(&self) -> TypeId {
        self.ty
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let i = m.0.wrapping_sub(VALUE_BASE) as usize;
        if self.is_enum()
            && (VALUE_BASE..EXPORT_BASE).contains(&m.0)
            && let Some(v) = enums::value_at(&self.app, self.obj, i)?
        {
            return Ok(v);
        }
        if !self.is_enum()
            && (VALUE_BASE..EXPORT_BASE).contains(&m.0)
            && let Some(name) = predefined::item_name(&self.app, self.obj, i)
        {
            crate::read::admit_data_read(ex)?;
            let r =
                self.app
                    .predefined()
                    .by_name(&self.app, self.obj, &AreaKey::default(), &name)?;
            return Ok(ref_value(&self.app, r)?);
        }
        let name = self
            .member(ex, m)
            .map_or_else(|| m.0.to_string(), |n| n.1.to_string());
        if name == "ТочкиМаршрута" && self.kind() == ObjKind::BusinessProcess {
            return super::points::collection(&self.app, self.obj);
        }
        Err(HostError::not_implemented(&self.type_name(), &name).into())
    }

    fn set(&self, ex: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        let name = if (VALUE_BASE..EXPORT_BASE).contains(&m.0) {
            let i = (m.0 - VALUE_BASE) as usize;
            if self.is_enum() {
                self.app
                    .model()
                    .obj(self.obj)
                    .enum_values
                    .get(i)
                    .map_or_else(String::new, |v| v.name.clone())
            } else {
                predefined::item_name(&self.app, self.obj, i).unwrap_or_default()
            }
        } else {
            self.member(ex, m)
                .map_or_else(String::new, |n| n.1.to_string())
        };
        Err(HostError::key(bsl_stdlib::bind::msg::PROP_NOT_WRITABLE)
            .with_arg(name)
            .raw()
            .into())
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        if m.0 >= EXPORT_BASE {
            let idx = (m.0 - EXPORT_BASE) as usize;
            let mref = self
                .app
                .modules()
                .of_object(self.obj, md_model::ModuleKind::ManagerModule)
                .ok_or_else(|| {
                    HostError::text("internal: no manager module", "internal: no manager module")
                })?;
            let name = self
                .app
                .modules()
                .compiled(mref)
                .ok()
                .and_then(|c| c.methods.get(idx).map(|code| code.name.name.to_string()))
                .ok_or_else(|| {
                    HostError::text(
                        "internal: no exported method",
                        "internal: no exported method",
                    )
                })?;
            let inst = self
                .app
                .instance_in_call(ex, mref)
                .map_err(CallError::Exc)?;
            return ex.call(&inst, &name, args).map_err(CallError::Exc);
        }
        let ru = self.member(ex, m).map_or("", |n| n.1);
        match ru {
            "СоздатьЭлемент" if self.kind() == ObjKind::Catalog => {
                crate::objects::create_item(ex, &self.app, self.obj)
            }
            "ПустаяСсылка" => Ok(empty_ref(&self.app, self.obj)?),
            "ПолучитьСсылку" => self.get_ref(ex, args),
            "Количество" if self.is_enum() => Ok(Value::from(
                i64::try_from(enums::count(&self.app, self.obj)).unwrap_or(i64::MAX),
            )),
            "Получить" if self.is_enum() => {
                let v = arg_value(ex, args, 0)?;
                let n = match &v {
                    Value::Number(n) => n.clone(),
                    Value::String(t) => bsl_prim::Number::parse(t.to_string_lossy().trim())
                        .ok_or_else(|| type_mismatch(1))?,
                    _ => return Err(type_mismatch(1).into()),
                };
                let i =
                    usize::try_from(n.trunc_to_i64_saturating()).map_err(|_| Self::off_bound())?;
                enums::value_at(&self.app, self.obj, i)?.ok_or_else(|| Self::off_bound().into())
            }
            "ПолучитьОбновлениеПредопределенныхДанных" if self.has_predefined() =>
            {
                let mode = self.app.predefined().update_mode(&self.app, self.obj)?;
                predefined::update_mode_value(mode)
                    .ok_or_else(|| HostError::not_implemented(&self.type_name(), ru).into())
            }
            "УстановитьОбновлениеПредопределенныхДанных" if self.has_predefined() =>
            {
                let v = arg_value(ex, args, 0)?;
                let mode = predefined::update_mode_of(&v).ok_or_else(|| type_mismatch(1))?;
                self.app
                    .predefined()
                    .set_update_mode(&self.app, self.obj, mode)?;
                Ok(Value::Undefined)
            }
            "Индекс" if self.is_enum() => {
                let v = arg_value(ex, args, 0)?;
                Ok(Value::from(enums::index_of(&self.app, self.obj, &v)?))
            }
            n if crate::select::serves(n) && crate::select::selectable(self.kind()) => {
                let names = self.param_names(ex, m);
                let p = crate::select::Params::collect(ex, args, &names)?;
                crate::select::manager_call(ex, &self.app, self.obj, n, &p)
            }
            _ => Err(HostError::not_implemented(&self.type_name(), ru).into()),
        }
    }

    fn find_member(&self, name: &str) -> Option<MemberId> {
        if self.is_enum()
            && let Some(i) = enums::find(&self.app, self.obj, name)
        {
            return u32::try_from(i).ok().map(|i| MemberId(VALUE_BASE + i));
        }
        if !self.is_enum()
            && let Some(i) = predefined::item_position(&self.app, self.obj, name)
        {
            return u32::try_from(i).ok().map(|i| MemberId(VALUE_BASE + i));
        }
        self.export(name).map(|(_, i)| MemberId(EXPORT_BASE + i))
    }

    fn member_kind(&self, m: MemberId) -> Option<MemberKind> {
        if (VALUE_BASE..EXPORT_BASE).contains(&m.0) {
            return Some(MemberKind::Property {
                readable: true,
                writable: false,
            });
        }
        if m.0 < EXPORT_BASE {
            return None;
        }
        let mref = self
            .app
            .modules()
            .of_object(self.obj, md_model::ModuleKind::ManagerModule)?;
        let c = self.app.modules().compiled(mref).ok()?;
        let code = c.methods.get((m.0 - EXPORT_BASE) as usize)?;
        let clamp = |n: usize| u16::try_from(n).unwrap_or(u16::MAX);
        let required = code
            .params
            .iter()
            .rposition(|p| p.default.is_none())
            .map_or(0, |i| i + 1);
        Some(MemberKind::Method {
            is_function: code.kind == MethodKind::Function,
            min_args: clamp(required),
            max_args: Some(clamp(code.params.len())),
        })
    }

    fn iter_len(&self, _: &mut Execution) -> Option<usize> {
        self.is_enum().then(|| enums::count(&self.app, self.obj))
    }

    fn iter_get(&self, _: &mut Execution, i: usize) -> CallResult<Value> {
        enums::value_at(&self.app, self.obj, i)?.ok_or_else(|| Self::off_bound().into())
    }

    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        Some(
            other
                .as_any()
                .downcast_ref::<ObjMgr>()
                .is_some_and(|o| o.obj == self.obj),
        )
    }
}
