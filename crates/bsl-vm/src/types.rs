use crate::registry::AppTypeInfo;
use bsl_prim::fold_name;
use platform_catalog::{Catalog, Member, MethodInfo, TypeIdx, TypeKind, messages::Lang};
use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct TypeId(pub u32);

const PRIMITIVE_IDS: [&str; 7] = [
    "Number",
    "String",
    "Date",
    "Boolean",
    "Undefined",
    "Null",
    "Type",
];
const PRIMITIVE_COUNT: u32 = PRIMITIVE_IDS.len() as u32;
const CATALOG_BASE: u32 = 1000;
const APP_BASE: u32 = 100_000;

impl TypeId {
    pub const NUMBER: TypeId = TypeId(0);
    pub const STRING: TypeId = TypeId(1);
    pub const DATE: TypeId = TypeId(2);
    pub const BOOL: TypeId = TypeId(3);
    pub const UNDEFINED: TypeId = TypeId(4);
    pub const NULL: TypeId = TypeId(5);
    pub const TYPE: TypeId = TypeId(6);

    pub fn of_catalog(t: TypeIdx) -> TypeId {
        let i = u32::from(t.0);
        if i < PRIMITIVE_COUNT {
            TypeId(i)
        } else {
            TypeId(CATALOG_BASE + i)
        }
    }

    #[allow(
        clippy::expect_used,
        reason = "more applied types than the TypeId range is out of scope"
    )]
    pub(crate) fn app(n: usize) -> TypeId {
        let id = u32::try_from(n).ok().and_then(|n| n.checked_add(APP_BASE));
        TypeId(id.expect("applied type count exceeds the TypeId range"))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct MemberId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemberKind {
    Property {
        readable: bool,
        writable: bool,
    },
    Method {
        is_function: bool,
        min_args: u16,
        max_args: Option<u16>,
    },
}

pub struct TypeRegistry {
    catalog: &'static Catalog,
    app: Vec<AppTypeInfo>,
    app_names: HashMap<String, TypeId>,
}

enum Slot<'a> {
    Catalog(TypeIdx),
    App(&'a AppTypeInfo),
}

impl TypeRegistry {
    pub(crate) fn new(catalog: &'static Catalog, app: Vec<AppTypeInfo>) -> TypeRegistry {
        for (i, id) in PRIMITIVE_IDS.iter().enumerate() {
            let ok = catalog
                .types()
                .get(i)
                .is_some_and(|t| t.kind == TypeKind::Primitive && t.id == *id);
            assert!(ok, "catalog type {i} must be the primitive type {id}");
        }
        let mut app_names = HashMap::new();
        for (n, info) in app.iter().enumerate() {
            let t = TypeId::app(n);
            app_names.entry(fold_name(&info.name_ru)).or_insert(t);
            app_names.entry(fold_name(&info.name_en)).or_insert(t);
        }
        TypeRegistry {
            catalog,
            app,
            app_names,
        }
    }

    fn slot(&self, t: TypeId) -> Option<Slot<'_>> {
        let i = t.0;
        if i < PRIMITIVE_COUNT {
            return Some(Slot::Catalog(TypeIdx(u16::try_from(i).ok()?)));
        }
        if i >= APP_BASE {
            let n = usize::try_from(i - APP_BASE).ok()?;
            return self.app.get(n).map(Slot::App);
        }
        if i < CATALOG_BASE + PRIMITIVE_COUNT {
            return None;
        }
        let idx = u16::try_from(i - CATALOG_BASE).ok()?;
        (usize::from(idx) < self.catalog.types().len()).then_some(Slot::Catalog(TypeIdx(idx)))
    }

    pub fn by_name(&self, name: &str) -> Option<TypeId> {
        if let Some(t) = self.app_names.get(&fold_name(name)) {
            return Some(*t);
        }
        self.catalog.find_type(name).map(TypeId::of_catalog)
    }

    pub fn name(&self, t: TypeId, lang: Lang) -> &str {
        match self.slot(t) {
            Some(Slot::Catalog(idx)) => {
                let info = self.catalog.type_info(idx);
                match lang {
                    Lang::Ru => &info.name_ru,
                    Lang::En => &info.name_en,
                }
            }
            Some(Slot::App(info)) => match lang {
                Lang::Ru => &info.name_ru,
                Lang::En => &info.name_en,
            },
            None => "",
        }
    }

    pub fn presentation(&self, t: TypeId, lang: Lang) -> &str {
        match self.slot(t) {
            Some(Slot::Catalog(idx)) => {
                let info = self.catalog.type_info(idx);
                let p = match lang {
                    Lang::Ru => info
                        .value_type_presentation_ru
                        .as_ref()
                        .or(info.presentation_ru.as_ref()),
                    Lang::En => info
                        .value_type_presentation_en
                        .as_ref()
                        .or(info.presentation_en.as_ref()),
                };
                p.map(String::as_str).unwrap_or_else(|| self.name(t, lang))
            }
            Some(Slot::App(info)) => match lang {
                Lang::Ru => &info.presentation_ru,
                Lang::En => &info.presentation_en,
            },
            None => "",
        }
    }

    pub fn catalog_type(&self, t: TypeId) -> Option<TypeIdx> {
        match self.slot(t)? {
            Slot::Catalog(idx) => Some(idx),
            Slot::App(info) => info
                .template
                .filter(|t| usize::from(t.0) < self.catalog.types().len()),
        }
    }

    pub fn app_type(&self, t: TypeId) -> Option<&AppTypeInfo> {
        match self.slot(t)? {
            Slot::App(info) => Some(info),
            Slot::Catalog(_) => None,
        }
    }

    pub fn member(&self, t: TypeId, name: &str) -> Option<(MemberId, MemberKind)> {
        let idx = self.catalog_type(t)?;
        let info = self.catalog.types().get(usize::from(idx.0))?;
        match self.catalog.find_member(idx, name)? {
            Member::Property(p) => {
                let i = info.properties.iter().position(|q| std::ptr::eq(q, p))?;
                let kind = MemberKind::Property {
                    readable: p.readable,
                    writable: p.writable,
                };
                Some((MemberId(u32::try_from(i).ok()?), kind))
            }
            Member::Method(m) => {
                let j = info.methods.iter().position(|q| std::ptr::eq(q, m))?;
                let id = u32::try_from(info.properties.len() + j).ok()?;
                Some((MemberId(id), method_kind(m)))
            }
        }
    }

    pub fn member_name(&self, t: TypeId, name: &str, method: bool, lang: Lang) -> Option<&str> {
        let idx = self.catalog_type(t)?;
        let (ru, en) = if method {
            let m = self.catalog.find_method(idx, name)?;
            (&m.name_ru, &m.name_en)
        } else {
            match self.catalog.find_member(idx, name)? {
                Member::Property(p) => (&p.name_ru, &p.name_en),
                Member::Method(m) => (&m.name_ru, &m.name_en),
            }
        };
        Some(if lang == Lang::Ru { ru } else { en })
    }

    pub(crate) fn method(&self, t: TypeId, name: &str) -> Option<(MemberId, MemberKind)> {
        let idx = self.catalog_type(t)?;
        let info = self.catalog.types().get(usize::from(idx.0))?;
        let m = self.catalog.find_method(idx, name)?;
        let j = info.methods.iter().position(|q| std::ptr::eq(q, m))?;
        let id = u32::try_from(info.properties.len() + j).ok()?;
        Some((MemberId(id), method_kind(m)))
    }
}

fn method_kind(m: &MethodInfo) -> MemberKind {
    let clamp = |n: usize| u16::try_from(n).unwrap_or(u16::MAX);
    let mut min_args: Option<u16> = None;
    let mut max_args: Option<u16> = Some(0);
    for v in &m.variants {
        let required = v
            .params
            .iter()
            .rposition(|p| !p.optional && !p.variadic)
            .map_or(0, |i| i + 1);
        min_args = Some(min_args.map_or(clamp(required), |n| n.min(clamp(required))));
        max_args = match max_args {
            Some(n) if !v.params.iter().any(|p| p.variadic) => Some(n.max(clamp(v.params.len()))),
            _ => None,
        };
    }
    if m.variants.is_empty() {
        max_args = None;
    }
    MemberKind::Method {
        is_function: m.returns_value,
        min_args: min_args.unwrap_or(0),
        max_args,
    }
}
