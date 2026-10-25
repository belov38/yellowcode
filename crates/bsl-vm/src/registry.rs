use crate::error::Exception;
use crate::object::CallResult;
use crate::place::Arg;
use crate::runtime::Execution;
use crate::shared::SharedValue;
use crate::types::TypeId;
use crate::value::Value;
use bsl_prim::{Str, fold_name};
use platform_catalog::{Catalog, TypeIdx, XdtoName};
use std::collections::HashMap;
use std::sync::Arc;

pub type GlobalFn = Arc<dyn Fn(&mut Execution, &mut [Arg]) -> CallResult<Value> + Send + Sync>;
pub type GlobalProp = Arc<dyn Fn(&mut Execution) -> CallResult<Value> + Send + Sync>;
pub type CtorFn = Arc<dyn Fn(&mut Execution, &mut [Arg]) -> CallResult<Value> + Send + Sync>;
pub type Presenter = Arc<dyn Fn(&mut Execution, &Value) -> Result<Str, Exception> + Send + Sync>;

pub struct TypeImpl {
    pub ctor: Option<CtorFn>,
}

pub struct AppTypeInfo {
    pub name_ru: String,
    pub name_en: String,
    pub presentation_ru: String,
    pub presentation_en: String,
    pub guid: [u8; 16],
    pub xdto: Option<XdtoName>,
    pub template: Option<TypeIdx>,
    pub default_value: Option<SharedValue>,
}

pub struct Registry {
    pub(crate) catalog: &'static Catalog,
    global_fns: HashMap<String, GlobalFn>,
    global_props: HashMap<String, GlobalProp>,
    type_impls: HashMap<TypeIdx, TypeImpl>,
    fallback_impls: HashMap<TypeIdx, TypeImpl>,
    pub(crate) system_enums: HashMap<TypeIdx, GlobalProp>,
    pub(crate) app_types: Vec<AppTypeInfo>,
    pub(crate) presenter: Option<Presenter>,
}

fn put<T: Clone>(map: &mut HashMap<String, T>, name_ru: &str, name_en: &str, v: T, what: &str) {
    let ru = fold_name(name_ru);
    let en = fold_name(name_en);
    debug_assert!(
        !map.contains_key(&ru) && !map.contains_key(&en),
        "{what} {name_ru} is registered twice"
    );
    if en != ru {
        map.insert(en, v.clone());
    }
    map.insert(ru, v);
}

impl Registry {
    pub fn new(catalog: &'static Catalog) -> Registry {
        Registry {
            catalog,
            global_fns: HashMap::new(),
            global_props: HashMap::new(),
            type_impls: HashMap::new(),
            fallback_impls: HashMap::new(),
            system_enums: HashMap::new(),
            app_types: Vec::new(),
            presenter: None,
        }
    }

    pub fn global_fn(&mut self, name_ru: &str, name_en: &str, f: GlobalFn) {
        debug_assert!(
            self.catalog.find_global_method(name_ru).is_some(),
            "{name_ru} is not a global method of the catalog"
        );
        self.debug_same_global(name_ru, name_en, true);
        put(&mut self.global_fns, name_ru, name_en, f, "global function");
    }

    pub fn global_fn_names(&self) -> Vec<String> {
        self.global_fns.keys().cloned().collect()
    }

    pub(crate) fn find_global_fn(&self, name: &str) -> Option<GlobalFn> {
        self.global_fns.get(&fold_name(name)).cloned()
    }

    pub fn global_prop(&mut self, name_ru: &str, name_en: &str, f: GlobalProp) {
        debug_assert!(
            self.catalog.find_global_property(name_ru).is_some(),
            "{name_ru} is not a global property of the catalog"
        );
        self.debug_same_global(name_ru, name_en, false);
        put(
            &mut self.global_props,
            name_ru,
            name_en,
            f,
            "global property",
        );
    }

    pub(crate) fn find_global_prop(&self, name: &str) -> Option<GlobalProp> {
        self.global_props.get(&fold_name(name)).cloned()
    }

    pub fn type_impl(&mut self, t: TypeIdx, imp: TypeImpl) {
        let old = self.type_impls.insert(t, imp);
        debug_assert!(old.is_none(), "type {} is implemented twice", t.0);
    }

    pub fn fallback_type_impl(&mut self, t: TypeIdx, imp: TypeImpl) {
        self.fallback_impls.insert(t, imp);
    }

    pub(crate) fn has_type_impl(&self, t: TypeIdx) -> bool {
        self.type_impls.contains_key(&t)
    }

    pub(crate) fn find_ctor(&self, t: TypeIdx) -> Option<CtorFn> {
        self.type_impls
            .get(&t)
            .or_else(|| self.fallback_impls.get(&t))?
            .ctor
            .clone()
    }

    fn debug_same_global(&self, name_ru: &str, name_en: &str, method: bool) {
        if cfg!(debug_assertions) {
            let same = if method {
                let (a, b) = (
                    self.catalog.find_global_method(name_ru),
                    self.catalog.find_global_method(name_en),
                );
                matches!((a, b), (Some(a), Some(b)) if std::ptr::eq(a, b))
            } else {
                let (a, b) = (
                    self.catalog.find_global_property(name_ru),
                    self.catalog.find_global_property(name_en),
                );
                matches!((a, b), (Some(a), Some(b)) if std::ptr::eq(a, b))
            };
            assert!(
                same,
                "{name_ru} and {name_en} are not the same catalog entry"
            );
        }
    }

    pub fn register_app_type(&mut self, info: AppTypeInfo) -> TypeId {
        debug_assert!(
            info.template
                .is_none_or(|t| usize::from(t.0) < self.catalog.types().len()),
            "the template of applied type {} is outside the catalog",
            info.name_ru
        );
        debug_assert!(
            !self
                .app_types
                .iter()
                .any(|t| fold_name(&t.name_ru) == fold_name(&info.name_ru)
                    || fold_name(&t.name_en) == fold_name(&info.name_en)),
            "applied type {} is registered twice",
            info.name_ru
        );
        self.app_types.push(info);
        TypeId::app(self.app_types.len() - 1)
    }

    pub fn set_presenter(&mut self, p: Presenter) {
        self.presenter = Some(p);
    }
}
