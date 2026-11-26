use super::refusal;
use super::schedule::{JobSchedule, Schedule};
use super::store::JobRow;
use crate::Application;
use crate::meta::{MdKey, MdPath, member_name, no_field, value_of};
use bsl_vm::{
    Arg, CallResult, Category, CategorySet, Exception, Execution, HostError, HostObject, MemberId,
    Number, RetainedMemory, TypeId, TypeIdx, Value, creation_number,
};
use ibschema::values::Uuid16;
use platform_catalog::catalog;
use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

pub(crate) const TYPE: &str = "РегламентноеЗадание";

#[derive(Clone, Debug)]
pub(crate) enum Origin {
    Stored(Box<JobRow>),
    New { md: Uuid16, id: Uuid16 },
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Edit {
    pub use_: Option<bool>,
    pub user_name: Option<String>,
    pub description: Option<String>,
    pub key: Option<String>,
    pub restart_count: Option<i64>,
    pub restart_period: Option<i64>,
    pub schedule: Option<Schedule>,
    pub parameters_empty: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct Values {
    pub use_: bool,
    pub user_name: String,
    pub description: String,
    pub key: String,
    pub restart_count: i64,
    pub restart_period: i64,
    pub schedule: Schedule,
}

pub(crate) struct Job {
    pub(super) app: Arc<Application>,
    ty: TypeIdx,
    schedule_ty: TypeIdx,
    pub(super) origin: RefCell<Origin>,
    pub(super) edit: RefCell<Edit>,
    pub(super) deleted: std::cell::Cell<bool>,
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
            origin: RefCell::new(origin),
            edit: RefCell::new(Edit::default()),
            deleted: std::cell::Cell::new(false),
            hash: creation_number() as i32,
        }))
    }

    pub(super) fn md(&self) -> Uuid16 {
        match &*self.origin.borrow() {
            Origin::Stored(r) => r.md,
            Origin::New { md, .. } => *md,
        }
    }

    pub(super) fn id(&self) -> Uuid16 {
        match &*self.origin.borrow() {
            Origin::Stored(r) => r.id,
            Origin::New { id, .. } => *id,
        }
    }

    pub(super) fn stored(&self) -> Option<JobRow> {
        match &*self.origin.borrow() {
            Origin::Stored(r) => Some((**r).clone()),
            Origin::New { .. } => None,
        }
    }

    pub(super) fn values(&self) -> CallResult<Values> {
        let e = self.edit.borrow().clone();
        let base = match self.stored() {
            Some(r) => Values {
                use_: r.use_,
                user_name: r.user_name.clone(),
                description: r.description.clone(),
                key: r.key.clone(),
                restart_count: r.restart_count,
                restart_period: r.restart_period,
                schedule: match &e.schedule {
                    Some(s) => s.clone(),
                    None => r
                        .schedule
                        .clone()
                        .map_err(|why| refusal(TYPE, &format!("Расписание: {why}")))?,
                },
            },
            None => Values {
                use_: match e.use_ {
                    Some(u) => u,
                    None => self.new_use()?,
                },
                user_name: String::new(),
                description: String::new(),
                key: self.new_key()?,
                restart_count: self.md_int("RestartCountOnFailure")?,
                restart_period: self.md_int("RestartIntervalOnFailure")?,
                schedule: match &e.schedule {
                    Some(s) => s.clone(),
                    None => self.md_schedule()?,
                },
            },
        };
        Ok(Values {
            use_: e.use_.unwrap_or(base.use_),
            user_name: e.user_name.unwrap_or(base.user_name),
            description: e.description.unwrap_or(base.description),
            key: e.key.unwrap_or(base.key),
            restart_count: e.restart_count.unwrap_or(base.restart_count),
            restart_period: e.restart_period.unwrap_or(base.restart_period),
            schedule: base.schedule,
        })
    }

    fn new_use(&self) -> CallResult<bool> {
        match self.md_prop("Use")? {
            md_model::Value::Bool(b) => Ok(b),
            _ => Err(refusal(TYPE, "Использование: не булево").into()),
        }
    }

    fn new_key(&self) -> CallResult<String> {
        match self.md_prop("Key")? {
            md_model::Value::Text(t) => Ok(t),
            _ => Err(refusal(TYPE, "Ключ: не строка").into()),
        }
    }

    fn md_schedule(&self) -> CallResult<Schedule> {
        Ok(self
            .md_object()
            .and_then(|o| o.parts.schedule.as_ref())
            .ok_or_else(|| refusal(TYPE, "Расписание: у объекта метаданных нет расписания"))
            .and_then(|m| {
                Schedule::of_metadata(m).map_err(|why| refusal(TYPE, &format!("Расписание: {why}")))
            })?)
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
        let assigned = self.edit.borrow().schedule.clone();
        let s = match (assigned, self.stored()) {
            (Some(s), _) => s,
            (None, Some(r)) => r
                .schedule
                .map_err(|why| refusal(TYPE, &format!("Расписание: {why}")))?,
            (None, None) => self.md_schedule()?,
        };
        Ok(JobSchedule::new_value(self.schedule_ty, s))
    }

    pub(super) fn md_object(&self) -> Option<&md_model::MdObject> {
        let md = self.md();
        self.app.model().config.objects.iter().find(|o| o.id == md)
    }

    fn md_prop(&self, name: &str) -> CallResult<md_model::Value> {
        self.md_object()
            .and_then(|o| o.prop(name).cloned())
            .ok_or_else(|| refusal(TYPE, &format!("{name}: нет свойства метаданных")).into())
    }

    fn md_int(&self, name: &str) -> CallResult<i64> {
        match self.md_prop(name)? {
            md_model::Value::Text(t) => t
                .trim()
                .parse::<i64>()
                .map_err(|_| refusal(TYPE, &format!("{name}: не число")).into()),
            _ => Err(refusal(TYPE, &format!("{name}: не число")).into()),
        }
    }

    fn md_number(&self, name: &str) -> CallResult<Value> {
        self.md_int(name)
            .map(|n| Value::Number(Number::from_i64(n)))
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
        if self.deleted.get() {
            return Err(refusal(TYPE, &format!("{ru} после Удалить: не наблюдалось")).into());
        }
        let stored = self.stored();
        let stored = stored.as_ref();
        let number = |n: i64| Value::Number(Number::from_i64(n));
        {
            let e = self.edit.borrow();
            let assigned = match en {
                "Use" => e.use_.map(Value::Bool),
                "UserName" => e.user_name.as_deref().map(Value::from),
                "Description" => e.description.as_deref().map(Value::from),
                "Key" => e.key.as_deref().map(Value::from),
                "RestartCountOnFailure" => e.restart_count.map(number),
                "RestartIntervalOnFailure" => e.restart_period.map(number),
                "Parameters" if e.parameters_empty => Some(bsl_stdlib::new_array(Vec::new())),
                _ => None,
            };
            if let Some(v) = assigned {
                return Ok(v);
            }
        }
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
            ("Use", None) => Value::Bool(self.new_use()?),
            ("UserName", Some(r)) => Value::from(r.user_name.as_str()),
            ("UserName" | "Description", None) => Value::from(""),
            ("Key", None) => Value::from(self.new_key()?.as_str()),
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

    fn set(&self, _: &mut Execution, m: MemberId, v: Value) -> CallResult<()> {
        let Some((false, ru, en)) = member_name(catalog(), self.ty, m) else {
            return Err(match member_name(catalog(), self.ty, m) {
                Some((_, ru, _)) => refusal(TYPE, &format!("{ru} (запись)")).into(),
                None => no_field(&m.0.to_string()).into(),
            });
        };
        if self.deleted.get() {
            return Err(refusal(TYPE, &format!("{ru} после Удалить: не наблюдалось")).into());
        }
        let bad = || -> bsl_vm::CallError {
            refusal(TYPE, &format!("{ru}: значение другого типа")).into()
        };
        let text = |v: Value| match v {
            Value::String(s) => Ok(s.to_string_lossy()),
            _ => Err(bad()),
        };
        let count = |v: Value| match v {
            Value::Number(n) => n.to_i64().filter(|n| *n >= 0).ok_or_else(bad),
            _ => Err(bad()),
        };
        let mut e = self.edit.borrow_mut();
        match en {
            "Use" => match v {
                Value::Bool(b) => e.use_ = Some(b),
                _ => return Err(bad()),
            },
            "UserName" => e.user_name = Some(text(v)?),
            "Description" => e.description = Some(text(v)?),
            "Key" => e.key = Some(text(v)?),
            "RestartCountOnFailure" => e.restart_count = Some(count(v)?),
            "RestartIntervalOnFailure" => e.restart_period = Some(count(v)?),
            "Schedule" => e.schedule = Some(Schedule::of_value(&v).ok_or_else(bad)?),
            "Parameters" => match bsl_stdlib::array_items(&v) {
                Some(items) if items.is_empty() => e.parameters_empty = true,
                Some(_) => {
                    return Err(refusal(TYPE, &format!("{ru}: непустой массив (запись)")).into());
                }
                None => return Err(bad()),
            },
            _ => return Err(refusal(TYPE, &format!("{ru} (запись)")).into()),
        }
        Ok(())
    }

    fn call(&self, ex: &mut Execution, m: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        match member_name(catalog(), self.ty, m) {
            Some((true, _, "Write")) => super::write::write(ex, self).map(|()| Value::Undefined),
            Some((true, _, "Delete")) => super::write::delete(ex, self).map(|()| Value::Undefined),
            Some((_, ru, _)) => Err(refusal(TYPE, ru).into()),
            None => Err(no_field(&m.0.to_string()).into()),
        }
    }

    fn presentation(&self, _: &mut Execution) -> CallResult<Option<bsl_prim::Str>> {
        let description = match (self.edit.borrow().description.clone(), self.stored()) {
            (Some(d), _) => d,
            (None, Some(r)) => r.description,
            (None, None) => String::new(),
        };
        match self.md_object() {
            Some(o) => Ok(Some(bsl_prim::Str::from(
                format!("{}:{description}", o.name).as_str(),
            ))),
            None => Err(refusal(TYPE, "Представление: объект метаданных не найден").into()),
        }
    }

    fn filled_check(&self) -> Option<HostError> {
        let mut e = HostError::text(
            "Проверка мутабельных значений на заполненность не поддерживается",
            "Mutable values filling check is unavailable",
        );
        e.categories = Some(CategorySet::of(&[Category::OtherError]));
        Some(e)
    }

    fn boost_hash(&self) -> Option<i32> {
        Some(self.hash)
    }

    fn retained_memory(&self, _: &mut dyn RetainedMemory) -> Result<bool, Exception> {
        Ok(false)
    }
}
