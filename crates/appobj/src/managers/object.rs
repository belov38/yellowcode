use super::enums;
use crate::Application;
use crate::meta::{arg_value, member_name, type_mismatch};
use crate::model::{ObjId, ObjKind};
use crate::predefined::{self, AreaKey};
use crate::txn::CallTxn;
use crate::values::refs::{RefValue, empty_ref, ref_value};
use bsl_prim::fold_name;
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
const EXT_BASE: u32 = 1 << 28;
const EXT_SLOTS: usize = 256;
const EXT_METHODS: u32 = 1 << 16;

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

    fn value_member(&self, name: &str) -> Option<MemberId> {
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
        None
    }

    fn intercept_module(&self) -> String {
        let def = self.app.model().obj(self.obj);
        format!("{}.{}.ManagerModule", def.kind.md_kind().as_str(), def.name)
    }

    fn intercepted(&self, folded: &str) -> Option<String> {
        let module = fold_name(&self.intercept_module());
        self.app.options().extensions.iter().find_map(|e| {
            e.intercepts
                .iter()
                .find(|x| fold_name(&x.module) == module && fold_name(&x.target) == folded)
                .map(|x| {
                    format!(
                        "расширение «{}» перехватывает метод ({})",
                        e.name, x.annotation
                    )
                })
        })
    }

    fn resolve(&self, name: &str, lang: Lang) -> Result<Option<MemberId>, String> {
        let folded = fold_name(name);
        let mods = self.app.modules();
        let ext = mods.extension_managers(self.obj);
        if let Some((_, i)) = self.export(name) {
            if let Some(why) = self.intercepted(&folded) {
                return Err(why);
            }
            for &m in ext {
                let methods = mods.extension_methods(m)?;
                if methods.iter().any(|x| x.export && x.folded == folded) {
                    let extension = mods.extension_of(m).map_or("", |(_, n)| n);
                    return Err(format!(
                        "метод есть и в модуле менеджера конфигурации, и в модуле менеджера расширения «{extension}»"
                    ));
                }
            }
            return Ok(Some(MemberId(EXPORT_BASE + i)));
        }
        if !crate::extensions::manager_gate(&self.app)? {
            return Ok(None);
        }
        let mut found = Vec::new();
        for (slot, &m) in ext.iter().enumerate() {
            let methods = mods.extension_methods(m)?;
            if let Some(x) = methods.iter().find(|x| x.export && x.folded == folded) {
                found.push((slot, m, x.intercept));
            }
        }
        let (slot, m, annotated) = match found.as_slice() {
            [] => return Ok(None),
            [one] => *one,
            [..] => {
                return Err("метод есть в модулях менеджера нескольких расширений".to_string());
            }
        };
        let (facts, extension) = mods
            .extension_of(m)
            .ok_or("internal: not a module of an extension")?;
        if let Some(why) = self.intercepted(&folded) {
            return Err(why);
        }
        if annotated {
            return Err(format!(
                "метод расширения «{extension}» с аннотацией перехвата"
            ));
        }
        let e = self
            .app
            .options()
            .extensions
            .get(facts)
            .ok_or("internal: no facts of the extension")?;
        match &e.info {
            Ok(info) if info.safe_mode || !info.security_profile.is_empty() => {
                return Err(format!(
                    "метод расширения «{extension}» в безопасном режиме"
                ));
            }
            Ok(_) => {}
            Err(why) => return Err(format!("расширение «{extension}»: {why}")),
        }
        let compiled = match mods.compiled(m) {
            Ok(c) => c,
            Err(crate::modules::ModuleError::Compile(err)) => {
                let module = mods.name_of(m).unwrap_or("");
                return Err(format!(
                    "модуль менеджера расширения «{extension}» не компилируется: {}",
                    err.cause_text(module, lang)
                ));
            }
            Err(crate::modules::ModuleError::InvalidRef(r)) => {
                return Err(format!("internal: invalid module reference {}", r.0));
            }
        };
        let Some((i, _)) = compiled.method(name).filter(|(_, code)| code.export) else {
            return Ok(None);
        };
        if slot >= EXT_SLOTS || i >= EXT_METHODS {
            return Err("слишком много модулей или методов расширений".to_string());
        }
        Ok(Some(MemberId(EXT_BASE | ((slot as u32) << 16) | i)))
    }

    fn ext_method(
        &self,
        m: MemberId,
    ) -> Option<(bsl_vm::ModuleRef, Arc<bsl_compiler::CompiledModule>, usize)> {
        let rest = m.0.checked_sub(EXT_BASE)?;
        let slot = (rest >> 16) as usize;
        let idx = (rest & (EXT_METHODS - 1)) as usize;
        let mods = self.app.modules();
        let mref = *mods.extension_managers(self.obj).get(slot)?;
        let compiled = mods.compiled(mref).ok()?;
        compiled.methods.get(idx)?;
        Some((mref, compiled, idx))
    }
}

fn method_kind(code: &bsl_compiler::MethodCode) -> MemberKind {
    let clamp = |n: usize| u16::try_from(n).unwrap_or(u16::MAX);
    let required = code
        .params
        .iter()
        .rposition(|p| p.default.is_none())
        .map_or(0, |i| i + 1);
    MemberKind::Method {
        is_function: code.kind == MethodKind::Function,
        min_args: clamp(required),
        max_args: Some(clamp(code.params.len())),
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
            let own = CallTxn::of_call(ex);
            let r = self.app.predefined().by_name(
                &self.app,
                self.obj,
                &AreaKey::default(),
                &name,
                own.as_deref(),
            )?;
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
        if m.0 >= EXT_BASE {
            let (mref, compiled, idx) = self.ext_method(m).ok_or_else(|| {
                HostError::text(
                    "internal: no method of an extension",
                    "internal: no method of an extension",
                )
            })?;
            let name = compiled
                .methods
                .get(idx)
                .map(|code| code.name.name.to_string())
                .unwrap_or_default();
            let inst = self
                .app
                .instance_in_call(ex, mref)
                .map_err(CallError::Exc)?;
            return ex.call(&inst, &name, args).map_err(CallError::Exc);
        }
        if m.0 >= EXPORT_BASE {
            let idx = (m.0 - EXPORT_BASE) as usize;
            if let Some(name) = self
                .app
                .modules()
                .of_object(self.obj, md_model::ModuleKind::ManagerModule)
                .and_then(|r| self.app.modules().compiled(r).ok())
                .and_then(|c| c.methods.get(idx).map(|code| code.name.name.to_string()))
                && let Err(why) = self.resolve(&name, ex.lang())
            {
                return Err(HostError::not_implemented(
                    &self.type_name(),
                    &format!("{name}: {why}"),
                )
                .into());
            }
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
                crate::write::admit_insert(ex, &self.app, self.obj, "СоздатьЭлемент")?;
                crate::objects::create_item(ex, &self.app, self.obj)
            }
            "СоздатьДокумент" if self.kind() == ObjKind::Document => {
                crate::write::admit_insert(ex, &self.app, self.obj, "СоздатьДокумент")?;
                crate::objects::create_document(ex, &self.app, self.obj)
            }
            "СоздатьГруппу" if self.kind() == ObjKind::Catalog => {
                crate::write::admit_insert(ex, &self.app, self.obj, "СоздатьГруппу")?;
                crate::objects::create_group(ex, &self.app, self.obj)
            }
            "СоздатьУзел" if self.kind() == ObjKind::ExchangePlan => {
                crate::write::admit_insert(ex, &self.app, self.obj, "СоздатьУзел")?;
                crate::objects::create_node(ex, &self.app, self.obj)
            }
            "СоздатьНаборЗаписей" if self.kind() == ObjKind::InformationRegister => {
                crate::recordset::create_set(ex, &self.app, self.obj)
            }
            "СоздатьКлючЗаписи" if self.kind() == ObjKind::InformationRegister => {
                crate::recordset::create_key(ex, &self.app, self.obj, args)
            }
            "ПустаяСсылка" => Ok(empty_ref(&self.app, self.obj)?),
            "ЭтотУзел" => crate::select::this_node(ex, &self.app, self.obj),
            "Получить" if self.kind() == ObjKind::InformationRegister => {
                crate::recordset::manager_get(ex, &self.app, self.obj, args)
            }
            "ПолучитьСсылку" => self.get_ref(ex, args),
            "Получить" if self.kind() == ObjKind::Constant => {
                super::constants::get(ex, &self.app, self.obj)
            }
            "Установить" if self.kind() == ObjKind::Constant => {
                super::constants::set(ex, &self.app, self.obj, args)
            }
            "СоздатьМенеджерЗначения" if self.kind() == ObjKind::Constant => {
                super::constants::ValueManager::create(&self.app, self.obj)
            }
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
                if CallTxn::of_call(ex).is_some_and(|t| t.depth() > 0) {
                    return Err(HostError::not_implemented(
                        "Предопределенные",
                        "УстановитьОбновлениеПредопределенныхДанных в транзакции",
                    )
                    .into());
                }
                self.app
                    .predefined()
                    .set_update_mode(&self.app, self.obj, mode)?;
                Ok(Value::Undefined)
            }
            "Индекс" if self.is_enum() => {
                let v = arg_value(ex, args, 0)?;
                Ok(Value::from(enums::index_of(&self.app, self.obj, &v)?))
            }
            n if crate::regread::serves(self.kind(), n) => {
                crate::regread::manager_call(ex, &self.app, self.obj, n, args)
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
        self.value_member(name)
            .or_else(|| self.export(name).map(|(_, i)| MemberId(EXPORT_BASE + i)))
    }

    fn find_member_guarded(
        &self,
        ex: &mut Execution,
        name: &str,
    ) -> Result<Option<MemberId>, bsl_vm::Exception> {
        if let Some(m) = self.value_member(name) {
            return Ok(Some(m));
        }
        self.resolve(name, ex.lang()).map_err(|why| {
            ex.error(HostError::not_implemented(
                &self.type_name(),
                &format!("{name}: {why}"),
            ))
        })
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
        if m.0 >= EXT_BASE {
            let (_, compiled, idx) = self.ext_method(m)?;
            return compiled.methods.get(idx).map(method_kind);
        }
        let mref = self
            .app
            .modules()
            .of_object(self.obj, md_model::ModuleKind::ManagerModule)?;
        let c = self.app.modules().compiled(mref).ok()?;
        let code = c.methods.get((m.0 - EXPORT_BASE) as usize)?;
        Some(method_kind(code))
    }

    fn index_get(&self, _: &mut Execution, key: &Value) -> CallResult<Value> {
        if !self.is_enum() {
            return Err(super::not_indexed().into());
        }
        let i = match key {
            Value::String(name) => {
                let name = name.to_string_lossy();
                enums::find(&self.app, self.obj, &name)
                    .ok_or_else(|| crate::meta::no_field(&name))?
            }
            Value::Number(n) => usize::try_from(n.trunc_to_i64_saturating())
                .map_err(|_| super::index_off_bound())?,
            _ => return Err(super::not_indexed().into()),
        };
        enums::value_at(&self.app, self.obj, i)?.ok_or_else(|| super::index_off_bound().into())
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
