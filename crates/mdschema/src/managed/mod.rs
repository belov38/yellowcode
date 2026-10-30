use crate::{forms_upgrade::*, rights::RightsTables, templates::TemplateTables, *};
use memory_budget::{
    BudgetError, BudgetedVec, Category, Charged, Grant, Lock, MemoryBudget, Reservation,
};
use std::sync::{
    OnceLock,
    atomic::{AtomicBool, Ordering},
};

#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("schema data: {0}")]
pub struct SchemaDataError(pub &'static str);
#[derive(Debug, thiserror::Error)]
pub enum ManagedSchemaError {
    #[error("schema preparation cancelled")]
    Cancelled,
    #[error("{0}")]
    Memory(
        #[from]
        #[source]
        BudgetError,
    ),
    #[error("{0}")]
    Resource(
        #[from]
        #[source]
        ResourceError,
    ),
    #[error("{0}")]
    Data(
        #[from]
        #[source]
        SchemaDataError,
    ),
}
fn check_cancelled(is_cancelled: &dyn Fn() -> bool) -> Result<(), ManagedSchemaError> {
    if is_cancelled() {
        Err(ManagedSchemaError::Cancelled)
    } else {
        Ok(())
    }
}
fn data(message: &'static str) -> ManagedSchemaError {
    SchemaDataError(message).into()
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaGroup {
    Schema,
    Rights,
    Templates,
    StdAttrs,
    Upgrade,
    LearnedKinds,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Payload,
    IndexAdmitted,
    Index,
    Publish,
}

#[derive(Default)]
struct Plan(u64);
impl Plan {
    fn vector<T>(&mut self, n: usize) -> Result<(), BudgetError> {
        if n == 0 {
            return Ok(());
        }
        let bytes = std::alloc::Layout::array::<T>(n)
            .map_err(|_| BudgetError::Overflow)?
            .size();
        let bytes = u64::try_from(bytes)
            .map_err(|_| BudgetError::Overflow)?
            .checked_add(std::mem::size_of::<BudgetedVec<T>>() as u64)
            .ok_or(BudgetError::Overflow)?;
        self.0 = self.0.checked_add(bytes).ok_or(BudgetError::Overflow)?;
        Ok(())
    }
}
trait Backing {
    fn backing(&self) -> Result<u64, BudgetError>;
}
fn sum(a: u64, b: u64) -> Result<u64, BudgetError> {
    a.checked_add(b).ok_or(BudgetError::Overflow)
}
impl<T: Backing> Backing for Vec<T> {
    fn backing(&self) -> Result<u64, BudgetError> {
        let mut p = Plan::default();
        p.vector::<T>(self.capacity())?;
        self.iter().try_fold(p.0, |n, v| sum(n, v.backing()?))
    }
}
impl Backing for String {
    fn backing(&self) -> Result<u64, BudgetError> {
        let mut p = Plan::default();
        p.vector::<u8>(self.capacity())?;
        Ok(p.0)
    }
}
impl<T: Backing> Backing for Option<T> {
    fn backing(&self) -> Result<u64, BudgetError> {
        self.as_ref().map_or(Ok(0), Backing::backing)
    }
}
impl<A: Backing, B: Backing> Backing for (A, B) {
    fn backing(&self) -> Result<u64, BudgetError> {
        sum(self.0.backing()?, self.1.backing()?)
    }
}
impl<K: Backing, V: Backing> Backing for FactMap<K, V> {
    fn backing(&self) -> Result<u64, BudgetError> {
        self.rows.backing()
    }
}
impl Backing for FactJson {
    fn backing(&self) -> Result<u64, BudgetError> {
        match self {
            Self::Embedded(_) => Ok(0),
            Self::Legacy(_) => Err(BudgetError::Overflow),
        }
    }
}
macro_rules! scalar {($($ty:ty),*)=>{$(impl Backing for $ty{fn backing(&self)->Result<u64,BudgetError>{Ok(0)}} impl Shape for $ty{type Owned=Self;fn plan(&self,_:&mut Plan)->Result<(),BudgetError>{Ok(())}fn build(&self,_:&mut Builder<'_>)->Result<Self,ManagedSchemaError>{Ok(*self)}})*}}
scalar!(usize, u32, i32, i64, bool, Codec);
impl Backing for &'static SchemaResource {
    fn backing(&self) -> Result<u64, BudgetError> {
        Ok(0)
    }
}
struct Builder<'a> {
    grant: Option<&'a mut Grant>,
    resource: &'static SchemaResource,
}
impl Builder<'_> {
    fn vector<T>(&mut self, n: usize) -> Result<Vec<T>, ManagedSchemaError> {
        if n == 0 {
            return Ok(Vec::new());
        }
        let value = if let Some(g) = self.grant.as_mut() {
            g.try_vec_unowned(n)?
        } else {
            let mut v = Vec::new();
            v.try_reserve_exact(n)
                .map_err(|_| BudgetError::AllocationFailed)?;
            v
        };
        #[cfg(test)]
        build_panic_point();
        Ok(value)
    }
}
#[cfg(test)]
thread_local! {static BUILD_PANIC_AT:std::cell::Cell<usize>=const{std::cell::Cell::new(0)};}

#[cfg(test)]
thread_local! {static ADOPTION_PANIC:std::cell::Cell<bool>=const{std::cell::Cell::new(false)};}
trait Shape {
    type Owned: Backing;
    fn plan(&self, p: &mut Plan) -> Result<(), BudgetError>;
    fn build(&self, b: &mut Builder<'_>) -> Result<Self::Owned, ManagedSchemaError>;
}
impl Shape for &'static str {
    type Owned = String;
    fn plan(&self, p: &mut Plan) -> Result<(), BudgetError> {
        p.vector::<u8>(self.len())
    }
    fn build(&self, b: &mut Builder<'_>) -> Result<String, ManagedSchemaError> {
        let mut bytes = b.vector(self.len())?;
        bytes.extend_from_slice(self.as_bytes());
        match String::from_utf8(bytes) {
            Ok(v) => Ok(v),
            Err(e) => {
                drop(e);
                Err(data("invalid generated UTF8"))
            }
        }
    }
}
impl<D: Shape> Shape for &'static [D] {
    type Owned = Vec<D::Owned>;
    fn plan(&self, p: &mut Plan) -> Result<(), BudgetError> {
        p.vector::<D::Owned>(self.len())?;
        for d in *self {
            d.plan(p)?
        }
        Ok(())
    }
    fn build(&self, b: &mut Builder<'_>) -> Result<Self::Owned, ManagedSchemaError> {
        let mut out = b.vector(self.len())?;
        for d in *self {
            out.push(d.build(b)?)
        }
        Ok(out)
    }
}
impl<D: Shape> Shape for Option<D> {
    type Owned = Option<D::Owned>;
    fn plan(&self, p: &mut Plan) -> Result<(), BudgetError> {
        if let Some(d) = self {
            d.plan(p)?
        }
        Ok(())
    }
    fn build(&self, b: &mut Builder<'_>) -> Result<Self::Owned, ManagedSchemaError> {
        self.as_ref().map(|d| d.build(b)).transpose()
    }
}
impl<A: Shape, B: Shape> Shape for (A, B) {
    type Owned = (A::Owned, B::Owned);
    fn plan(&self, p: &mut Plan) -> Result<(), BudgetError> {
        self.0.plan(p)?;
        self.1.plan(p)
    }
    fn build(&self, b: &mut Builder<'_>) -> Result<Self::Owned, ManagedSchemaError> {
        Ok((self.0.build(b)?, self.1.build(b)?))
    }
}
struct MapDesc<K: 'static, V: 'static>(&'static [(K, V)]);
impl<K: Shape, V: Shape> Shape for MapDesc<K, V>
where
    K::Owned: Ord,
{
    type Owned = FactMap<K::Owned, V::Owned>;
    fn plan(&self, p: &mut Plan) -> Result<(), BudgetError> {
        self.0.plan(p)
    }
    fn build(&self, b: &mut Builder<'_>) -> Result<Self::Owned, ManagedSchemaError> {
        Ok(FactMap::from_unique_rows(self.0.build(b)?))
    }
}
struct ResourceTag;
impl Shape for ResourceTag {
    type Owned = &'static SchemaResource;
    fn plan(&self, _: &mut Plan) -> Result<(), BudgetError> {
        Ok(())
    }
    fn build(&self, b: &mut Builder<'_>) -> Result<Self::Owned, ManagedSchemaError> {
        Ok(b.resource)
    }
}
enum Step {
    Key(&'static str),
    Index(usize),
}
struct JsonRef {
    file: &'static str,
    path: &'static [Step],
}
impl Shape for JsonRef {
    type Owned = FactJson;
    fn plan(&self, _: &mut Plan) -> Result<(), BudgetError> {
        Ok(())
    }
    fn build(&self, _: &mut Builder<'_>) -> Result<FactJson, ManagedSchemaError> {
        let mut v = static_schema()
            .table(self.file)
            .ok_or_else(|| data("missing generated JSON table"))?;
        for step in self.path {
            v = match step {
                Step::Key(k) => v.get(k),
                Step::Index(i) => v.at(*i),
            }
            .ok_or_else(|| data("missing generated JSON path"))?
        }
        Ok(FactJson::Embedded(v))
    }
}
enum DConfigPath {
    Part2,
    Body(&'static [usize]),
}
impl Shape for DConfigPath {
    type Owned = ConfigPath;
    fn plan(&self, p: &mut Plan) -> Result<(), BudgetError> {
        if let Self::Body(v) = self {
            v.plan(p)?
        }
        Ok(())
    }
    fn build(&self, b: &mut Builder<'_>) -> Result<ConfigPath, ManagedSchemaError> {
        Ok(match self {
            Self::Part2 => ConfigPath::Part2,
            Self::Body(v) => ConfigPath::Body(v.build(b)?),
        })
    }
}
impl Backing for ConfigPath {
    fn backing(&self) -> Result<u64, BudgetError> {
        match self {
            Self::Part2 => Ok(0),
            Self::Body(v) => v.backing(),
        }
    }
}
enum DDomain {
    Scalar(fn() -> Domain),
    Set(&'static [i64]),
}
impl Shape for DDomain {
    type Owned = Domain;
    fn plan(&self, p: &mut Plan) -> Result<(), BudgetError> {
        if let Self::Set(v) = self {
            v.plan(p)?
        }
        Ok(())
    }
    fn build(&self, b: &mut Builder<'_>) -> Result<Domain, ManagedSchemaError> {
        Ok(match self {
            Self::Scalar(value) => value(),
            Self::Set(v) => Domain::Set(v.build(b)?),
        })
    }
}
impl Backing for Domain {
    fn backing(&self) -> Result<u64, BudgetError> {
        match self {
            Self::Set(v) => v.backing(),
            _ => Ok(0),
        }
    }
}
macro_rules! descriptor{($desc:ident => $owned:ident {$($field:ident:$ty:ty),*})=>{
 pub(super) struct $desc{$($field:$ty),*}
 impl Shape for $desc{type Owned=$owned;fn plan(&self,p:&mut Plan)->Result<(),BudgetError>{$ (self.$field.plan(p)?;)*Ok(())}fn build(&self,b:&mut Builder<'_>)->Result<$owned,ManagedSchemaError>{Ok($owned{$($field:self.$field.build(b)?),*})}}
 impl Backing for $owned{fn backing(&self)->Result<u64,BudgetError>{let n=0u64;$(let n=sum(n,self.$field.backing()?)?;)*Ok(n)}}
}}
mod descriptors;
use descriptors::*;
type DKindMap = MapDesc<&'static str, DKindSpec>;

#[derive(Debug)]
pub(crate) enum Built<T> {
    Legacy(T),
    Managed(Charged<T>),
}
impl<T> Built<T> {
    fn value(&self) -> &T {
        match self {
            Self::Legacy(v) => v,
            Self::Managed(v) => v,
        }
    }
}
#[cfg(any(test, feature = "testing"))]
impl<T> Built<T> {
    fn bytes(&self) -> u64 {
        match self {
            Self::Legacy(_) => 0,
            Self::Managed(v) => v.reservation().bytes(),
        }
    }
}
fn build_shape<D: Shape>(
    desc: &D,
    resource: &'static SchemaResource,
    root: Option<&MemoryBudget>,
) -> Result<Built<D::Owned>, ManagedSchemaError> {
    if let Some(root) = root {
        let mut p = Plan::default();
        desc.plan(&mut p)?;
        Ok(Built::Managed(root.try_prepaid(
            Category::Metadata,
            p.0,
            |grant| {
                desc.build(&mut Builder {
                    grant: Some(grant),
                    resource,
                })
            },
        )?))
    } else {
        Ok(Built::Legacy(desc.build(&mut Builder {
            grant: None,
            resource,
        })?))
    }
}
fn build_schema(
    resource: &'static SchemaResource,
    root: Option<&MemoryBudget>,
    mut hook: impl FnMut(Stage),
) -> Result<Built<Schema>, ManagedSchemaError> {
    let payload = build_shape(&SCHEMA, resource, root)?;
    hook(Stage::Payload);
    let s = payload.value();
    let mut plan = Plan::default();
    for n in [s.classes_top.len(), s.folders.len(), s.config_props.len()] {
        plan.vector::<usize>(n)?;
    }
    let index_build = |b: &mut Builder<'_>| -> Result<_, ManagedSchemaError> {
        let mut classes = b.vector::<usize>(s.classes_top.len())?;
        classes.extend(0..s.classes_top.len());
        crate::tables::sort_ordinals(&mut classes, &s.classes_top, |(k, _)| k);
        let mut folders = b.vector::<usize>(s.folders.len())?;
        folders.extend(0..s.folders.len());
        crate::tables::sort_ordinals(&mut folders, &s.folders, |(k, _)| k);
        let mut config = b.vector::<usize>(s.config_props.len())?;
        config.extend(0..s.config_props.len());
        crate::tables::sort_ordinals(&mut config, &s.config_props, |p| p.name.as_str());
        Ok((classes, (folders, config)))
    };
    let indexes = if let Some(root) = root {
        Built::Managed(root.try_prepaid(Category::Metadata, plan.0, |grant| {
            hook(Stage::IndexAdmitted);
            index_build(&mut Builder {
                grant: Some(grant),
                resource,
            })
        })?)
    } else {
        Built::Legacy(index_build(&mut Builder {
            grant: None,
            resource,
        })?)
    };
    hook(Stage::Index);
    type IndexedSchema = (Schema, (Vec<usize>, (Vec<usize>, Vec<usize>)));
    let assemble = |(mut s, (class_index, (folder_index, config_index))): IndexedSchema| {
        s.class_index = class_index;
        s.folder_index = folder_index;
        s.config_index = config_index;
        s
    };
    let value = match (payload, indexes) {
        (Built::Managed(p), Built::Managed(i)) => Built::Managed(p.try_zip(i)?.map(assemble)),
        (Built::Legacy(p), Built::Legacy(i)) => Built::Legacy(assemble((p, i))),
        _ => return Err(data("mixed schema owners")),
    };
    hook(Stage::Publish);
    Ok(value)
}
fn build_rights(
    r: &'static SchemaResource,
    b: Option<&MemoryBudget>,
) -> Result<Built<RightsTables>, ManagedSchemaError> {
    build_shape(&RIGHTS, r, b)
}
fn build_templates(
    r: &'static SchemaResource,
    b: Option<&MemoryBudget>,
) -> Result<Built<TemplateTables>, ManagedSchemaError> {
    build_shape(&TEMPLATES, r, b)
}
fn build_stdattrs(
    r: &'static SchemaResource,
    b: Option<&MemoryBudget>,
) -> Result<Built<Vec<forms::KindStdAttrs>>, ManagedSchemaError> {
    build_shape(&STDATTRS, r, b)
}
fn build_upgrade(
    r: &'static SchemaResource,
    b: Option<&MemoryBudget>,
) -> Result<Built<UpgradeTables>, ManagedSchemaError> {
    build_shape(&UPGRADE, r, b)
}

fn build_learned(
    r: &'static SchemaResource,
    b: Option<&MemoryBudget>,
) -> Result<Built<FactMap<String, KindSpec>>, ManagedSchemaError> {
    build_shape(&LEARNED_KINDS, r, b)
}

struct Claim<'a>(&'a AtomicBool);
impl<'a> Claim<'a> {
    fn take(flag: &'a AtomicBool) -> Self {
        while flag
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            #[cfg(test)]
            publication_tests::waiting(flag);
            #[cfg(test)]
            learned_tests::point(
                learned_tests::Point::Waiting,
                std::ptr::from_ref(flag) as usize,
            );
            std::thread::yield_now()
        }
        Self(flag)
    }
}
impl Drop for Claim<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release)
    }
}
struct Payload<T> {
    value: Built<T>,
    adopted: Lock<Option<Reservation>>,
}
pub(crate) struct Cell<T> {
    value: OnceLock<Payload<T>>,
    claimed: AtomicBool,
}
impl<T> Cell<T> {
    pub const fn new() -> Self {
        Self {
            value: OnceLock::new(),
            claimed: AtomicBool::new(false),
        }
    }
    pub(crate) fn legacy<E>(&self, build: impl FnOnce() -> Result<Built<T>, E>) -> Result<&T, E> {
        let _claim = Claim::take(&self.claimed);
        if let Some(payload) = self.value.get() {
            return Ok(payload.value.value());
        }
        let payload = Payload {
            value: build()?,
            adopted: Lock::new(None),
        };
        Ok(self.value.get_or_init(|| payload).value.value())
    }
    pub(crate) fn legacy_plain<E>(&self, build: impl FnOnce() -> Result<T, E>) -> Result<&T, E> {
        self.legacy(|| build().map(Built::Legacy))
    }
    fn prepare_checked(
        &self,
        root: &MemoryBudget,
        build: impl FnOnce() -> Result<Built<T>, ManagedSchemaError>,
        backing: impl FnOnce(&T) -> Result<u64, BudgetError>,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<&T, ManagedSchemaError> {
        let _claim = Claim::take(&self.claimed);
        #[cfg(feature = "testing")]
        testing::cell_event(
            std::ptr::from_ref(self) as usize,
            testing::LearnedEvent::AfterClaim,
        );
        check_cancelled(is_cancelled)?;
        if let Some(p) = self.value.get() {
            let needs_adoption = matches!(p.value, Built::Legacy(_)) && p.adopted.lock().is_none();
            if needs_adoption {
                let owner = root
                    .try_reserve(Category::Metadata, backing(p.value.value())?)
                    .map_err(BudgetError::from)?;
                #[cfg(test)]
                learned_tests::point(
                    learned_tests::Point::AdoptionAdmitted,
                    std::ptr::from_ref(self) as usize,
                );
                #[cfg(test)]
                adoption_panic_point();
                check_cancelled(is_cancelled)?;
                *p.adopted.lock() = Some(owner)
            }
            return Ok(p.value.value());
        }
        let payload = Payload {
            value: build()?,
            adopted: Lock::new(None),
        };
        #[cfg(test)]
        publication_tests::before_publication(std::ptr::from_ref(self) as usize);
        #[cfg(test)]
        learned_tests::point(
            learned_tests::Point::BeforePublication,
            std::ptr::from_ref(self) as usize,
        );
        #[cfg(feature = "testing")]
        testing::cell_event(
            std::ptr::from_ref(self) as usize,
            testing::LearnedEvent::BeforePublication,
        );
        check_cancelled(is_cancelled)?;
        let published = self.value.get_or_init(|| payload);
        #[cfg(test)]
        publication_tests::published(std::ptr::from_ref(self) as usize);
        #[cfg(test)]
        learned_tests::point(
            learned_tests::Point::Published,
            std::ptr::from_ref(self) as usize,
        );
        #[cfg(feature = "testing")]
        testing::cell_event(
            std::ptr::from_ref(self) as usize,
            testing::LearnedEvent::Published,
        );
        Ok(published.value.value())
    }
}
pub(crate) struct Binding {
    root: Lock<Option<MemoryBudget>>,
    claimed: AtomicBool,
}
impl Binding {
    pub const fn new() -> Self {
        Self {
            root: Lock::new(None),
            claimed: AtomicBool::new(false),
        }
    }
    fn owner(&self) -> Option<MemoryBudget> {
        self.root.lock().clone()
    }
    fn prepare<'a, T>(
        &self,
        passed: &MemoryBudget,
        cell: &'a Cell<T>,
        build: impl FnOnce(&MemoryBudget) -> Result<Built<T>, ManagedSchemaError>,
        backing: impl FnOnce(&T) -> Result<u64, BudgetError>,
    ) -> Result<&'a T, ManagedSchemaError> {
        self.prepare_checked(passed, cell, build, backing, &|| false)
    }
    fn prepare_checked<'a, T>(
        &self,
        passed: &MemoryBudget,
        cell: &'a Cell<T>,
        build: impl FnOnce(&MemoryBudget) -> Result<Built<T>, ManagedSchemaError>,
        backing: impl FnOnce(&T) -> Result<u64, BudgetError>,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<&'a T, ManagedSchemaError> {
        check_cancelled(is_cancelled)?;
        let _claim = Claim::take(&self.claimed);
        #[cfg(test)]
        publication_tests::claimed(&self.claimed);
        #[cfg(test)]
        learned_tests::point(
            learned_tests::Point::Claimed,
            std::ptr::from_ref(&self.claimed) as usize,
        );
        check_cancelled(is_cancelled)?;
        let root = self.owner().unwrap_or_else(|| passed.root_budget());
        let value = cell.prepare_checked(&root, || build(&root), backing, is_cancelled)?;
        let unbound = self.root.lock().is_none();
        if unbound {
            *self.root.lock() = Some(root)
        }
        #[cfg(feature = "testing")]
        testing::cell_event(
            std::ptr::from_ref(cell) as usize,
            testing::LearnedEvent::Bound,
        );
        check_cancelled(is_cancelled)?;
        Ok(value)
    }
}
impl SchemaResource {
    pub fn prepare_with_memory(
        &'static self,
        group: SchemaGroup,
        budget: &MemoryBudget,
    ) -> Result<MemoryBudget, ManagedSchemaError> {
        self.require_embedded()?;
        match group {
            SchemaGroup::Schema => {
                self.binding.prepare(
                    budget,
                    &self.schema,
                    |root| build_schema(self, Some(root), |_| {}),
                    Schema::backing,
                )?;
            }
            SchemaGroup::Rights => {
                self.binding.prepare(
                    budget,
                    &self.rights,
                    |root| build_rights(self, Some(root)),
                    RightsTables::backing,
                )?;
            }
            SchemaGroup::Templates => {
                self.binding.prepare(
                    budget,
                    &self.templates,
                    |root| build_templates(self, Some(root)),
                    TemplateTables::backing,
                )?;
            }
            SchemaGroup::StdAttrs => {
                self.binding.prepare(
                    budget,
                    &self.stdattrs,
                    |root| build_stdattrs(self, Some(root)),
                    Vec::backing,
                )?;
            }
            SchemaGroup::LearnedKinds => {
                return self.prepare_learned_kinds_with_memory_checked(budget, &|| false);
            }
            SchemaGroup::Upgrade => {
                self.binding.prepare(
                    budget,
                    &self.upgrade,
                    |root| build_upgrade(self, Some(root)),
                    UpgradeTables::backing,
                )?;
            }
        }
        self.binding
            .owner()
            .ok_or_else(|| data("missing resource owner"))
    }
    pub fn prepare_learned_kinds_with_memory_checked(
        &'static self,
        budget: &MemoryBudget,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<MemoryBudget, ManagedSchemaError> {
        self.require_embedded()?;
        #[cfg(feature = "testing")]
        testing::cell_event(
            std::ptr::from_ref(&self.learned) as usize,
            testing::LearnedEvent::Entry,
        );
        self.binding.prepare_checked(
            budget,
            &self.learned,
            |root| build_learned(self, Some(root)),
            FactMap::backing,
            is_cancelled,
        )?;
        self.binding
            .owner()
            .ok_or_else(|| data("missing resource owner"))
    }
    pub fn learned_kinds_with_memory(
        &'static self,
        budget: &MemoryBudget,
    ) -> Result<(&'static FactMap<String, KindSpec>, MemoryBudget), ManagedSchemaError> {
        let root = self.prepare_with_memory(SchemaGroup::LearnedKinds, budget)?;
        let value = self
            .learned
            .legacy(|| Err(data("prepared learned kinds missing")))?;
        Ok((value, root))
    }
    pub fn schema_with_memory(
        &'static self,
        b: &MemoryBudget,
    ) -> Result<(&'static Schema, MemoryBudget), ManagedSchemaError> {
        let root = self.prepare_with_memory(SchemaGroup::Schema, b)?;
        let value = self
            .schema
            .legacy(|| Err(data("prepared schema missing")))?;
        Ok((value, root))
    }

    pub fn rights_tables_with_memory(
        &'static self,
        b: &MemoryBudget,
    ) -> Result<(&'static RightsTables, MemoryBudget), ManagedSchemaError> {
        let root = self.prepare_with_memory(SchemaGroup::Rights, b)?;
        Ok((
            self.rights
                .legacy(|| Err(data("prepared rights missing")))?,
            root,
        ))
    }
    pub fn template_tables_with_memory(
        &'static self,
        b: &MemoryBudget,
    ) -> Result<(&'static TemplateTables, MemoryBudget), ManagedSchemaError> {
        let root = self.prepare_with_memory(SchemaGroup::Templates, b)?;
        Ok((
            self.templates
                .legacy(|| Err(data("prepared templates missing")))?,
            root,
        ))
    }
    pub fn std_attrs_with_memory(
        &'static self,
        b: &MemoryBudget,
    ) -> Result<(&'static [forms::KindStdAttrs], MemoryBudget), ManagedSchemaError> {
        let root = self.prepare_with_memory(SchemaGroup::StdAttrs, b)?;
        Ok((
            self.stdattrs
                .legacy(|| Err(data("prepared stdattrs missing")))?
                .as_slice(),
            root,
        ))
    }
    pub fn upgrade_tables_with_memory(
        &'static self,
        b: &MemoryBudget,
    ) -> Result<(&'static UpgradeTables, MemoryBudget), ManagedSchemaError> {
        let root = self.prepare_with_memory(SchemaGroup::Upgrade, b)?;
        Ok((
            self.upgrade
                .legacy(|| Err(data("prepared upgrade missing")))?,
            root,
        ))
    }
    pub(crate) fn embedded_schema(&'static self) -> Result<&'static Schema, ManagedSchemaError> {
        self.schema.legacy(|| build_schema(self, None, |_| {}))
    }
    pub(crate) fn embedded_rights(
        &'static self,
    ) -> Result<&'static RightsTables, ManagedSchemaError> {
        self.rights.legacy(|| build_rights(self, None))
    }
    pub(crate) fn embedded_templates(
        &'static self,
    ) -> Result<&'static TemplateTables, ManagedSchemaError> {
        self.templates.legacy(|| build_templates(self, None))
    }
    pub(crate) fn embedded_stdattrs(
        &'static self,
    ) -> Result<&'static [forms::KindStdAttrs], ManagedSchemaError> {
        self.stdattrs
            .legacy(|| build_stdattrs(self, None))
            .map(Vec::as_slice)
    }
    pub(crate) fn embedded_upgrade(
        &'static self,
    ) -> Result<&'static UpgradeTables, ManagedSchemaError> {
        self.upgrade.legacy(|| build_upgrade(self, None))
    }
}

#[cfg(feature = "testing")]
pub mod testing {
    use super::*;
    use std::sync::Arc;
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum LearnedEvent {
        Entry,
        AfterClaim,
        BeforePublication,
        Published,
        Bound,
    }
    type LearnedHook = Arc<dyn Fn(LearnedEvent) + Send + Sync>;
    static LEARNED_HOOK: Lock<Option<LearnedHook>> = Lock::new(None);
    struct HookGuard(Option<LearnedHook>);
    impl Drop for HookGuard {
        fn drop(&mut self) {
            *LEARNED_HOOK.lock() = self.0.take();
        }
    }
    pub fn with_learned_hook<R>(
        hook: impl Fn(LearnedEvent) + Send + Sync + 'static,
        run: impl FnOnce() -> R,
    ) -> R {
        let previous = LEARNED_HOOK.lock().replace(Arc::new(hook));
        let _guard = HookGuard(previous);
        run()
    }
    pub(super) fn cell_event(cell: usize, event: LearnedEvent) {
        if let Ok(resource) = embedded_resource()
            && cell == std::ptr::from_ref(&resource.learned) as usize
        {
            let hook = LEARNED_HOOK.lock().clone();
            if let Some(hook) = hook {
                hook(event);
            }
        }
    }
    pub fn learned_published(resource: &SchemaResource) -> bool {
        resource.learned.value.get().is_some()
    }
    pub fn published_charge(resource: &SchemaResource) -> u64 {
        fn charge<T>(cell: &Cell<T>) -> u64 {
            cell.value.get().map_or(0, |p| {
                p.value.bytes()
                    + p.adopted
                        .lock()
                        .as_ref()
                        .map_or(0, memory_budget::Reservation::bytes)
            })
        }
        charge(&resource.schema)
            + charge(&resource.rights)
            + charge(&resource.templates)
            + charge(&resource.stdattrs)
            + charge(&resource.upgrade)
            + charge(&resource.learned)
    }
    pub fn learned_backing(resource: &SchemaResource) -> Result<u64, BudgetError> {
        resource
            .learned
            .value
            .get()
            .map_or(Ok(0), |p| p.value.value().backing())
    }
    pub fn resource_owner(resource: &SchemaResource) -> Option<MemoryBudget> {
        resource.binding.owner()
    }
    pub fn learned_legacy_builds(resource: &SchemaResource) -> usize {
        resource.learned_legacy_builds.load(Ordering::Relaxed)
    }
    pub fn map_capacity<K, V>(map: &FactMap<K, V>) -> usize {
        map.rows.capacity()
    }

    #[derive(Debug, Default)]
    pub struct Unpublished {
        schema: Option<Built<Schema>>,
        rights: Option<Built<RightsTables>>,
        templates: Option<Built<TemplateTables>>,
        stdattrs: Option<Built<Vec<forms::KindStdAttrs>>>,
        upgrade: Option<Built<UpgradeTables>>,
        learned: Option<Built<FactMap<String, KindSpec>>>,
    }
    impl Unpublished {
        pub fn learned_kinds(&self) -> Option<&FactMap<String, KindSpec>> {
            self.learned.as_ref().map(Built::value)
        }
        pub fn bytes(&self) -> u64 {
            self.schema
                .as_ref()
                .map(Built::bytes)
                .or_else(|| self.rights.as_ref().map(Built::bytes))
                .or_else(|| self.templates.as_ref().map(Built::bytes))
                .or_else(|| self.stdattrs.as_ref().map(Built::bytes))
                .or_else(|| self.upgrade.as_ref().map(Built::bytes))
                .or_else(|| self.learned.as_ref().map(Built::bytes))
                .unwrap_or(0)
        }
        pub fn actual_backing(&self) -> Result<u64, BudgetError> {
            self.schema
                .as_ref()
                .map(|v| v.value().backing())
                .or_else(|| self.rights.as_ref().map(|v| v.value().backing()))
                .or_else(|| self.templates.as_ref().map(|v| v.value().backing()))
                .or_else(|| self.stdattrs.as_ref().map(|v| v.value().backing()))
                .or_else(|| self.upgrade.as_ref().map(|v| v.value().backing()))
                .or_else(|| self.learned.as_ref().map(|v| v.value().backing()))
                .unwrap_or(Ok(0))
        }
    }
    pub fn build_unpublished(
        resource: &'static SchemaResource,
        group: SchemaGroup,
        root: &MemoryBudget,
    ) -> Result<Unpublished, ManagedSchemaError> {
        resource.require_embedded()?;
        let mut out = Unpublished::default();
        match group {
            SchemaGroup::Schema => out.schema = Some(build_schema(resource, Some(root), |_| {})?),
            SchemaGroup::Rights => out.rights = Some(build_rights(resource, Some(root))?),
            SchemaGroup::Templates => out.templates = Some(build_templates(resource, Some(root))?),
            SchemaGroup::StdAttrs => out.stdattrs = Some(build_stdattrs(resource, Some(root))?),
            SchemaGroup::Upgrade => out.upgrade = Some(build_upgrade(resource, Some(root))?),
            SchemaGroup::LearnedKinds => out.learned = Some(build_learned(resource, Some(root))?),
        }
        Ok(out)
    }
}
