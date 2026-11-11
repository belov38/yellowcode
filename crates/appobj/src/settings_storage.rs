use crate::Application;
use crate::meta::{arg_value, member_name};
use crate::read::{admit_data_read, internal, read_view, store_call_error};
use crate::read_budget as budget;
use crate::txn::CallTxn;
use bsl_prim::fold_name;
use bsl_vm::{
    Arg, CallError, CallResult, Execution, HostError, HostObject, MemberId, Registry, TypeId, Value,
};
use ibschema::fields::FieldValue;
use ibschema::values::Uuid16;
use platform_catalog::{TypeIdx, catalog};
use std::any::Any;
use std::rc::Rc;
use std::sync::Arc;

const STORAGES: [(&str, &str, Option<&str>); 2] = [
    (
        "ХранилищеОбщихНастроек",
        "CommonSettings",
        Some("CommonSettingsStorage"),
    ),
    ("ХранилищеСистемныхНастроек", "SystemSettings", None),
];

pub(crate) fn register(app: &Arc<Application>, reg: &mut Registry) {
    let catalog = catalog();
    for (ru, table, configured) in STORAGES {
        if configured.is_some_and(|prop| names_a_storage(app, prop)) {
            continue;
        }
        let Some(prop) = catalog.find_global_property(ru) else {
            continue;
        };
        let Some(ty) = prop
            .types
            .first()
            .and_then(|t| catalog.find_type(&t.name_ru))
        else {
            continue;
        };
        let app = app.clone();
        reg.global_prop(
            ru,
            &prop.name_en,
            Arc::new(move |_| {
                Ok(Value::Object(Rc::new(Storage {
                    app: app.clone(),
                    table,
                    ty,
                })))
            }),
        );
    }
}

fn names_a_storage(app: &Application, prop: &str) -> bool {
    use md_model::Value as Mv;
    let named = |value: &Mv| match value {
        Mv::Ref(r) => r.id.is_some_and(|id| id != Uuid16::NIL) || !r.name.is_empty(),
        Mv::Text(t) => !t.is_empty(),
        _ => true,
    };
    let props = &app.model().config.props;
    props.iter().any(|p| p.name == prop && named(&p.value))
}

fn refusal(member: &str) -> HostError {
    HostError::not_implemented("СтандартноеХранилищеНастроекМенеджер", member)
}

struct Storage {
    app: Arc<Application>,
    table: &'static str,
    ty: TypeIdx,
}

fn text_arg(ex: &mut Execution, args: &[Arg], at: usize, what: &str) -> CallResult<Option<String>> {
    match arg_value(ex, args, at)? {
        Value::Undefined => Ok(None),
        Value::String(s) => Ok(Some(s.to_string_lossy())),
        _ => Err(refusal(&format!("Загрузить: {what}")).into()),
    }
}

fn key_of(row: &[FieldValue], at: usize) -> Result<String, HostError> {
    match row.get(at) {
        Some(FieldValue::String(s)) => Ok(fold_name(s)),
        Some(FieldValue::Null) => Ok(String::new()),
        _ => Err(internal(
            "the settings table holds a key that is not a string",
        )),
    }
}

impl Storage {
    fn load(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        admit_data_read(ex)?;
        let object = match arg_value(ex, args, 0)? {
            Value::String(s) => Some(fold_name(&s.to_string_lossy())),
            Value::Number(n) => Some(fold_name(&n.to_string())),
            Value::Undefined => None,
            _ => return Err(refusal("Загрузить: ключ объекта").into()),
        };
        let settings = fold_name(&text_arg(ex, args, 1, "ключ настроек")?.unwrap_or_default());
        if !matches!(arg_value(ex, args, 2)?, Value::Undefined) {
            return Err(refusal("Загрузить: описание настроек").into());
        }
        let named = text_arg(ex, args, 3, "имя пользователя")?.is_some_and(|n| !n.is_empty());

        let storage = self.app.store().storage();
        let id = storage
            .table_by_name(self.table)
            .ok_or_else(|| internal("the base has no table of the settings storage"))?;
        let table = storage.table(id);
        if !table.separators.is_empty() && self.app.options().area().is_none() {
            return Err(refusal("Загрузить: Разделение данных").into());
        }
        let Some(object) = object.filter(|_| !named) else {
            return Ok(Value::Undefined);
        };
        let column = |name: &str| {
            let field = table.field_by_name(name);
            field
                .map(|f| usize::from(f.0))
                .ok_or_else(|| internal("the settings table lacks a column"))
        };
        let (user_at, object_at, settings_at) = (
            column("UserId")?,
            column("ObjectKey")?,
            column("SettingsKey")?,
        );

        let call_txn = CallTxn::of_call(ex);
        let mut view = read_view(ex, call_txn.as_deref(), &self.app)?;
        let ceiling = ex.options().memory_limit.unwrap_or(u64::MAX);
        let remaining = ceiling.saturating_sub(ex.retained_workspace_bytes());
        let memory = view
            .read_memory(remaining)
            .map_err(|e| store_call_error(ex, e))?;
        let txn = view.txn();
        for item in txn.scan(id).map_err(|e| store_call_error(ex, e))? {
            ex.charge(1)?;
            let (_, row) = item.map_err(|e| store_call_error(ex, e))?;
            budget::admit(ex, &memory, budget::stored_row(&row))?;
            if key_of(&row, user_at)?.is_empty()
                && key_of(&row, object_at)? == object
                && key_of(&row, settings_at)? == settings
            {
                return Err(refusal("Загрузить: запись в хранилище").into());
            }
        }
        Ok(Value::Undefined)
    }

    fn unmodeled(&self, m: MemberId) -> CallError {
        match member_name(catalog(), self.ty, m) {
            Some((_, ru, _)) => refusal(ru).into(),
            None => internal("unknown member of a settings storage").into(),
        }
    }
}

impl HostObject for Storage {
    fn retained_memory(
        &self,
        visit: &mut dyn bsl_vm::RetainedMemory,
    ) -> Result<bool, bsl_vm::Exception> {
        visit.work(1)?;
        Ok(true)
    }
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.ty)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        Err(self.unmodeled(m))
    }
    fn set(&self, _: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        Err(self.unmodeled(m))
    }
    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        match member_name(catalog(), self.ty, m) {
            Some((true, "Загрузить", _)) => self.load(ex, args),
            _ => Err(self.unmodeled(m)),
        }
    }
    fn boost_hash(&self) -> Option<i32> {
        Some(0x7374_0000 | i32::from(self.ty.0))
    }
    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        let same = |o: &Storage| Arc::ptr_eq(&o.app, &self.app) && o.table == self.table;
        Some(other.as_any().downcast_ref::<Storage>().is_some_and(same))
    }
}
