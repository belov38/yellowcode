use crate::error::{Category, ErrorInfo, Exception, HostError, MsgRef};
use crate::object::{CallResult, HostObject};
use crate::ops;
use crate::place::Arg;
use crate::registry::Registry;
use crate::runtime::Execution;
use crate::shared::{SharedObject, SharedValue};
use crate::types::{MemberId, MemberKind, TypeId};
use crate::value::Value;
use bsl_prim::{Number, Str};
use platform_catalog::messages::Lang;
use platform_catalog::{Context, TypeIdx, catalog};
use std::any::Any;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

const TYPE_MISMATCH: &str = "core85:IDS_PARAMETER_N_TYPE_MISMATCH";
const TYPE_NOT_DEFINED: &str = "bsl:IDS_RTE_TYPENOTDEFINED";
const PROP_NOT_FOUND: &str = "bsl:IDS_RTE_VALPROP_NOTFOUND";
const PROP_NOT_WRITABLE: &str = "bsl:IDS_RTE_PROP_NONWRITABLE";
const METHOD_NOT_FOUND: &str = "bsl:IDS_RTE_VALMETHOD_NOTFOUND";

pub(crate) fn register_language(reg: &mut Registry) {
    reg.global_fn("ТипЗнч", "TypeOf", Arc::new(|ex, args| type_of(ex, args)));
    reg.global_fn("Тип", "Type", Arc::new(|ex, args| type_by_name(ex, args)));
    reg.global_fn(
        "ИнформацияОбОшибке",
        "ErrorInfo",
        Arc::new(|ex, _| Ok(wrap(ex.current_error().unwrap_or_else(empty_info)))),
    );
    reg.global_fn(
        "ОписаниеОшибки",
        "ErrorDescription",
        Arc::new(|ex, _| {
            let lang = ex.lang();
            let text = ex.current_error().map(|e| e.description_text(lang));
            Ok(Value::String(Str::from(text.unwrap_or_default())))
        }),
    );
    reg.global_fn(
        "КраткоеПредставлениеОшибки",
        "BriefErrorDescription",
        Arc::new(|ex, args| {
            let info = info_arg(ex, args)?;
            Ok(Value::String(Str::from(info.brief())))
        }),
    );
    reg.global_fn(
        "ПодробноеПредставлениеОшибки",
        "DetailErrorDescription",
        Arc::new(|ex, args| {
            let info = info_arg(ex, args)?;
            let text = info.detailed(ex.env().compat(), ex.lang());
            Ok(Value::String(Str::from(text)))
        }),
    );
    if let Some(t) = category_type() {
        reg.system_enum(t, Arc::new(|_| Ok(Value::Object(Rc::new(CategoryManager)))));
    }
}

pub fn error_info_of(v: &Value) -> Option<Rc<ErrorInfo>> {
    let o = v.as_object()?.as_any().downcast_ref::<ErrorInfoObject>()?;
    Some(o.0.clone())
}

pub fn error_info_value(info: Rc<ErrorInfo>) -> Value {
    wrap(info)
}

pub(crate) fn category_of(v: &Value) -> Option<Category> {
    let o = v.as_object()?.as_any().downcast_ref::<CategoryValue>()?;
    Some(o.0)
}

pub(crate) fn arg_value(ex: &mut Execution, a: &Arg) -> Result<Value, Exception> {
    match a {
        Arg::Val(v) => Ok(v.clone()),
        Arg::Omitted => Ok(Value::Undefined),
        Arg::Place(p) => p.get(ex),
    }
}

fn first_arg(ex: &mut Execution, args: &[Arg]) -> Result<Value, Exception> {
    match args.first() {
        Some(a) => arg_value(ex, a),
        None => Ok(Value::Undefined),
    }
}

fn mismatch(n: u32) -> HostError {
    let mut e = HostError::key(TYPE_MISMATCH);
    if let MsgRef::Key(k) = &mut e.msg {
        k.args.push(n.to_string());
    }
    e
}

fn type_of(ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
    let v = first_arg(ex, args)?;
    Ok(Value::Type(v.type_id()))
}

fn type_by_name(ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
    let Value::String(s) = first_arg(ex, args)? else {
        return Err(mismatch(1).into());
    };
    let name = s.to_string_lossy();
    let types = ex.runtime().types();
    let known = types.by_name(&name).filter(|t| {
        types.app_type(*t).is_some()
            || types.catalog_type(*t).is_some_and(|i| {
                catalog()
                    .type_info(i)
                    .accepts_type_name(&name)
                    .unwrap_or_else(|| catalog().type_info(i).avail.has(Context::Server))
            })
    });
    match known {
        Some(t) => Ok(Value::Type(t)),
        None => Err(HostError::key(TYPE_NOT_DEFINED).with_arg(name).raw().into()),
    }
}

fn info_arg(ex: &mut Execution, args: &[Arg]) -> CallResult<Rc<ErrorInfo>> {
    let v = first_arg(ex, args)?;
    error_info_of(&v).ok_or_else(|| mismatch(1).into())
}

fn wrap(info: Rc<ErrorInfo>) -> Value {
    Value::Object(Rc::new(ErrorInfoObject(info)))
}

fn empty_info() -> Rc<ErrorInfo> {
    Rc::new(ErrorInfo {
        description: Str::new(),
        module_name: Str::new(),
        line: 0,
        source_line: Str::new(),
        cause: None,
        code: Str::new(),
        additional: Value::Undefined,
        categories: crate::error::CategorySet::default(),
        stack: Vec::new(),
        key: None,
        wire: None,
    })
}

fn type_idx(name: &str) -> Option<TypeIdx> {
    catalog().find_type(name)
}

fn category_type() -> Option<TypeIdx> {
    static T: OnceLock<Option<TypeIdx>> = OnceLock::new();
    *T.get_or_init(|| type_idx("КатегорияОшибки"))
}

fn error_info_idx() -> Option<TypeIdx> {
    static T: OnceLock<Option<TypeIdx>> = OnceLock::new();
    *T.get_or_init(|| type_idx("ИнформацияОбОшибке"))
}

fn error_info_type() -> TypeId {
    error_info_idx().map_or(TypeId::UNDEFINED, TypeId::of_catalog)
}

struct ErrorInfoObject(Rc<ErrorInfo>);

impl ErrorInfoObject {
    fn member_name(&self, m: MemberId) -> Option<&'static str> {
        let t = error_info_idx()?;
        let props = &catalog().type_info(t).properties;
        props.get(m.0 as usize).map(|p| p.name_ru.as_str())
    }
}

impl HostObject for ErrorInfoObject {
    fn type_id(&self) -> TypeId {
        error_info_type()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<Str>> {
        if ex.script_variant() != Lang::Ru {
            return Ok(None);
        }
        Ok(error_info_idx().map(|t| Str::from(catalog().type_info(t).name_ru.as_str())))
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let i = &*self.0;
        Ok(match self.member_name(m) {
            Some("Описание") => Value::String(i.description.clone()),
            Some("ИмяМодуля") => Value::String(i.module_name.clone()),
            Some("НомерСтроки") => Value::Number(Number::from_i64(i64::from(i.line))),
            Some("ИсходнаяСтрока") => Value::String(i.source_line.clone()),
            Some("Причина") => i.cause.clone().map_or(Value::Undefined, wrap),
            Some("Код") => Value::String(i.code.clone()),
            Some("ДополнительнаяИнформация") => i.additional.clone(),
            _ => return Err(HostError::key(PROP_NOT_FOUND).raw().into()),
        })
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        let name = self.member_name(m).unwrap_or_default();
        Err(HostError::key(PROP_NOT_WRITABLE)
            .with_arg(name)
            .raw()
            .into())
    }

    fn call(&self, ex: &mut Execution, _m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        let first = first_arg(ex, args)?;
        let Some(category) = category_of(&first) else {
            return Err(mismatch(1).into());
        };
        let with_causes = match args.get(1) {
            None | Some(Arg::Omitted) => true,
            Some(a) => {
                let v = arg_value(ex, a)?;
                ops::to_bool(ex, &v)?
            }
        };
        Ok(Value::Bool(self.0.is_category(category, with_causes)))
    }
}

struct CategoryValue(Category);

impl HostObject for CategoryValue {
    fn type_id(&self) -> TypeId {
        category_type().map_or(TypeId::UNDEFINED, TypeId::of_catalog)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _ex: &mut Execution, _m: MemberId) -> CallResult<Value> {
        Err(HostError::key(PROP_NOT_FOUND).raw().into())
    }

    fn set(&self, _ex: &mut Execution, _m: MemberId, _v: Value) -> CallResult<()> {
        Err(HostError::key(PROP_NOT_FOUND).raw().into())
    }

    fn call(&self, _ex: &mut Execution, _m: MemberId, _a: &mut [Arg]) -> CallResult<Value> {
        Err(HostError::key(METHOD_NOT_FOUND).raw().into())
    }

    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<Str>> {
        if self.0 == Category::ConfigurationError {
            return Ok(Some(Str::from(match ex.lang() {
                Lang::Ru => "Ошибка конфигурации",
                Lang::En => "Configuration error",
            })));
        }
        Ok(Some(Str::from(match ex.script_variant() {
            Lang::Ru => self.0.name_ru(),
            Lang::En => self.0.name_en(),
        })))
    }

    fn boost_hash(&self) -> Option<i32> {
        Some(bsl_prim::boost_hash::ordinal(self.0 as u32))
    }

    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        let o = other.as_any().downcast_ref::<CategoryValue>()?;
        Some(o.0 == self.0)
    }

    fn to_shared(&self) -> Option<SharedValue> {
        Some(SharedValue::Object(Arc::new(SharedCategory(self.0))))
    }
}

struct SharedCategory(Category);

impl SharedObject for SharedCategory {
    fn to_value(&self, _ex: &mut Execution) -> Result<Value, Exception> {
        Ok(Value::Object(Rc::new(CategoryValue(self.0))))
    }
}

struct CategoryManager;

impl CategoryManager {
    fn value(m: MemberId) -> CallResult<Value> {
        let c = Category::all()
            .get(m.0 as usize)
            .ok_or_else(|| HostError::key(PROP_NOT_FOUND).raw())?;
        Ok(Value::Object(Rc::new(CategoryValue(*c))))
    }
}

impl HostObject for CategoryManager {
    fn type_id(&self) -> TypeId {
        category_type().map_or(TypeId::UNDEFINED, TypeId::of_catalog)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        CategoryManager::value(m)
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        let name = Category::all()
            .get(m.0 as usize)
            .map_or("", |c| c.name_ru());
        Err(HostError::key(PROP_NOT_WRITABLE)
            .with_arg(name)
            .raw()
            .into())
    }

    fn call(&self, _ex: &mut Execution, _m: MemberId, _a: &mut [Arg]) -> CallResult<Value> {
        Err(HostError::key(METHOD_NOT_FOUND).raw().into())
    }

    fn find_member(&self, name: &str) -> Option<MemberId> {
        Category::from_name(name).map(|c| MemberId(c as u32))
    }

    fn member_kind(&self, _m: MemberId) -> Option<MemberKind> {
        Some(MemberKind::Property {
            readable: true,
            writable: false,
        })
    }

    fn index_get(&self, ex: &mut Execution, key: &Value) -> CallResult<Value> {
        let name = match key {
            Value::String(s) => s.to_string_lossy(),
            other => ex.present(other)?.to_string_lossy(),
        };
        match self.find_member(&name) {
            Some(m) => CategoryManager::value(m),
            None => Err(HostError::key(PROP_NOT_FOUND).with_arg(name).raw().into()),
        }
    }
}
