use super::{Column, FIELD_BASE, SetCore, Source, column_type};
use crate::meta::{arg_value, member_name, no_field};
use crate::model::{FieldRole, InfoRegWriteMode, StdAttr};
use crate::read_budget;
use crate::session::internal;
use bsl_prim::fold_name;
use bsl_vm::{
    Arg, CallResult, Exception, Execution, HostError, HostObject, MemberId, MemberKind, ObjRef,
    RetainedMemory, TypeId, Value,
};
use platform_catalog::{TypeIdx, catalog};
use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;

const FILTER: &str = "Отбор";
const ITEM: &str = "ЭлементОтбора";

fn catalog_type(name: &str) -> Result<TypeIdx, HostError> {
    catalog()
        .find_type(name)
        .ok_or_else(|| internal("no type of the filter in the catalog"))
}

fn filter_columns(core: &SetCore, cols: &[Column]) -> Vec<usize> {
    let subordinate = core.def().behavior.write_mode == Some(InfoRegWriteMode::RecorderSubordinate);
    cols.iter()
        .enumerate()
        .filter(|(_, c)| {
            let Some(f) = core.field(c) else {
                return false;
            };
            if subordinate {
                f.std == Some(StdAttr::Recorder)
            } else {
                f.std == Some(StdAttr::Period)
                    || (f.std.is_none() && f.role == FieldRole::Dimension)
            }
        })
        .map(|(i, _)| i)
        .collect()
}

#[derive(Default)]
struct ItemState {
    value: Option<Value>,
    used: bool,
    value_assigned: bool,
    use_given: bool,
}

pub(crate) struct FilterItem {
    core: Rc<SetCore>,
    ty: TypeIdx,
    pub(crate) col: usize,
    name: String,
    state: RefCell<ItemState>,
}

impl FilterItem {
    fn refusal(&self, member: &str) -> HostError {
        HostError::not_implemented(ITEM, member)
    }

    pub(crate) fn use_unknown(&self) -> bool {
        let st = self.state.borrow();
        st.value_assigned && !st.use_given
    }

    pub(crate) fn used_value(&self) -> Option<Value> {
        let st = self.state.borrow();
        if st.used { st.value.clone() } else { None }
    }

    fn set_value(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let value = arg_value(ex, args, 0)?;
        if matches!(value, Value::Undefined) {
            return Ok(Value::Undefined);
        }
        let used = match arg_value(ex, args, 1)? {
            Value::Undefined => true,
            Value::Bool(b) => b,
            _ => return Err(self.refusal("Установить: использование не Булево").into()),
        };
        let cast = self.cast(ex, value)?;
        let mut st = self.state.borrow_mut();
        st.value = Some(cast);
        st.used = used;
        st.use_given = true;
        Ok(Value::Undefined)
    }

    fn cast(&self, ex: &mut Execution, value: Value) -> CallResult<Value> {
        let cols = self.core.cols()?;
        let col = cols
            .get(self.col)
            .ok_or_else(|| internal("no column of the filter"))?;
        let f = self
            .core
            .field(col)
            .ok_or_else(|| internal("a filter of a separator"))?;
        let cast = crate::values::typedesc::cast_to_field(&self.core.app, ex, f, value)?;
        read_budget::reserve(ex, read_budget::value_heap(&cast).saturating_add(256))?;
        Ok(cast)
    }
}

impl HostObject for FilterItem {
    fn retained_memory(&self, visit: &mut dyn RetainedMemory) -> Result<bool, Exception> {
        visit.work(1)?;
        visit.mutable();
        visit.bytes(self.name.capacity() as u64)?;
        if let Some(v) = &self.state.borrow().value {
            visit.value(v)?;
        }
        Ok(true)
    }

    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.ty)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        match member_name(catalog(), self.ty, m) {
            Some((false, "Имя", _)) => Ok(Value::from(self.name.as_str())),
            Some((false, "Использование", _)) => {
                Ok(Value::Bool(self.state.borrow().used))
            }
            Some((false, "Значение", _)) => match self.state.borrow().value.clone() {
                Some(v) => Ok(v),
                None => Err(self.refusal("Значение: элемент не устанавливали").into()),
            },
            Some((false, "ТипЗначения", _)) => {
                let cols = self.core.cols()?;
                let col = cols
                    .get(self.col)
                    .ok_or_else(|| internal("no column of the filter"))?;
                column_type(&self.core.app, ex, self.core.obj, col)
            }
            Some((_, ru, _)) => Err(self.refusal(ru).into()),
            None => Err(self.refusal(&m.0.to_string()).into()),
        }
    }

    fn set(&self, ex: &mut Execution, m: MemberId, v: Value) -> CallResult<()> {
        match member_name(catalog(), self.ty, m) {
            Some((false, "Значение", _)) => {
                if matches!(v, Value::Undefined) {
                    return Err(self.refusal("Значение: Неопределено").into());
                }
                let cast = self.cast(ex, v)?;
                let mut st = self.state.borrow_mut();
                st.value = Some(cast);
                st.value_assigned = true;
                Ok(())
            }
            Some((false, "Использование", _)) => match v {
                Value::Bool(b) => {
                    let mut st = self.state.borrow_mut();
                    st.used = b;
                    st.use_given = true;
                    Ok(())
                }
                _ => Err(self.refusal("Использование: значение не Булево").into()),
            },
            Some((_, ru, _)) => Err(self.refusal(ru).into()),
            None => Err(self.refusal(&m.0.to_string()).into()),
        }
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        match member_name(catalog(), self.ty, m) {
            Some((true, "Установить", _)) => self.set_value(ex, args),
            Some((_, ru, _)) => Err(self.refusal(ru).into()),
            None => Err(self.refusal(&m.0.to_string()).into()),
        }
    }
}

pub(crate) struct Filter {
    ty: TypeIdx,
    pub(crate) items: Vec<Rc<FilterItem>>,
}

impl Filter {
    pub(crate) fn new(ex: &mut Execution, core: &Rc<SetCore>) -> CallResult<Rc<Filter>> {
        let cols = core.cols()?;
        let item_ty = catalog_type(ITEM)?;
        let wanted = filter_columns(core, &cols);
        let bytes = wanted.iter().fold(
            read_budget::rc_size::<Filter>()
                .saturating_add(read_budget::slots::<Rc<FilterItem>>(wanted.len())),
            |n, i| {
                n.saturating_add(read_budget::rc_size::<FilterItem>())
                    .saturating_add(cols.get(*i).map_or(0, |c| c.name.len() as u64))
            },
        );
        read_budget::reserve(ex, bytes)?;
        let items = wanted
            .into_iter()
            .filter_map(|i| {
                let col = cols.get(i)?;
                debug_assert!(matches!(col.source, Source::Field(_)));
                Some(Rc::new(FilterItem {
                    core: core.clone(),
                    ty: item_ty,
                    col: i,
                    name: col.name.clone(),
                    state: RefCell::new(ItemState::default()),
                }))
            })
            .collect();
        Ok(Rc::new(Filter {
            ty: catalog_type(FILTER)?,
            items,
        }))
    }

    fn refusal(&self, member: &str) -> HostError {
        HostError::not_implemented(FILTER, member)
    }

    fn item(&self, i: usize) -> CallResult<Value> {
        let item: ObjRef = self.items.get(i).ok_or_else(super::off_bound)?.clone();
        Ok(Value::Object(item))
    }

    fn position(&self, name: &str) -> Option<usize> {
        let wanted = fold_name(name);
        self.items
            .iter()
            .position(|it| fold_name(&it.name) == wanted)
    }
}

impl HostObject for Filter {
    fn retained_memory(&self, visit: &mut dyn RetainedMemory) -> Result<bool, Exception> {
        visit.work(1)?;
        visit.bytes(read_budget::slots::<Rc<FilterItem>>(self.items.capacity()))?;
        for it in &self.items {
            let it: ObjRef = it.clone();
            visit.value(&Value::Object(it))?;
        }
        Ok(true)
    }

    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.ty)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        if m.0 >= FIELD_BASE {
            let i = usize::try_from(m.0 - FIELD_BASE).map_err(|_| internal("filter item"))?;
            return self.item(i);
        }
        let name =
            member_name(catalog(), self.ty, m).map_or_else(|| m.0.to_string(), |n| n.1.to_string());
        Err(no_field(&name).into())
    }

    fn set(&self, _: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        Err(self.refusal(&m.0.to_string()).into())
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        match member_name(catalog(), self.ty, m) {
            Some((true, "Количество", _)) => Ok(Value::from(
                i64::try_from(self.items.len()).unwrap_or(i64::MAX),
            )),
            Some((true, "Получить", _)) => {
                let Value::Number(n) = arg_value(ex, args, 0)? else {
                    return Err(crate::meta::type_mismatch(1).into());
                };
                let i =
                    usize::try_from(n.trunc_to_i64_saturating()).map_err(|_| super::off_bound())?;
                self.item(i)
            }
            Some((true, "Найти", _)) => match arg_value(ex, args, 0)? {
                Value::String(s) => match self.position(&s.to_string_lossy()) {
                    Some(i) => self.item(i),
                    None => Ok(Value::Undefined),
                },
                _ => Err(crate::meta::type_mismatch(1).into()),
            },
            Some((true, "Сбросить", _)) => {
                for it in &self.items {
                    let mut st = it.state.borrow_mut();
                    st.used = false;
                    st.use_given = true;
                }
                Ok(Value::Undefined)
            }
            Some((_, ru, _)) => Err(self.refusal(ru).into()),
            None => Err(self.refusal(&m.0.to_string()).into()),
        }
    }

    fn find_member(&self, name: &str) -> Option<MemberId> {
        let i = self.position(name)?;
        FIELD_BASE.checked_add(u32::try_from(i).ok()?).map(MemberId)
    }

    fn member_kind(&self, m: MemberId) -> Option<MemberKind> {
        let i = usize::try_from(m.0.checked_sub(FIELD_BASE)?).ok()?;
        (i < self.items.len()).then_some(MemberKind::Property {
            readable: true,
            writable: false,
        })
    }

    fn iter_len(&self, _: &mut Execution) -> Option<usize> {
        Some(self.items.len())
    }

    fn iter_get(&self, _: &mut Execution, i: usize) -> CallResult<Value> {
        self.item(i)
    }

    fn index_get(&self, _: &mut Execution, key: &Value) -> CallResult<Value> {
        match key {
            Value::Number(n) => {
                let i =
                    usize::try_from(n.trunc_to_i64_saturating()).map_err(|_| super::off_bound())?;
                self.item(i)
            }
            Value::String(s) => {
                let name = s.to_string_lossy();
                match self.position(&name) {
                    Some(i) => self.item(i),
                    None => Err(no_field(&name).into()),
                }
            }
            _ => Err(self.refusal("[ключ не число и не строка]").into()),
        }
    }
}
