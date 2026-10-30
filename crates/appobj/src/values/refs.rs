use super::hash::ref_hash;
use crate::Application;
use crate::model::{ObjId, ObjKind};
use crate::read::{field_index, is_std_name, read_object_row, tabular_index};
use crate::readtab::tabular_value;
use crate::types::Category;
use bsl_prim::Str;
use bsl_vm::{
    Arg, CallResult, Exception, Execution, HostError, HostObject, MemberId, MemberKind,
    SharedObject, SharedValue, TypeId, Value,
};
use ibschema::values::RefId;
use std::any::Any;
use std::rc::Rc;
use std::sync::{Arc, Weak};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct RefValue {
    pub obj: ObjId,
    pub id: RefId,
}

impl RefValue {
    pub fn is_empty(&self) -> bool {
        self.id.is_nil()
    }
}

const INDEX_MEMBER: MemberId = MemberId(u32::MAX - 1);
const ATTR_BASE: u32 = 1 << 20;
const TAB_BASE: u32 = 1 << 21;

pub struct RefObject {
    pub r: RefValue,
    ty: TypeId,
    no: u32,
    app: Arc<Application>,
}

impl RefObject {
    pub(crate) fn belongs_to(&self, app: &Application) -> bool {
        std::ptr::eq(self.app.as_ref(), app)
    }

    pub fn table_number(&self) -> Option<u32> {
        self.app.model().storage_of(self.r.obj)?.ref_number
    }

    fn is_enum(&self) -> bool {
        self.app.model().obj(self.r.obj).kind == ObjKind::Enum
    }

    fn enum_index(&self) -> Option<usize> {
        let def = self.app.model().obj(self.r.obj);
        def.enum_values
            .iter()
            .position(|v| RefId::from_uuid(v.md) == self.r.id)
    }

    fn member(&self, ex: &Execution, m: MemberId) -> Option<(bool, &'static str)> {
        let t = ex.runtime().types().app_type(self.ty)?.template?;
        crate::meta::member_name(platform_catalog::catalog(), t, m)
            .map(|(method, ru, _)| (method, ru))
    }

    fn not_implemented(&self, member: &str) -> HostError {
        HostError::not_implemented("Ссылка", member)
    }

    fn attr(&self, ex: &mut Execution, i: usize) -> CallResult<Value> {
        let row = read_object_row(ex, &self.app, self.r)?;
        row.value(ex, &self.app, self.r.obj, i)
    }

    fn no_field(name: &str) -> HostError {
        HostError::key("bsl:IDS_RTE_VALPROP_NOTFOUND")
            .with_arg(name)
            .raw()
    }
}

pub fn ref_value(app: &Application, r: RefValue) -> Result<Value, HostError> {
    let arc = app.arc().ok_or_else(|| {
        HostError::text(
            "internal: the application of a reference is gone",
            "internal: the application of a reference is gone",
        )
    })?;
    let ty = app
        .types()
        .and_then(|t| t.type_of(r.obj, Category::Ref))
        .ok_or_else(|| {
            let name = &app.model().obj(r.obj).name;
            HostError::text(
                &format!("internal: no reference type for the object {name}"),
                &format!("internal: no reference type for the object {name}"),
            )
        })?;
    let no = app
        .model()
        .storage_of(r.obj)
        .and_then(|s| s.ref_number)
        .unwrap_or(0);
    Ok(Value::Object(Rc::new(RefObject {
        r,
        ty,
        no,
        app: arc,
    })))
}

pub fn as_ref(v: &Value) -> Option<RefValue> {
    v.as_object()?
        .as_any()
        .downcast_ref::<RefObject>()
        .map(|o| o.r)
}

pub fn empty_ref(app: &Application, obj: ObjId) -> Result<Value, HostError> {
    ref_value(
        app,
        RefValue {
            obj,
            id: RefId::NIL,
        },
    )
}

pub(crate) fn shared_empty(app: Weak<Application>, obj: ObjId) -> SharedValue {
    SharedValue::Object(Arc::new(SharedRef {
        app,
        r: RefValue {
            obj,
            id: RefId::NIL,
        },
    }))
}

struct SharedRef {
    app: Weak<Application>,
    r: RefValue,
}

impl SharedObject for SharedRef {
    fn to_value(&self, ex: &mut Execution) -> Result<Value, Exception> {
        match self.app.upgrade() {
            Some(app) => ref_value(&app, self.r).map_err(|e| ex.error(e)),
            None => Err(ex.error(HostError::text(
                "internal: the application of a reference is gone",
                "internal: the application of a reference is gone",
            ))),
        }
    }
}

impl HostObject for RefObject {
    fn point_in_time_key(&self, ex: &Execution) -> Option<([u8; 16], [u8; 16])> {
        let ty = ex.runtime().types().app_type(self.ty)?;
        Some((ty.guid, *self.r.id.to_uuid().as_bytes()))
    }

    fn retained_memory(
        &self,
        visit: &mut dyn bsl_vm::RetainedMemory,
    ) -> Result<bool, bsl_vm::Exception> {
        visit.work(1)?;
        Ok(true)
    }
    fn type_id(&self) -> TypeId {
        self.ty
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        if (ATTR_BASE..TAB_BASE).contains(&m.0) {
            return self.attr(ex, (m.0 - ATTR_BASE) as usize);
        }
        if (TAB_BASE..INDEX_MEMBER.0).contains(&m.0) {
            return tabular_value(ex, &self.app, self.r, (m.0 - TAB_BASE) as usize);
        }
        match self.member(ex, m) {
            Some((false, "Ссылка")) => Ok(ref_value(&self.app, self.r)?),
            Some((false, "ВерсияДанных")) if !self.is_enum() => {
                let row = read_object_row(ex, &self.app, self.r)?;
                let v = if row.version.is_empty() {
                    " ".repeat(12)
                } else {
                    row.version.clone()
                };
                Ok(Value::from(v.as_str()))
            }
            Some((false, ru)) if !self.is_enum() => {
                let def = self.app.model().obj(self.r.obj);
                if let Some(i) = tabular_index(def, ru) {
                    return tabular_value(ex, &self.app, self.r, i);
                }
                match field_index(def, ru) {
                    Some(i) => self.attr(ex, i),
                    None if bsl_prim::fold_name(ru) == "этогруппа"
                        && def.kind == crate::model::ObjKind::Catalog =>
                    {
                        Ok(Value::Bool(false))
                    }
                    None if is_std_name(ru) => Err(Self::no_field(ru).into()),
                    None => Err(self.not_implemented(ru).into()),
                }
            }
            Some((_, ru)) => Err(self.not_implemented(ru).into()),
            None => Err(self.not_implemented(&m.0.to_string()).into()),
        }
    }

    fn set(&self, ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        let name = self.member(ex, m).map_or("", |n| n.1);
        Err(HostError::key(bsl_stdlib::bind::msg::PROP_NOT_WRITABLE)
            .with_arg(name)
            .raw()
            .into())
    }

    fn call(&self, ex: &mut Execution, m: MemberId, _args: &mut [Arg]) -> CallResult<Value> {
        if m == INDEX_MEMBER {
            let i = self.enum_index().and_then(|i| i64::try_from(i).ok());
            return Ok(Value::from(i.unwrap_or(-1)));
        }
        match self.member(ex, m) {
            Some((true, "Пустая")) => Ok(Value::Bool(self.r.is_empty())),
            Some((true, "УникальныйИдентификатор")) => Ok(
                bsl_stdlib::values::new_uuid(*self.r.id.to_uuid().as_bytes()),
            ),
            Some((true, "ПолучитьОбъект")) if !self.is_enum() => {
                crate::objects::get_object(ex, &self.app, self.r)
            }
            Some((true, "Метаданные")) => {
                use crate::meta::{MdKey, MdPath, value_of};
                let cx = self.app.md_ctx().ok_or_else(|| {
                    HostError::text(
                        "internal: no Метаданные context",
                        "internal: no Метаданные context",
                    )
                })?;
                Ok(value_of(&cx, ex, MdKey::Path(MdPath::Object(self.r.obj))))
            }
            Some((_, ru)) => Err(self.not_implemented(ru).into()),
            None => Err(self.not_implemented(&m.0.to_string()).into()),
        }
    }

    fn find_member(&self, name: &str) -> Option<MemberId> {
        let n = bsl_prim::fold_name(name);
        if self.is_enum() {
            return (n == bsl_prim::fold_name("Индекс") || n == bsl_prim::fold_name("Index"))
                .then_some(INDEX_MEMBER);
        }
        let def = self.app.model().obj(self.r.obj);
        if let Some(i) = field_index(def, name).and_then(|i| u32::try_from(i).ok()) {
            return Some(MemberId(ATTR_BASE + i));
        }
        tabular_index(def, name)
            .and_then(|i| u32::try_from(i).ok())
            .map(|i| MemberId(TAB_BASE + i))
    }

    fn member_kind(&self, m: MemberId) -> Option<MemberKind> {
        if m == INDEX_MEMBER {
            return Some(MemberKind::Method {
                is_function: true,
                min_args: 0,
                max_args: Some(0),
            });
        }
        (m.0 >= ATTR_BASE).then_some(MemberKind::Property {
            readable: true,
            writable: false,
        })
    }

    fn is_empty_value(&self) -> bool {
        self.r.is_empty()
    }

    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<Str>> {
        crate::present::ref_presentation(ex, &self.app, self.r).map(Some)
    }

    fn boost_hash(&self) -> Option<i32> {
        Some(ref_hash(self.no, &self.r.id))
    }

    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        Some(
            other
                .as_any()
                .downcast_ref::<RefObject>()
                .is_some_and(|o| o.r == self.r),
        )
    }

    fn to_shared(&self) -> Option<SharedValue> {
        Some(SharedValue::Object(Arc::new(SharedRef {
            app: Arc::downgrade(&self.app),
            r: self.r,
        })))
    }
}
