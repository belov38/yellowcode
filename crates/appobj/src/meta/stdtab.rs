use super::object::read_only;
use super::{MdCtx, fold, no_field, not_implemented};
use bsl_prim::Str;
use bsl_vm::{Arg, CallResult, Execution, HostObject, MemberId, MemberKind, TypeId, Value};
use platform_catalog::messages::Lang;
use std::any::Any;
use std::sync::Arc;

const TABS: [(&str, &str, &str); 3] = [
    (
        "ВедущиеВидыРасчета",
        "LeadingCalculationTypes",
        "Ведущие виды расчета",
    ),
    (
        "ВытесняющиеВидыРасчета",
        "DisplacingCalculationTypes",
        "Вытесняющие виды расчета",
    ),
    (
        "БазовыеВидыРасчета",
        "BaseCalculationTypes",
        "Базовые виды расчета",
    ),
];
const ATTRS: [(&str, &str); 3] = [
    ("Предопределенный", "Predefined"),
    ("ВидРасчета", "CalculationType"),
    ("НомерСтроки", "LineNumber"),
];

fn word(cx: &MdCtx, ru: &str, en: &str) -> String {
    match cx.script_lang() {
        Lang::Ru => ru,
        Lang::En => en,
    }
    .to_string()
}

fn type_of(cx: &MdCtx, ru: &str) -> TypeId {
    cx.catalog
        .find_type(ru)
        .map_or(cx.t_obj, TypeId::of_catalog)
}

fn name_of(cx: &MdCtx, type_ru: &str, m: MemberId) -> Option<String> {
    let t = cx.catalog.find_type(type_ru)?;
    super::member_name(cx.catalog, t, m).map(|(_, ru, _)| fold(ru))
}

pub(crate) fn tabs(cx: &Arc<MdCtx>) -> Value {
    Value::Object(std::rc::Rc::new(StdTabs { cx: cx.clone() }))
}

struct StdTabs {
    cx: Arc<MdCtx>,
}

struct StdTab {
    cx: Arc<MdCtx>,
    i: usize,
}

struct StdTabAttr {
    cx: Arc<MdCtx>,
    i: usize,
}

fn tab_value(cx: &Arc<MdCtx>, i: usize) -> Value {
    Value::Object(std::rc::Rc::new(StdTab { cx: cx.clone(), i }))
}

impl HostObject for StdTabs {
    fn type_id(&self) -> TypeId {
        type_of(&self.cx, "ОписанияСтандартныхТабличныхЧастей")
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        let i = m.0 as usize;
        if i < TABS.len() {
            Ok(tab_value(&self.cx, i))
        } else {
            Err(no_field(&m.0.to_string()).into())
        }
    }
    fn set(&self, _: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        Err(read_only(TABS.get(m.0 as usize).map_or("", |t| t.0)).into())
    }
    fn call(&self, _: &mut Execution, m: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        Err(not_implemented(&m.0.to_string()).into())
    }
    fn find_member(&self, name: &str) -> Option<MemberId> {
        let f = fold(name);
        TABS.iter()
            .position(|t| fold(t.0) == f || fold(t.1) == f)
            .and_then(|i| u32::try_from(i).ok())
            .map(MemberId)
    }
    fn member_kind(&self, m: MemberId) -> Option<MemberKind> {
        ((m.0 as usize) < TABS.len()).then_some(MemberKind::Property {
            readable: true,
            writable: false,
        })
    }
    fn iter_len(&self, _: &mut Execution) -> Option<usize> {
        Some(TABS.len())
    }
    fn iter_get(&self, _: &mut Execution, i: usize) -> CallResult<Value> {
        if i < TABS.len() {
            Ok(tab_value(&self.cx, i))
        } else {
            Err(no_field(&i.to_string()).into())
        }
    }
    fn presentation(&self, _: &mut Execution) -> CallResult<Option<Str>> {
        Ok(Some(Str::from(
            word(
                &self.cx,
                "ОписанияСтандартныхТабличныхЧастей",
                "StandardTabularSectionDescriptions",
            )
            .as_str(),
        )))
    }
    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        Some(other.as_any().is::<StdTabs>())
    }
}

impl HostObject for StdTab {
    fn type_id(&self) -> TypeId {
        type_of(&self.cx, "ОписаниеСтандартнойТабличнойЧасти")
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        let Some(t) = TABS.get(self.i) else {
            return Err(no_field("").into());
        };
        Ok(
            match name_of(&self.cx, "ОписаниеСтандартнойТабличнойЧасти", m).as_deref()
            {
                Some("имя") => Value::from(word(&self.cx, t.0, t.1).as_str()),
                Some("синоним" | "комментарий" | "подсказка") => {
                    Value::from("")
                }
                Some("стандартныереквизиты") => {
                    let attrs = (0..ATTRS.len())
                        .map(|i| {
                            Value::Object(std::rc::Rc::new(StdTabAttr {
                                cx: self.cx.clone(),
                                i,
                            }))
                        })
                        .collect();
                    bsl_stdlib::new_array(attrs)
                }
                _ => return Err(not_implemented(&m.0.to_string()).into()),
            },
        )
    }
    fn set(&self, _: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        Err(read_only(&m.0.to_string()).into())
    }
    fn call(&self, _: &mut Execution, m: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        let Some(t) = TABS.get(self.i) else {
            return Err(no_field("").into());
        };
        match name_of(&self.cx, "ОписаниеСтандартнойТабличнойЧасти", m).as_deref()
        {
            Some("представление") => Ok(Value::from(t.2)),
            _ => Err(not_implemented(&m.0.to_string()).into()),
        }
    }
    fn presentation(&self, _: &mut Execution) -> CallResult<Option<Str>> {
        Ok(Some(Str::from(
            word(
                &self.cx,
                "ОписаниеСтандартнойТабличнойЧасти",
                "StandardTabularSectionDescription",
            )
            .as_str(),
        )))
    }
    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        Some(
            other
                .as_any()
                .downcast_ref::<StdTab>()
                .is_some_and(|o| o.i == self.i),
        )
    }
}

impl HostObject for StdTabAttr {
    fn type_id(&self) -> TypeId {
        type_of(&self.cx, "ОписаниеСтандартногоРеквизита")
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        let Some(a) = ATTRS.get(self.i) else {
            return Err(no_field("").into());
        };
        Ok(
            match name_of(&self.cx, "ОписаниеСтандартногоРеквизита", m).as_deref()
            {
                Some("имя") => Value::from(word(&self.cx, a.0, a.1).as_str()),
                Some("синоним" | "комментарий") => Value::from(""),
                _ => return Err(not_implemented(&m.0.to_string()).into()),
            },
        )
    }
    fn set(&self, _: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        Err(read_only(&m.0.to_string()).into())
    }
    fn call(&self, _: &mut Execution, m: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        Err(not_implemented(&m.0.to_string()).into())
    }
    fn presentation(&self, _: &mut Execution) -> CallResult<Option<Str>> {
        Ok(Some(Str::from(
            word(
                &self.cx,
                "ОписаниеСтандартногоРеквизита",
                "StandardAttributeDescription",
            )
            .as_str(),
        )))
    }
    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        Some(
            other
                .as_any()
                .downcast_ref::<StdTabAttr>()
                .is_some_and(|o| o.i == self.i),
        )
    }
}
