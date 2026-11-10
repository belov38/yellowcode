use crate::session::internal;
use bsl_prim::{Number, Str, fold_name};
use bsl_stdlib::bind::{arg_value, msg};
use bsl_vm::error::{MsgKey, MsgRef};
use bsl_vm::{
    AppTypeInfo, Arg, CallError, CallResult, Execution, HostError, HostObject, MemberId,
    MemberKind, Registry, TypeId, Value,
};
use std::any::Any;
use std::rc::Rc;
use std::sync::Arc;

const MANAGER: &str = "МенеджерОбработкиСтрокиXML";
const PROPERTY_RU: &str = "ОбработкаСтрокиXML";
const FIND_RU: &str = "НайтиНедопустимыеСимволыXML";
const FIND_EN: &str = "FindDisallowedXMLCharacters";
const REMOVE_RU: &str = "УдалитьНедопустимыеСимволыXML";
const REMOVE_EN: &str = "DeleteDisallowedXMLCharacters";

const FIND_ID: u32 = 1;
const REMOVE_ID: u32 = 2;

const HASH: i32 = 0x786d_0000;

pub(crate) fn register(reg: &mut Registry) {
    let catalog = reg.catalog();
    let (Some(ty), Some(property)) = (
        catalog.find_type(MANAGER),
        catalog.find_global_property(PROPERTY_RU),
    ) else {
        return;
    };
    let info = catalog.type_info(ty);
    let Some(guid) = info.guid else {
        return;
    };
    let presentation = |value: &Option<String>, plain: &Option<String>, name: &String| {
        value
            .clone()
            .or_else(|| plain.clone())
            .unwrap_or_else(|| name.clone())
    };
    let t = reg.register_app_type(AppTypeInfo {
        name_ru: info.name_ru.clone(),
        name_en: info.name_en.clone(),
        presentation_ru: presentation(
            &info.value_type_presentation_ru,
            &info.presentation_ru,
            &info.name_ru,
        ),
        presentation_en: presentation(
            &info.value_type_presentation_en,
            &info.presentation_en,
            &info.name_en,
        ),
        guid,
        xdto: None,
        template: None,
        default_value: None,
    });
    let (ru, en) = (property.name_ru.clone(), property.name_en.clone());
    reg.global_prop(
        &ru,
        &en,
        Arc::new(move |_| Ok(Value::Object(Rc::new(XmlStrings { t })))),
    );
}

struct XmlStrings {
    t: TypeId,
}

impl HostObject for XmlStrings {
    fn type_id(&self) -> TypeId {
        self.t
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, _: &mut Execution, _: MemberId) -> CallResult<Value> {
        Err(internal("the manager of XML strings has no properties").into())
    }
    fn set(&self, _: &mut Execution, _: MemberId, _: Value) -> CallResult<()> {
        Err(internal("the manager of XML strings has no properties").into())
    }
    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        match m.0 {
            FIND_ID => find(ex, args),
            REMOVE_ID => remove(ex, args),
            _ => Err(internal("unknown method of the manager of XML strings").into()),
        }
    }
    fn find_member(&self, name: &str) -> Option<MemberId> {
        let name = fold_name(name);
        if name == fold_name(FIND_RU) || name == fold_name(FIND_EN) {
            Some(MemberId(FIND_ID))
        } else if name == fold_name(REMOVE_RU) || name == fold_name(REMOVE_EN) {
            Some(MemberId(REMOVE_ID))
        } else {
            None
        }
    }
    fn member_kind(&self, m: MemberId) -> Option<MemberKind> {
        let min_args = match m.0 {
            FIND_ID => 1,
            REMOVE_ID => 0,
            _ => return None,
        };
        Some(MemberKind::Method {
            is_function: true,
            min_args,
            max_args: None,
        })
    }
    fn boost_hash(&self) -> Option<i32> {
        Some(HASH)
    }
    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        Some(other.as_any().is::<XmlStrings>())
    }
}

fn refusal(method: &str, reason: &str) -> CallError {
    HostError::not_implemented(MANAGER, &format!("{method}: {reason}")).into()
}

fn value_mismatch(param: usize) -> CallError {
    let mut e = HostError::key(msg::VALUE_MISMATCH);
    e.msg = MsgRef::Key(MsgKey {
        key: msg::VALUE_MISMATCH,
        args: vec![param.to_string()],
        suffix: String::new(),
    });
    e.into()
}

fn text_arg(ex: &mut Execution, args: &[Arg], method: &str) -> CallResult<Str> {
    match arg_value(ex, args.first())? {
        Value::String(s) => Ok(s),
        _ => Err(refusal(
            method,
            "первый аргумент не строка (нативных данных нет)",
        )),
    }
}

fn version_arg(ex: &mut Execution, args: &[Arg], i: usize, method: &str) -> CallResult<()> {
    match arg_value(ex, args.get(i))? {
        Value::String(s) if s.to_string_lossy() == "1.1" => Ok(()),
        _ => Err(refusal(
            method,
            "версия XML: нативно известна только строка \"1.1\"",
        )),
    }
}

fn c1(unit: u16) -> bool {
    matches!(unit, 0x7F..=0x84 | 0x86..=0x9F)
}

fn find(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    let n = args.len();
    if n > 3 {
        return Err(refusal(
            FIND_RU,
            &format!(
                "аргументов {n}, нативно известны формы (Строка), (Строка, ПозицияНачала) и (Строка, ПозицияНачала, Версия)"
            ),
        ));
    }
    let text = text_arg(ex, args, FIND_RU)?;
    let start = if n > 1 {
        match arg_value(ex, args.get(1))? {
            Value::Number(x) if x.is_integer() => x.trunc_to_i64_saturating(),
            _ => {
                return Err(refusal(
                    FIND_RU,
                    "позиция начала не целое число (нативных данных нет)",
                ));
            }
        }
    } else {
        1
    };
    if n > 2 {
        version_arg(ex, args, 2, FIND_RU)?;
    }
    let len = i64::try_from(text.len()).unwrap_or(i64::MAX);
    if start < 1 || start > len.saturating_add(1) {
        return Err(value_mismatch(2));
    }
    if start > len {
        return Err(refusal(
            FIND_RU,
            "позиция начала на единицу больше длины строки (нативных данных нет)",
        ));
    }
    let Some(plain) = args.get_mut(..n.min(2)) else {
        return Err(internal("arguments of the manager of XML strings").into());
    };
    let Value::Number(found) = ex.call_global(FIND_RU, plain).map_err(CallError::Exc)? else {
        return Err(internal("the global function gave no number").into());
    };
    let mut found = found.trunc_to_i64_saturating();
    if n == 3 {
        let from = usize::try_from(start - 1).unwrap_or(usize::MAX);
        let tail = text.units().get(from..).unwrap_or_default();
        if let Some(i) = tail.iter().position(|&u| c1(u)) {
            let at = i64::try_from(from.saturating_add(i).saturating_add(1)).unwrap_or(i64::MAX);
            if found == 0 || at < found {
                found = at;
            }
        }
    }
    Ok(Value::Number(Number::from_i64(found)))
}

fn remove(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    let n = args.len();
    if !(1..=2).contains(&n) {
        return Err(refusal(
            REMOVE_RU,
            &format!("аргументов {n}, нативно известны формы (Строка) и (Строка, Версия)"),
        ));
    }
    text_arg(ex, args, REMOVE_RU)?;
    if n == 2 {
        version_arg(ex, args, 1, REMOVE_RU)?;
    }
    let Some(plain) = args.get_mut(..1) else {
        return Err(internal("arguments of the manager of XML strings").into());
    };
    let Value::String(kept) = ex.call_global(REMOVE_RU, plain).map_err(CallError::Exc)? else {
        return Err(internal("the global function gave no string").into());
    };
    if n == 1 {
        return Ok(Value::String(kept));
    }
    let kept = kept.units().iter().copied().filter(|&u| !c1(u)).collect();
    Ok(Value::String(Str::from_vec(kept)))
}
