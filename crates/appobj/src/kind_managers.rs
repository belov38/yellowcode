use crate::meta::{member_name, no_field};
use crate::{Application, session::internal};
use bsl_prim::fold_name;
use bsl_vm::{
    Arg, CallResult, Execution, HostError, HostObject, MemberId, MemberKind, Registry, TypeId,
    Value,
};
use md_model::MdKind;
use platform_catalog::{TypeIdx, catalog};
use std::any::Any;
use std::rc::Rc;
use std::sync::Arc;

struct Spec {
    ru: &'static str,
    en: &'static str,
    manager: &'static str,
    kind: MdKind,
}

static SPECS: [Spec; 5] = [
    Spec {
        ru: "КритерииОтбора",
        en: "FilterCriteria",
        manager: "КритерииОтбораМенеджер",
        kind: MdKind::FilterCriterion,
    },
    Spec {
        ru: "ХранилищаНастроек",
        en: "SettingsStorages",
        manager: "ХранилищаНастроекМенеджер",
        kind: MdKind::SettingsStorage,
    },
    Spec {
        ru: "WSСсылки",
        en: "WSReferences",
        manager: "WSСсылкиМенеджер",
        kind: MdKind::WSReference,
    },
    Spec {
        ru: "СервисыИнтеграции",
        en: "IntegrationServices",
        manager: "СервисыИнтеграцииМенеджер",
        kind: MdKind::IntegrationService,
    },
    Spec {
        ru: "ВнешниеИсточникиДанных",
        en: "ExternalDataSources",
        manager: "ВнешниеИсточникиДанныхМенеджер",
        kind: MdKind::ExternalDataSource,
    },
];

const OBJECT_BASE: u32 = 1 << 20;

pub(crate) fn register(app: &Arc<Application>, reg: &mut Registry) {
    let catalog = catalog();
    for spec in &SPECS {
        let (Some(ty), Some(_)) = (
            catalog.find_type(spec.manager),
            catalog.find_global_property(spec.ru),
        ) else {
            continue;
        };
        let app = app.clone();
        reg.global_prop(
            spec.ru,
            spec.en,
            Arc::new(move |_| {
                Ok(Value::Object(Rc::new(KindManager {
                    app: app.clone(),
                    spec,
                    ty,
                })))
            }),
        );
    }
}

struct KindManager {
    app: Arc<Application>,
    spec: &'static Spec,
    ty: TypeIdx,
}

impl KindManager {
    fn names(&self) -> impl Iterator<Item = &str> + '_ {
        self.app
            .model()
            .config
            .objects
            .iter()
            .filter(|o| o.kind == self.spec.kind)
            .map(|o| o.name.as_str())
    }

    fn find(&self, name: &str) -> Option<(usize, &str)> {
        let folded = fold_name(name);
        self.names()
            .enumerate()
            .find(|(_, n)| fold_name(n) == folded)
    }

    fn named(&self, m: MemberId) -> Option<&str> {
        let i = m.0.checked_sub(OBJECT_BASE)?;
        self.names().nth(usize::try_from(i).ok()?)
    }

    fn refusal(&self, member: &str) -> HostError {
        HostError::not_implemented(self.spec.manager, member)
    }

    fn collection(&self) -> (bool, bool) {
        catalog()
            .type_info(self.ty)
            .collection
            .as_ref()
            .map_or((false, false), |c| (c.for_each, c.index_access))
    }
}

impl HostObject for KindManager {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.ty)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<bsl_prim::Str>> {
        Ok(Some(crate::managers::kind_manager_string(
            ex,
            HostObject::type_id(self),
        )))
    }
    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        match self.named(m) {
            Some(name) => Err(self.refusal(name).into()),
            None => Err(internal("unknown member of a metadata kind manager").into()),
        }
    }
    fn set(&self, _: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        Err(HostError::key(bsl_stdlib::bind::msg::PROP_NOT_WRITABLE)
            .with_arg(self.named(m).unwrap_or_default())
            .raw()
            .into())
    }
    fn call(&self, _: &mut Execution, m: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        match member_name(catalog(), self.ty, m) {
            Some((true, ru, _)) => Err(self.refusal(ru).into()),
            _ => Err(internal("unknown method of a metadata kind manager").into()),
        }
    }
    fn find_member(&self, name: &str) -> Option<MemberId> {
        let (i, _) = self.find(name)?;
        Some(MemberId(OBJECT_BASE.checked_add(u32::try_from(i).ok()?)?))
    }
    fn member_kind(&self, m: MemberId) -> Option<MemberKind> {
        self.named(m).map(|_| MemberKind::Property {
            readable: true,
            writable: false,
        })
    }
    fn index_get(&self, _: &mut Execution, key: &Value) -> CallResult<Value> {
        if !self.collection().1 {
            return Err(HostError::key("bsl:IDS_RTE_NOTINDEXED").raw().into());
        }
        let Value::String(s) = key else {
            return Err(self.refusal("[ключ не строка]").into());
        };
        let name = s.to_string_lossy();
        match self.find(&name) {
            Some((_, configured)) => Err(self.refusal(configured).into()),
            None => Err(no_field(&name).into()),
        }
    }
    fn iter_len(&self, _: &mut Execution) -> Option<usize> {
        self.collection().0.then(|| self.names().count())
    }
    fn iter_get(&self, _: &mut Execution, i: usize) -> CallResult<Value> {
        match self.names().nth(i) {
            Some(name) => Err(self.refusal(name).into()),
            None => Err(HostError::key(bsl_stdlib::bind::msg::INDEX_OFFBOUND)
                .raw()
                .into()),
        }
    }
    fn boost_hash(&self) -> Option<i32> {
        Some(0x6b6d_0000 | i32::from(self.ty.0))
    }
    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        Some(
            other
                .as_any()
                .downcast_ref::<KindManager>()
                .is_some_and(|o| Arc::ptr_eq(&o.app, &self.app) && o.ty == self.ty),
        )
    }
}
