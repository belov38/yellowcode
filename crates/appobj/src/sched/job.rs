use super::refusal;
use super::schedule::{JobSchedule, Schedule};
use super::store::JobRow;
use crate::Application;
use crate::meta::{MdKey, MdPath, member_name, no_field, value_of};
use bsl_vm::{
    Arg, CallResult, Exception, Execution, HostObject, MemberId, Number, RetainedMemory, TypeId,
    TypeIdx, Value, creation_number,
};
use ibschema::values::Uuid16;
use platform_catalog::catalog;
use std::any::Any;
use std::rc::Rc;
use std::sync::Arc;

pub(crate) const TYPE: &str = "РегламентноеЗадание";

#[derive(Clone, Debug)]
pub(crate) enum Origin {
    Stored(Box<JobRow>),
    New { md: Uuid16, id: Uuid16 },
}

pub(crate) struct Job {
    app: Arc<Application>,
    ty: TypeIdx,
    schedule_ty: TypeIdx,
    origin: Origin,
    hash: i32,
}

impl Job {
    pub(crate) fn new_value(
        app: &Arc<Application>,
        ty: TypeIdx,
        schedule_ty: TypeIdx,
        origin: Origin,
    ) -> Value {
        Value::Object(Rc::new(Job {
            app: app.clone(),
            ty,
            schedule_ty,
            origin,
            hash: creation_number() as i32,
        }))
    }

    fn md(&self) -> Uuid16 {
        match &self.origin {
            Origin::Stored(r) => r.md,
            Origin::New { md, .. } => *md,
        }
    }

    fn id(&self) -> Uuid16 {
        match &self.origin {
            Origin::Stored(r) => r.id,
            Origin::New { id, .. } => *id,
        }
    }

    fn metadata(&self, ex: &mut Execution) -> CallResult<Value> {
        let cx = self
            .app
            .md_ctx()
            .ok_or_else(|| refusal(TYPE, "Метаданные: нет контекста метаданных"))?;
        let i = *cx
            .cfg_index
            .get(&self.md())
            .ok_or_else(|| refusal(TYPE, "Метаданные: объект не найден в конфигурации"))?;
        Ok(value_of(&cx, ex, MdKey::Path(MdPath::Other(i))))
    }

    fn schedule(&self) -> CallResult<Value> {
        let s = match &self.origin {
            Origin::Stored(r) => r
                .schedule
                .clone()
                .map_err(|why| refusal(TYPE, &format!("Расписание: {why}")))?,
            Origin::New { .. } => self
                .md_object()
                .and_then(|o| o.parts.schedule.as_ref())
                .ok_or_else(|| refusal(TYPE, "Расписание: у объекта метаданных нет расписания"))
                .and_then(|m| {
                    Schedule::of_metadata(m)
                        .map_err(|why| refusal(TYPE, &format!("Расписание: {why}")))
                })?,
        };
        Ok(JobSchedule::new_value(self.schedule_ty, s))
    }

    fn md_object(&self) -> Option<&md_model::MdObject> {
        let md = self.md();
        self.app.model().config.objects.iter().find(|o| o.id == md)
    }

    fn md_prop(&self, name: &str) -> CallResult<md_model::Value> {
        self.md_object()
            .and_then(|o| o.prop(name).cloned())
            .ok_or_else(|| refusal(TYPE, &format!("{name}: нет свойства метаданных")).into())
    }

    fn md_number(&self, name: &str) -> CallResult<Value> {
        match self.md_prop(name)? {
            md_model::Value::Text(t) => t
                .trim()
                .parse::<i64>()
                .map(|n| Value::Number(Number::from_i64(n)))
                .map_err(|_| refusal(TYPE, &format!("{name}: не число")).into()),
            _ => Err(refusal(TYPE, &format!("{name}: не число")).into()),
        }
    }

    fn separation(&self) -> CallResult<Value> {
        let separated = self.app.model().config.objects.iter().any(|o| {
            o.kind == md_model::MdKind::CommonAttribute
                && matches!(o.prop("DataSeparation"), Some(md_model::Value::Text(t)) if t == "Separate")
        });
        if separated {
            return Err(refusal(TYPE, "РазделениеДанных: конфигурация с разделителями").into());
        }
        Ok(bsl_stdlib::new_structure(Vec::new()))
    }
}

impl HostObject for Job {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.ty)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let Some((false, ru, en)) = member_name(catalog(), self.ty, m) else {
            return Err(no_field(&m.0.to_string()).into());
        };
        let stored = match &self.origin {
            Origin::Stored(r) => Some(r),
            Origin::New { .. } => None,
        };
        let number = |n: i64| Value::Number(Number::from_i64(n));
        match ru {
            "ПоследнееЗадание" => return Ok(Value::Undefined),
            "РазделениеДанных" => return self.separation(),
            _ => {}
        }
        Ok(match (en, stored) {
            ("UUID", _) => bsl_stdlib::values::new_uuid(*self.id().as_bytes()),
            ("Metadata", _) => return self.metadata(ex),
            ("Schedule", _) => return self.schedule(),
            ("Predefined", Some(r)) => Value::Bool(r.predefined),
            ("Predefined", None) => Value::Bool(false),
            ("Use", Some(r)) => Value::Bool(r.use_),
            ("Use", None) => match self.md_prop("Use")? {
                md_model::Value::Bool(false) => Value::Bool(false),
                _ => {
                    return Err(
                        refusal(TYPE, "Использование нового задания: не наблюдалось").into(),
                    );
                }
            },
            ("UserName", Some(r)) => Value::from(r.user_name.as_str()),
            ("UserName" | "Description", None) => Value::from(""),
            ("Key", None) => match self.md_prop("Key")? {
                md_model::Value::Text(t) => Value::from(t.as_str()),
                _ => return Err(refusal(TYPE, "Ключ: не строка").into()),
            },
            ("RestartCountOnFailure", Some(r)) => number(r.restart_count),
            ("RestartCountOnFailure", None) => return self.md_number("RestartCountOnFailure"),
            ("RestartIntervalOnFailure", Some(r)) => number(r.restart_period),
            ("RestartIntervalOnFailure", None) => {
                return self.md_number("RestartIntervalOnFailure");
            }
            ("Parameters", Some(r)) if !r.parameters_empty => {
                return Err(refusal(TYPE, "Параметры: форма хранения не наблюдалась").into());
            }
            ("Parameters", _) => bsl_stdlib::new_array(Vec::new()),
            ("Key", Some(r)) => Value::from(r.key.as_str()),
            ("Description", Some(r)) => Value::from(r.description.as_str()),
            _ => return Err(refusal(TYPE, ru).into()),
        })
    }

    fn set(&self, _: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        match member_name(catalog(), self.ty, m) {
            Some((_, ru, _)) => Err(refusal(TYPE, &format!("{ru} (запись)")).into()),
            None => Err(no_field(&m.0.to_string()).into()),
        }
    }

    fn call(&self, _: &mut Execution, m: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        match member_name(catalog(), self.ty, m) {
            Some((_, ru, _)) => Err(refusal(TYPE, ru).into()),
            None => Err(no_field(&m.0.to_string()).into()),
        }
    }

    fn presentation(&self, _: &mut Execution) -> CallResult<Option<bsl_prim::Str>> {
        let empty = match &self.origin {
            Origin::Stored(r) => r.description.is_empty(),
            Origin::New { .. } => true,
        };
        let name = self.md_object().map(|o| o.name.clone());
        match (empty, name) {
            (true, Some(n)) => Ok(Some(bsl_prim::Str::from(format!("{n}:").as_str()))),
            _ => Err(refusal(TYPE, "Представление: с наименованием не наблюдалось").into()),
        }
    }

    fn boost_hash(&self) -> Option<i32> {
        Some(self.hash)
    }

    fn retained_memory(&self, _: &mut dyn RetainedMemory) -> Result<bool, Exception> {
        Ok(false)
    }
}
