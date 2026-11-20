mod due;
mod job;
mod present;
mod schedule;
mod store;

use crate::Application;
use crate::errors::{self, keys};
use crate::meta::{MdKey, MdPath, arg_value, key_with_app, member_name, no_field, type_mismatch};
use crate::session::call_ctx;
use bsl_prim::fold_name;
use bsl_vm::{
    Arg, CallResult, Execution, HostError, HostObject, MemberId, Registry, TypeId, TypeIdx,
    TypeImpl, Value,
};
use ibschema::values::Uuid16;
use job::{Job, Origin};
use md_model::{MdKind, Value as MdValue};
use platform_catalog::catalog;
use platform_catalog::messages::Lang;
use std::any::Any;
use std::rc::Rc;
use std::sync::Arc;
use store::JobRow;

const MANAGER: &str = "РегламентныеЗадания";

pub(crate) fn refusal(type_name: &str, what: &str) -> HostError {
    HostError::not_implemented(type_name, what)
}

pub(crate) fn register(app: &Arc<Application>, reg: &mut Registry) {
    let c = catalog();
    let (Some(manager), Some(job), Some(sched)) = (
        c.find_type("МенеджерРегламентныхЗаданий"),
        c.find_type("РегламентноеЗадание"),
        c.find_type(schedule::TYPE),
    ) else {
        return;
    };
    reg.type_impl(
        sched,
        TypeImpl {
            ctor: Some(Arc::new(move |_, _| {
                Ok(schedule::JobSchedule::new_value(
                    sched,
                    schedule::Schedule::default(),
                ))
            })),
        },
    );
    let app = app.clone();
    reg.global_prop(
        MANAGER,
        "ScheduledJobs",
        Arc::new(move |_| {
            Ok(Value::Object(Rc::new(Manager {
                app: app.clone(),
                ty: manager,
                job,
                sched,
            })))
        }),
    );
}

struct Manager {
    app: Arc<Application>,
    ty: TypeIdx,
    job: TypeIdx,
    sched: TypeIdx,
}

#[derive(Default)]
struct Filter {
    md: Option<Uuid16>,
    use_: Option<bool>,
    predefined: Option<bool>,
    id: Option<Uuid16>,
    key: Option<String>,
    empty_description: bool,
}

impl Filter {
    fn keeps(&self, r: &JobRow) -> bool {
        self.md.is_none_or(|m| m == r.md)
            && self.use_.is_none_or(|u| u == r.use_)
            && self.predefined.is_none_or(|p| p == r.predefined)
            && self.id.is_none_or(|i| i == r.id)
            && self.key.as_ref().is_none_or(|k| *k == r.key)
    }
}

impl Manager {
    fn refuse(&self, what: &str) -> HostError {
        refusal(MANAGER, what)
    }

    fn admit(&self, ex: &mut Execution, member: &str) -> CallResult<()> {
        if call_ctx(ex).is_some_and(|c| c.sess.user().no_users) {
            Ok(())
        } else {
            Err(self
                .refuse(&format!("{member}: сеанс с пользователями"))
                .into())
        }
    }

    fn unknown_name(name: &str) -> HostError {
        let both = |lang: Lang| {
            format!(
                "{} ({name})",
                errors::render(keys::PARAMETER_VALUE_MISMATCH, &["1"], lang)
            )
        };
        HostError::text(&both(Lang::Ru), &both(Lang::En))
    }

    fn md_value_job(&self, v: &Value) -> Option<Option<usize>> {
        match key_with_app(v) {
            Some((MdKey::Path(MdPath::Other(i)), app)) if Arc::ptr_eq(app, &self.app) => Some(
                self.app
                    .model()
                    .config
                    .objects
                    .get(i)
                    .filter(|o| o.kind == MdKind::ScheduledJob)
                    .map(|_| i),
            ),
            Some((_, app)) if Arc::ptr_eq(app, &self.app) => Some(None),
            _ => None,
        }
    }

    fn job_md(&self, v: &Value, member: &str) -> CallResult<usize> {
        let cfg = &self.app.model().config;
        let index = match v {
            Value::String(s) => {
                let name = fold_name(&s.to_string_lossy());
                cfg.objects
                    .iter()
                    .position(|o| o.kind == MdKind::ScheduledJob && fold_name(&o.name) == name)
                    .ok_or_else(|| {
                        self.refuse(&format!(
                            "{member}: нет регламентного задания с таким именем"
                        ))
                    })?
            }
            _ => match key_with_app(v) {
                Some((MdKey::Path(MdPath::Other(i)), app)) if Arc::ptr_eq(app, &self.app) => i,
                _ => {
                    return Err(self
                        .refuse(&format!("{member}: значение не объект метаданных"))
                        .into());
                }
            },
        };
        match cfg.objects.get(index) {
            Some(o) if o.kind == MdKind::ScheduledJob => Ok(index),
            _ => Err(self
                .refuse(&format!("{member}: не регламентное задание"))
                .into()),
        }
    }

    fn rows(&self, member: &str) -> CallResult<Vec<JobRow>> {
        let rows =
            store::read_all(&self.app).map_err(|why| self.refuse(&format!("{member}: {why}")))?;
        let cfg = &self.app.model().config;
        if let Some(r) = rows.iter().find(|r| {
            !cfg.objects
                .iter()
                .any(|o| o.kind == MdKind::ScheduledJob && o.id == r.md)
        }) {
            return Err(self
                .refuse(&format!(
                    "{member}: строка задания без объекта метаданных {}",
                    r.md
                ))
                .into());
        }
        Ok(rows)
    }

    fn value(&self, r: JobRow) -> Value {
        Job::new_value(&self.app, self.job, self.sched, Origin::Stored(Box::new(r)))
    }

    fn filter(&self, v: &Value, member: &str) -> CallResult<Filter> {
        let mut f = Filter::default();
        if matches!(v, Value::Undefined) {
            return Ok(f);
        }
        if bsl_stdlib::array_items(v).is_some() {
            return Err(type_mismatch(1).into());
        }
        let fields = bsl_stdlib::structure_fields(v)
            .ok_or_else(|| self.refuse(&format!("{member}: Отбор не Структура")))?;
        for (key, value) in fields {
            let bad = || self.refuse(&format!("{member}: Отбор.{key}: значение"));
            match fold_name(&key).as_str() {
                "метаданные" | "metadata" => {
                    let i = self.job_md(&value, member)?;
                    f.md = self.app.model().config.objects.get(i).map(|o| o.id);
                }
                "использование" | "use" => match value {
                    Value::Bool(b) => f.use_ = Some(b),
                    _ => return Err(bad().into()),
                },
                "предопределенное" | "predefined" => match value {
                    Value::Bool(b) => f.predefined = Some(b),
                    _ => return Err(bad().into()),
                },
                "уникальныйидентификатор" | "uuid" => {
                    let b = bsl_stdlib::values::uuid_bytes(&value).ok_or_else(bad)?;
                    f.id = Some(Uuid16::from_bytes(b));
                }
                "key" | "description" => {
                    return Err(self.refuse(&format!("{member}: Отбор.{key}")).into());
                }
                "ключ" => match value {
                    Value::String(s) => f.key = Some(s.to_string_lossy()),
                    _ => return Err(bad().into()),
                },
                "наименование" => match value {
                    Value::String(s) if s.to_string_lossy().is_empty() => {
                        f.empty_description = true
                    }
                    _ => return Err(bad().into()),
                },
                _ => {}
            }
        }
        Ok(f)
    }

    fn get_jobs(&self, ex: &mut Execution, args: &[Arg], member: &str) -> CallResult<Value> {
        let v = arg_value(ex, args, 0)?;
        let f = self.filter(&v, member)?;
        let all = self.rows(member)?;
        if f.empty_description && all.iter().any(|r| !r.description.is_empty()) {
            return Err(self
                .refuse(&format!(
                    "{member}: Отбор.Наименование при непустом наименовании"
                ))
                .into());
        }
        let mut rows: Vec<JobRow> = all.into_iter().filter(|r| f.keeps(r)).collect();
        if rows.len() > 1 {
            if rows.iter().any(|r| !r.predefined) {
                return Err(self
                    .refuse(&format!(
                        "{member}: порядок с непредопределенными заданиями"
                    ))
                    .into());
            }
            let cfg = &self.app.model().config;
            let at = |r: &JobRow| cfg.objects.iter().position(|o| o.id == r.md);
            rows.sort_by_key(at);
        }
        Ok(bsl_stdlib::new_array(
            rows.into_iter().map(|r| self.value(r)).collect(),
        ))
    }

    fn find_predefined(&self, ex: &mut Execution, args: &[Arg], member: &str) -> CallResult<Value> {
        let v = arg_value(ex, args, 0)?;
        let i = match (&v, self.md_value_job(&v)) {
            (_, Some(None)) => return Ok(Value::Undefined),
            (_, Some(Some(i))) => i,
            (Value::String(s), None) => {
                let name = s.to_string_lossy();
                let folded = fold_name(&name);
                self.app
                    .model()
                    .config
                    .objects
                    .iter()
                    .position(|o| o.kind == MdKind::ScheduledJob && fold_name(&o.name) == folded)
                    .ok_or_else(|| Manager::unknown_name(&name))?
            }
            _ => self.job_md(&v, member)?,
        };
        let md = self.app.model().config.objects.get(i).map(|o| o.id);
        let mut found = self
            .rows(member)?
            .into_iter()
            .filter(|r| r.predefined && Some(r.md) == md);
        match (found.next(), found.next()) {
            (Some(r), None) => Ok(self.value(r)),
            (None, _) => Err(self
                .refuse(&format!(
                    "{member}: в базе нет строки предопределенного задания"
                ))
                .into()),
            (Some(_), Some(_)) => Err(self
                .refuse(&format!("{member}: две строки предопределенного задания"))
                .into()),
        }
    }

    fn find_by_id(&self, ex: &mut Execution, args: &[Arg], member: &str) -> CallResult<Value> {
        let v = arg_value(ex, args, 0)?;
        if matches!(v, Value::String(_)) {
            return Err(type_mismatch(1).into());
        }
        let b = bsl_stdlib::values::uuid_bytes(&v).ok_or_else(|| {
            self.refuse(&format!("{member}: значение не УникальныйИдентификатор"))
        })?;
        let id = Uuid16::from_bytes(b);
        Ok(self
            .rows(member)?
            .into_iter()
            .find(|r| r.id == id)
            .map_or(Value::Undefined, |r| self.value(r)))
    }

    fn create(&self, ex: &mut Execution, args: &[Arg], member: &str) -> CallResult<Value> {
        let i = self.job_md(&arg_value(ex, args, 0)?, member)?;
        let o = self
            .app
            .model()
            .config
            .objects
            .get(i)
            .ok_or_else(|| self.refuse(member))?;
        if !matches!(o.prop("Predefined"), Some(MdValue::Bool(true))) {
            return Err(self
                .refuse(&format!(
                    "{member}: новое задание непредопределенного объекта не наблюдалось"
                ))
                .into());
        }
        let origin = Origin::New {
            md: o.id,
            id: crate::history::random_v4(),
        };
        Ok(Job::new_value(&self.app, self.job, self.sched, origin))
    }
}

impl HostObject for Manager {
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
        let Some((true, ru, en)) = member_name(catalog(), self.ty, m) else {
            return Err(no_field(&m.0.to_string()).into());
        };
        self.admit(ex, ru)?;
        match en {
            "GetScheduledJobs" => self.get_jobs(ex, args, ru),
            "FindPredefined" => self.find_predefined(ex, args, ru),
            "FindByUUID" => self.find_by_id(ex, args, ru),
            "CreateScheduledJob" => self.create(ex, args, ru),
            _ => Err(self.refuse(ru).into()),
        }
    }

    fn boost_hash(&self) -> Option<i32> {
        Some(0x7363_6800 | i32::from(self.ty.0 as u8))
    }

    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        Some(
            other
                .as_any()
                .downcast_ref::<Manager>()
                .is_some_and(|o| Arc::ptr_eq(&o.app, &self.app)),
        )
    }
}
