use crate::bind::msg;
use bsl_prim::Str;
use bsl_vm::{
    Arg, CallResult, Exception, Execution, HostError, HostObject, MemberId, MemberKind, Registry,
    SharedObject, SharedValue, TypeId, Value,
};
use platform_catalog::{Context, EnumValueInfo, TypeIdx, TypeInfo, TypeKind, catalog};
use std::any::Any;
use std::rc::Rc;
use std::sync::Arc;

const NOT_HERE: [&str; 5] = [
    "ErrorCategory",
    "Chars",
    "WebColors",
    "StyleColors",
    "StyleFonts",
];

pub(crate) fn register(reg: &mut Registry) {
    let c = catalog();
    for (i, info) in c.types().iter().enumerate() {
        let Ok(i) = u16::try_from(i) else { break };
        if managed_here(info) {
            reg.system_enum(TypeIdx(i), manager(TypeIdx(i), enum_member));
        }
    }
}

pub(crate) fn managed_here(info: &TypeInfo) -> bool {
    info.kind == TypeKind::Enum
        && (info.avail.has(Context::Server) || info.enum_global)
        && !info.values.is_empty()
        && !NOT_HERE.contains(&info.id.as_str())
}

fn member_fn(t: TypeIdx) -> Option<MemberValue> {
    let info = catalog().types().get(usize::from(t.0))?;
    if info.id == "Chars" {
        return Some(crate::symbols::char_member);
    }
    managed_here(info).then_some(enum_member as MemberValue)
}

pub(crate) fn type_by_id(id: &str) -> Option<TypeIdx> {
    let i = catalog().types().iter().position(|t| t.id == id)?;
    u16::try_from(i).ok().map(TypeIdx)
}

pub(crate) type MemberValue = fn(TypeIdx, usize) -> Option<Value>;

pub(crate) fn manager(t: TypeIdx, value: MemberValue) -> bsl_vm::GlobalProp {
    Arc::new(move |_| Ok(Value::Object(Rc::new(Manager { t, value }))))
}

fn enum_member(t: TypeIdx, i: usize) -> Option<Value> {
    let info = catalog().types().get(usize::from(t.0))?;
    let member = info.values.get(i)?;
    if member.ordinal.is_none() && info.values.iter().any(|v| v.ordinal.is_some()) {
        return None;
    }
    let index = u32::try_from(i).ok()?;
    Some(Value::Object(Rc::new(EnumValue { t, index })))
}

pub fn enum_value(t: TypeIdx, name: &str) -> Option<Value> {
    member_fn(t)?(t, catalog().find_enum_value(t, name)?)
}

pub fn as_enum_value(v: &Value) -> Option<&EnumValue> {
    v.as_object()?.as_any().downcast_ref::<EnumValue>()
}

pub struct EnumValue {
    t: TypeIdx,
    index: u32,
}

impl EnumValue {
    pub fn type_idx(&self) -> TypeIdx {
        self.t
    }

    pub fn index(&self) -> usize {
        self.index as usize
    }

    pub fn ordinal(&self) -> i32 {
        self.info()
            .and_then(|v| v.ordinal)
            .unwrap_or(self.index as i32)
    }

    fn info(&self) -> Option<&'static EnumValueInfo> {
        value_info(self.t, self.index())
    }

    pub fn name_ru(&self) -> &'static str {
        self.info().map_or("", |v| v.name_ru.as_str())
    }

    pub fn name_en(&self) -> &'static str {
        self.info().map_or("", |v| v.name_en.as_str())
    }
}

fn value_info(t: TypeIdx, i: usize) -> Option<&'static EnumValueInfo> {
    catalog().types().get(usize::from(t.0))?.values.get(i)
}

fn value_name(t: TypeIdx, i: usize) -> &'static str {
    value_info(t, i).map_or("", |v| v.name_ru.as_str())
}

fn no_field(name: &str) -> HostError {
    HostError::key(msg::PROP_NOT_FOUND).with_arg(name).raw()
}

fn member_name(t: TypeIdx, m: MemberId) -> &'static str {
    value_name(t, usize::try_from(m.0).unwrap_or(usize::MAX))
}

impl HostObject for EnumValue {
    fn session_edges(&self, _: &mut dyn bsl_vm::SessionEdges) -> bool {
        true
    }
    fn session_transfer_supported(&self) -> bool {
        true
    }
    fn retained_memory(&self, _: &mut dyn bsl_vm::RetainedMemory) -> Result<bool, Exception> {
        Ok(true)
    }
    fn session_transfer(
        &self,
        out: &mut dyn bsl_vm::SessionExport,
    ) -> Result<Option<Box<dyn bsl_vm::SessionObject>>, Exception> {
        out.reserve(std::mem::size_of::<crate::session::Leaf<SharedEnum>>() as u64)?;
        Ok(Some(Box::new(crate::session::Leaf {
            value: SharedEnum {
                t: self.t,
                index: self.index,
            },
            restore_bytes: crate::collections::retained_rc_size::<Self>(),
        })))
    }
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.t)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        Err(no_field(member_name(self.t, m)).into())
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        Err(no_field(member_name(self.t, m)).into())
    }

    fn call(&self, _ex: &mut Execution, m: MemberId, _args: &mut [Arg]) -> CallResult<Value> {
        Err(HostError::key(msg::METHOD_NOT_FOUND)
            .with_arg(member_name(self.t, m))
            .raw()
            .into())
    }

    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<Str>> {
        if let Some(i) = self.info()
            && !catalog().type_info(self.t).enum_global
            && i.presentation_ru.as_deref() == Some(self.name_ru())
            && i.presentation_en.as_deref() == Some(self.name_ru())
        {
            return Ok(Some(Str::from(match ex.script_variant() {
                platform_catalog::messages::Lang::Ru => self.name_ru(),
                platform_catalog::messages::Lang::En => self.name_en(),
            })));
        }
        if let Some(s) = self.info().and_then(|i| match ex.lang() {
            platform_catalog::messages::Lang::Ru => i.presentation_ru.as_deref(),
            platform_catalog::messages::Lang::En => i.presentation_en.as_deref(),
        }) {
            return Ok(Some(Str::from(s)));
        }
        if let Some(s) = crate::json::enum_presentation(self.t, self.name_en(), ex.lang()) {
            return Ok(Some(s));
        }
        if let Some(s) = crate::xml::enum_presentation(self.t, self.name_en(), ex.lang()) {
            return Ok(Some(s));
        }
        if let Some(s) = crate::binary::byte_order_presentation(self.t, self.name_en(), ex.lang()) {
            return Ok(Some(s));
        }
        if let Some(s) = crate::binary::function_presentation(self.t, self.name_en(), ex.lang()) {
            return Ok(Some(s));
        }
        if let Some(s) =
            crate::ui::enum_presentation(self.t, self.name_ru(), self.name_en(), ex.lang())
        {
            return Ok(Some(s));
        }
        Ok(Some(Str::from(match ex.script_variant() {
            platform_catalog::messages::Lang::Ru => self.name_ru(),
            platform_catalog::messages::Lang::En => self.name_en(),
        })))
    }

    fn boost_hash(&self) -> Option<i32> {
        Some(self.ordinal())
    }

    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        let o = other.as_any().downcast_ref::<EnumValue>()?;
        let guid = |t| catalog().type_info(t).guid;
        let same_type = o.t == self.t || guid(self.t).is_some_and(|g| Some(g) == guid(o.t));
        Some(same_type && o.ordinal() == self.ordinal())
    }

    fn to_shared(&self) -> Option<SharedValue> {
        Some(SharedValue::Object(Arc::new(SharedEnum {
            t: self.t,
            index: self.index,
        })))
    }
}

struct SharedEnum {
    t: TypeIdx,
    index: u32,
}

impl SharedObject for SharedEnum {
    fn to_value(&self, _ex: &mut Execution) -> Result<Value, Exception> {
        Ok(Value::Object(Rc::new(EnumValue {
            t: self.t,
            index: self.index,
        })))
    }
}

struct Manager {
    t: TypeIdx,
    value: MemberValue,
}

impl Manager {
    fn member(&self, m: MemberId) -> CallResult<Value> {
        let i = usize::try_from(m.0).unwrap_or(usize::MAX);
        (self.value)(self.t, i).ok_or_else(|| no_field(value_name(self.t, i)).into())
    }
}

impl HostObject for Manager {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.t)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        self.member(m)
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        let name = value_name(self.t, usize::try_from(m.0).unwrap_or(usize::MAX));
        Err(HostError::key(msg::PROP_NOT_WRITABLE)
            .with_arg(name)
            .raw()
            .into())
    }

    fn call(&self, _ex: &mut Execution, m: MemberId, _args: &mut [Arg]) -> CallResult<Value> {
        Err(HostError::key(msg::METHOD_NOT_FOUND)
            .with_arg(member_name(self.t, m))
            .raw()
            .into())
    }

    fn find_member(&self, name: &str) -> Option<MemberId> {
        let i = catalog().find_enum_value(self.t, name)?;
        let info = catalog().type_info(self.t);
        if info.values.get(i)?.ordinal.is_none() && info.values.iter().any(|v| v.ordinal.is_some())
        {
            return None;
        }
        u32::try_from(i).ok().map(MemberId)
    }

    fn member_kind(&self, _m: MemberId) -> Option<MemberKind> {
        Some(MemberKind::Property {
            readable: true,
            writable: false,
        })
    }

    fn index_get(&self, ex: &mut Execution, key: &Value) -> CallResult<Value> {
        let name = match key {
            Value::String(s) => s.to_string_lossy(),
            other => ex.present(other)?.to_string_lossy(),
        };
        match self.find_member(&name) {
            Some(m) => self.member(m),
            None => Err(no_field(&name).into()),
        }
    }
}
