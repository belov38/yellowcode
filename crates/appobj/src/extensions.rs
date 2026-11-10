use crate::Application;
use crate::meta::{arg_value, member_name, no_field, type_mismatch};
use bsl_vm::{
    Arg, CallResult, Execution, HostError, HostObject, MemberId, Registry, TypeId, Value,
};
use ibschema::store::Store;
use platform_catalog::{TypeIdx, catalog};
use std::any::Any;
use std::rc::Rc;
use std::sync::Arc;

const MANAGER: &str = "МенеджерРасширенийКонфигурации";
const SOURCE: &str = "ИсточникРасширенийКонфигурации";

const TABLES: [(&str, &str); 2] = [
    ("ExtensionsInfo", "чтение ExtensionsInfo не удалось"),
    ("ExtensionsInfoNGS", "чтение ExtensionsInfoNGS не удалось"),
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Presence {
    Absent,
    Present,
    Unknown(&'static str),
}

pub(crate) fn presence(store: &dyn Store) -> Presence {
    let map = store.storage();
    if map.table_by_name(TABLES[0].0).is_none() {
        return Presence::Unknown("ExtensionsInfo не описана в схеме базы");
    }
    let Ok(txn) = store.read() else {
        return Presence::Unknown("чтение базы не удалось");
    };
    for (name, unreadable) in TABLES {
        let Some(table) = map.table_by_name(name) else {
            continue;
        };
        let Ok(mut rows) = txn.scan(table) else {
            return Presence::Unknown(unreadable);
        };
        match rows.next() {
            Some(Ok(_)) => return Presence::Present,
            Some(Err(_)) => return Presence::Unknown(unreadable),
            None => {}
        }
    }
    Presence::Absent
}

pub(crate) fn register(app: &Arc<Application>, reg: &mut Registry) {
    let catalog = catalog();
    let (Some(ty), Some(source)) = (catalog.find_type(MANAGER), catalog.find_type(SOURCE)) else {
        return;
    };
    let app = app.clone();
    reg.global_prop(
        "РасширенияКонфигурации",
        "ConfigurationExtensions",
        Arc::new(move |_| {
            Ok(Value::Object(Rc::new(Manager {
                app: app.clone(),
                ty,
                source,
            })))
        }),
    );
}

struct Manager {
    app: Arc<Application>,
    ty: TypeIdx,
    source: TypeIdx,
}

fn refusal(member: &str) -> HostError {
    HostError::not_implemented(MANAGER, member)
}

impl Manager {
    fn check_get(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<()> {
        if !matches!(arg_value(ex, args, 0)?, Value::Undefined) {
            return Err(refusal("Получить: Отбор").into());
        }
        match arg_value(ex, args, 1)? {
            Value::Undefined => Ok(()),
            v if bsl_stdlib::enums::as_enum_value(&v)
                .is_some_and(|e| e.type_idx() == self.source) =>
            {
                Ok(())
            }
            _ => Err(type_mismatch(2).into()),
        }
    }

    fn none(&self, member: &str) -> CallResult<Value> {
        match self.app.extensions() {
            Presence::Absent => Ok(bsl_stdlib::new_array(Vec::new())),
            Presence::Present => Err(refusal(&format!("{member}: в базе есть расширения")).into()),
            Presence::Unknown(why) => Err(refusal(&format!("{member}: {why}")).into()),
        }
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
        match member_name(catalog(), self.ty, m) {
            Some((true, ru, "Get")) => {
                self.check_get(ex, args)?;
                self.none(ru)
            }
            Some((true, ru, "GetSessionApplicationIssuesInformation")) => self.none(ru),
            Some((true, ru, _)) => Err(refusal(ru).into()),
            _ => Err(no_field(&m.0.to_string()).into()),
        }
    }
    fn boost_hash(&self) -> Option<i32> {
        Some(0x6578_7400 | i32::from(self.ty.0))
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
