use super::{
    creation_number, method_name, not_implemented, position, property_name, release, same_value,
    vec_with_capacity,
};
use crate::bind::{is_given, msg, to_number, type_mismatch, value_mismatch};
use crate::typedesc::{AllowedLength, TypeDescription, cast_value, idx, new_type_description};
use bsl_prim::Str;
use bsl_vm::{
    Arg, CallResult, Exception, Execution, HostError, HostObject, MemberId, TypeId, Value,
};
use std::{
    any::Any,
    cell::{Cell, RefCell},
    rc::Rc,
};

type Policy = Rc<RefCell<Option<TypeDescription>>>;

#[derive(Clone)]
struct ItemData {
    value: Value,
    presentation: Str,
    check: bool,
    picture: Value,
}

impl ItemData {
    fn take_values(&mut self) -> Vec<Value> {
        if matches!(self.value, Value::Undefined) && matches!(self.picture, Value::Undefined) {
            return Vec::new();
        }
        vec![
            std::mem::take(&mut self.value),
            std::mem::take(&mut self.picture),
        ]
    }
}

pub(crate) struct ValueListItem {
    data: RefCell<ItemData>,
    policy: Policy,
    revision: Rc<Cell<u64>>,
    id: u64,
    hash: i32,
}

pub(crate) struct ValueList {
    items: RefCell<Vec<Rc<ValueListItem>>>,
    policy: Policy,
    available: RefCell<Value>,
    next_id: Cell<u64>,
    revision: Rc<Cell<u64>>,
    hash: i32,
}

thread_local! { static PRESENT_DEPTH: Cell<u32> = const { Cell::new(0) }; }
struct PresentGuard;
impl PresentGuard {
    fn enter() -> CallResult<Self> {
        PRESENT_DEPTH.with(|depth| {
            if depth.get() >= 128 {
                return Err(reentrant().into());
            }
            depth.set(depth.get() + 1);
            Ok(Self)
        })
    }
}
impl Drop for PresentGuard {
    fn drop(&mut self) {
        PRESENT_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

fn reentrant() -> HostError {
    HostError::not_implemented("СписокЗначений", "Рекурсивная операция")
}
fn bump(revision: &Cell<u64>) {
    revision.set(revision.get().wrapping_add(1));
}
fn clone_policy(
    ex: &mut Execution,
    policy: &Policy,
    live: u64,
) -> Result<Option<TypeDescription>, Exception> {
    let policy = policy.borrow();
    if let Some(td) = policy.as_ref() {
        ex.reserve(live.saturating_add(
            (td.types().len() as u64).saturating_mul(std::mem::size_of::<TypeId>() as u64),
        ))?;
        ex.charge(td.types().len() as u64 / 64)?;
    }
    Ok(policy.clone())
}
fn adjust(ex: &mut Execution, policy: &Policy, v: Value, live: u64) -> Result<Value, Exception> {
    let td = clone_policy(ex, policy, live)?;
    match td {
        Some(td) => guarded_cast(ex, &td, v, live),
        None => Ok(v),
    }
}
fn text_conversion_bytes(
    ex: &mut Execution,
    s: &Str,
    attempts: u64,
    numeric: bool,
) -> Result<u64, Exception> {
    ex.charge((s.len() as u64 / 32 + 1).saturating_mul(attempts))?;
    let mut expansion = 0u64;
    if numeric {
        if let Some(i) = s.units().iter().position(|u| matches!(*u, 0x45 | 0x65)) {
            for &u in &s.units()[i + 1..] {
                if (0x30..=0x39).contains(&u) {
                    expansion = expansion
                        .saturating_mul(10)
                        .saturating_add(u64::from(u - 0x30))
                        .min(100_001);
                }
            }
        }
        ex.charge(expansion / 32)?;
    }
    Ok((s.len() as u64)
        .saturating_add(expansion)
        .saturating_mul(32))
}
pub(super) fn guarded_cast(
    ex: &mut Execution,
    td: &TypeDescription,
    value: Value,
    live: u64,
) -> Result<Value, Exception> {
    if td.types().is_empty() {
        return cast_value(ex, td, value);
    }
    ex.charge(td.types().len() as u64 / 64)?;
    let keeps = td.contains(value.type_id());
    let mut scratch = scalar_clone_bytes(&value).saturating_add(
        (td.types().len() as u64).saturating_mul(std::mem::size_of::<TypeId>() as u64),
    );
    if let Value::String(s) = &value
        && !keeps
    {
        scratch = scratch.saturating_add(text_conversion_bytes(
            ex,
            s,
            td.types().len() as u64,
            td.contains(TypeId::NUMBER),
        )?);
    }
    if let Value::Number(n) = &value
        && (!keeps || td.number().digits != 0)
    {
        ex.charge(n.size_hint_limbs() / 64)?;
        scratch =
            scratch.saturating_add(n.size_hint_limbs().saturating_mul(32).saturating_add(256));
    }
    if !keeps && td.contains(TypeId::STRING) {
        let presentation = crate::present::preflight_collection_presentation(
            ex,
            &value,
            live.saturating_add(scratch),
        )?;
        scratch = scratch.saturating_add(presentation);
    }
    if (!keeps || matches!(value, Value::String(_))) && td.contains(TypeId::STRING) {
        let q = td.string();
        if q.length != 0 {
            let length = match &value {
                Value::String(s) if q.allowed != AllowedLength::Fixed => {
                    u64::from(q.length).min(s.len() as u64)
                }
                _ => u64::from(q.length),
            };
            scratch = scratch.saturating_add(length.saturating_mul(2));
        }
    }
    if (!keeps || crate::values::binary_data_bytes(&value).is_some())
        && td.contains(TypeId::of_catalog(idx("BinaryData")))
    {
        let q = td.binary();
        if q.length != 0 {
            let length = if q.allowed == AllowedLength::Fixed {
                u64::from(q.length)
            } else {
                crate::values::binary_data_bytes(&value)
                    .map_or(0, |b| b.len() as u64)
                    .min(u64::from(q.length))
            };
            scratch = scratch.saturating_add(length.saturating_mul(2));
        }
    }
    ex.reserve(
        crate::present::collection_presentation_live()
            .saturating_add(live)
            .saturating_add(scratch),
    )?;
    let _guard = crate::present::CollectionPresentationGuard::enter(live.saturating_add(scratch));
    cast_value(ex, td, value)
}
fn arg_value(ex: &mut Execution, arg: Option<&Arg>) -> CallResult<Value> {
    arg_value_cached(ex, arg, 0)
}
pub(super) fn arg_value_cached(
    ex: &mut Execution,
    arg: Option<&Arg>,
    live: u64,
) -> CallResult<Value> {
    match arg {
        Some(Arg::Val(value)) => {
            ex.reserve(live.saturating_add(scalar_clone_bytes(value)))?;
            Ok(clone_value(ex, value)?)
        }
        Some(Arg::Place(place)) => {
            let value = place.get_guarded(ex, live)?;
            ex.reserve(live.saturating_add(scalar_clone_bytes(&value)))?;
            Ok(value)
        }
        _ => Ok(Value::Undefined),
    }
}
fn arg_presentation(ex: &mut Execution, arg: Option<&Arg>, live: u64) -> CallResult<(Str, u64)> {
    let value = arg_value_cached(ex, arg, live)?;
    match value {
        Value::String(s) => Ok((s, 0)),
        Value::Undefined | Value::Null => Ok((Str::new(), 0)),
        _ => {
            let text = guarded_present(ex, &value, live)?;
            let bytes = (text.len() as u64).saturating_mul(2);
            ex.reserve(
                live.saturating_add(scalar_clone_bytes(&value))
                    .saturating_add(bytes),
            )?;
            Ok((text, bytes))
        }
    }
}
fn bool_value(ex: &mut Execution, value: &Value) -> Result<bool, Exception> {
    if let Value::String(s) = value {
        ex.charge(s.len() as u64 / 64)?;
    }
    bsl_vm::ops::to_bool(ex, value)
}
fn arg_bool(ex: &mut Execution, arg: Option<&Arg>) -> CallResult<bool> {
    arg_bool_cached(ex, arg, 0)
}
fn arg_bool_cached(ex: &mut Execution, arg: Option<&Arg>, live: u64) -> CallResult<bool> {
    let value = arg_value_cached(ex, arg, live)?;
    Ok(bool_value(ex, &value)?)
}
fn arg_i64(ex: &mut Execution, arg: Option<&Arg>, _: usize) -> CallResult<i64> {
    arg_i64_cached(ex, arg, 0)
}
pub(super) fn arg_i64_cached(ex: &mut Execution, arg: Option<&Arg>, live: u64) -> CallResult<i64> {
    let value = arg_value_cached(ex, arg, live)?;
    if let Value::Number(n) = value {
        return Ok(n.trunc_to_i64_saturating());
    }
    if let Value::String(s) = &value {
        let bytes = text_conversion_bytes(ex, s, 1, true)?;
        ex.reserve(live.saturating_add(bytes))?;
    }
    Ok(to_number(ex, &value)?.trunc_to_i64_saturating())
}
fn guarded_present(ex: &mut Execution, value: &Value, live: u64) -> Result<Str, Exception> {
    crate::present::present_for_collection(
        ex,
        value,
        live.saturating_add(scalar_clone_bytes(value)),
    )
}
pub(super) fn scalar_clone_bytes(value: &Value) -> u64 {
    match value {
        Value::Number(n) => n.size_hint_limbs().saturating_mul(4),
        _ => 0,
    }
}
fn source_shared_value(value: &Value) -> Option<Value> {
    match value {
        Value::String(_) | Value::Object(_) => Some(value.clone()),
        _ => None,
    }
}
fn prepared_payload_bytes(value: &Value, source: Option<&Value>) -> u64 {
    match (value, source) {
        (Value::String(s), Some(Value::String(original)))
            if std::ptr::eq(original.units(), s.units()) =>
        {
            return 0;
        }
        (Value::Object(object), Some(Value::Object(original))) if Rc::ptr_eq(object, original) => {
            return 0;
        }
        _ => {}
    }
    if let Some(bytes) = crate::values::binary_data_bytes(value) {
        return bytes.len() as u64;
    }
    match value {
        Value::String(s) => (s.len() as u64).saturating_mul(2),
        _ => scalar_clone_bytes(value),
    }
}
fn clone_value(ex: &mut Execution, value: &Value) -> Result<Value, Exception> {
    if let Value::Number(n) = value {
        ex.reserve(n.size_hint_limbs().saturating_mul(4))?;
        ex.charge(n.size_hint_limbs() / 64)?;
    }
    Ok(value.clone())
}
fn comparison_resources(value: &Value) -> (u64, u64) {
    if let Some(bytes) = crate::values::binary_data_bytes(value) {
        return (bytes.len() as u64 / 64 + 1, 0);
    }
    if let Some(td) = value
        .as_object()
        .and_then(|o| o.as_any().downcast_ref::<TypeDescription>())
    {
        return (td.types().len() as u64 / 64 + 1, 0);
    }
    if let Some(resources) = crate::ui::comparison_resources(value) {
        return resources;
    }
    if let Some(len) = crate::values::xml_type::comparison_input_len(value)
        .or_else(|| crate::xdto::comparison_input_len(value))
    {
        return (len / 64 + 1, 0);
    }
    (0, 0)
}
pub(super) fn guard_equal(
    ex: &mut Execution,
    a: &Value,
    b: &Value,
    live: u64,
) -> Result<(), Exception> {
    bsl_vm::ops::charge_equal(ex, a, b)?;
    if let (Value::Object(a_ref), Value::Object(b_ref)) = (a, b)
        && !Rc::ptr_eq(a_ref, b_ref)
        && a.type_id() == b.type_id()
    {
        let (a_work, a_bytes) = comparison_resources(a);
        let (b_work, b_bytes) = comparison_resources(b);
        ex.charge(a_work.saturating_add(b_work))?;
        ex.reserve(live.saturating_add(a_bytes).saturating_add(b_bytes))?;
    }
    Ok(())
}

fn item_text(ex: &mut Execution, value: &Value, presentation: &Str, live: u64) -> CallResult<Str> {
    if !presentation.is_empty() {
        return Ok(presentation.clone());
    }
    if matches!(value, Value::Undefined) {
        return Ok(Str::from(if ex.script_variant() == bsl_vm::Lang::Ru {
            "<Пустое значение>"
        } else {
            "<Empty value>"
        }));
    }
    Ok(guarded_present(ex, value, live)?)
}

impl ValueListItem {
    pub(super) fn visit_values(
        &self,
        visit: &mut super::retire::Visitor<'_>,
    ) -> Result<(), Exception> {
        let data = self.data.borrow();
        visit(&data.value)?;
        visit(&data.picture)
    }
    pub(super) fn retire_into(&self, out: &mut Vec<Value>) {
        let mut data = self.data.borrow_mut();
        super::retire::push(out, std::mem::take(&mut data.value));
        super::retire::push(out, std::mem::take(&mut data.picture));
    }

    fn new(data: ItemData, policy: Policy, revision: Rc<Cell<u64>>, id: u64) -> Rc<Self> {
        Rc::new(Self {
            data: RefCell::new(data),
            policy,
            revision,
            id,
            hash: creation_number(),
        })
    }
    fn text(&self, ex: &mut Execution, live: u64) -> CallResult<Str> {
        let value = {
            let data = self.data.borrow();
            if !data.presentation.is_empty() {
                return Ok(data.presentation.clone());
            }
            ex.reserve(
                crate::present::collection_presentation_live()
                    .saturating_add(live)
                    .saturating_add(scalar_clone_bytes(&data.value)),
            )?;
            clone_value(ex, &data.value)?
        };
        item_text(ex, &value, &Str::new(), live)
    }
    fn key(
        &self,
        ex: &mut Execution,
        cache_bytes: &mut u64,
    ) -> Result<crate::sort::SortKey, Exception> {
        let value = {
            let data = self.data.borrow();
            if let Value::Number(n) = &data.value {
                ex.reserve(cache_bytes.saturating_add(n.size_hint_limbs().saturating_mul(8)))?;
            }
            clone_value(ex, &data.value)?
        };
        crate::sort::sort_key(ex, &value, cache_bytes)
    }
    pub(crate) fn take_values(&self) -> Vec<Value> {
        self.data.borrow_mut().take_values()
    }
}
impl Drop for ValueListItem {
    fn drop(&mut self) {
        release(self.data.get_mut().take_values());
    }
}

impl ValueList {
    pub(super) fn visit_values(
        &self,
        visit: &mut super::retire::Visitor<'_>,
    ) -> Result<(), Exception> {
        for item in self.items.borrow().iter() {
            visit(&Value::Object(item.clone()))?;
        }
        visit(&self.available.borrow())
    }
    pub(super) fn retire_into(&self, out: &mut Vec<Value>) {
        for item in self.items.borrow_mut().drain(..) {
            super::retire::push(out, Value::Object(item));
        }
        super::retire::push(out, std::mem::take(&mut *self.available.borrow_mut()));
    }

    fn empty() -> Self {
        Self {
            items: RefCell::new(Vec::new()),
            policy: Rc::new(RefCell::new(None)),
            available: RefCell::new(Value::Undefined),
            next_id: Cell::new(0),
            revision: Rc::new(Cell::new(0)),
            hash: creation_number(),
        }
    }
    fn of(v: &Value) -> Option<&Self> {
        v.as_object()?.as_any().downcast_ref()
    }
    fn len(&self) -> usize {
        self.items.borrow().len()
    }
    fn snapshot(&self, ex: &mut Execution) -> Result<Vec<Rc<ValueListItem>>, Exception> {
        ex.charge(self.len() as u64)?;
        let mut out = vec_with_capacity(ex, self.len() as u64)?;
        out.extend(self.items.borrow().iter().cloned());
        Ok(out)
    }
    fn check_revision(&self, expected: u64) -> CallResult<()> {
        if self.revision.get() != expected {
            return Err(reentrant().into());
        }
        Ok(())
    }
    pub(crate) fn take_values(&self) -> Vec<Value> {
        let items = std::mem::take(&mut *self.items.borrow_mut());
        if items.is_empty() && matches!(*self.available.borrow(), Value::Undefined) {
            return Vec::new();
        }
        let mut values: Vec<_> = items.into_iter().map(|item| Value::Object(item)).collect();
        values.push(std::mem::take(&mut *self.available.borrow_mut()));
        values
    }
    fn item_at(&self, i: usize) -> CallResult<Value> {
        self.items
            .borrow()
            .get(i)
            .map(|item| Value::Object(item.clone()))
            .ok_or_else(|| HostError::key(msg::INDEX_OFFBOUND).into())
    }
    fn checked_number(
        &self,
        ex: &mut Execution,
        v: &Value,
        insert: bool,
        live: u64,
    ) -> CallResult<usize> {
        let index = if let Value::Number(n) = v {
            position(n)
        } else {
            if let Value::String(s) = v {
                let bytes = text_conversion_bytes(ex, s, 1, true)?;
                ex.reserve(live.saturating_add(bytes))?;
            }
            position(&to_number(ex, v).map_err(|_| type_mismatch(1))?)
        };
        match index {
            Some(i) if i < self.len() || (insert && i == self.len()) => Ok(i),
            _ => Err(HostError::key(msg::RANGE).into()),
        }
    }
    fn find_item(&self, ex: &mut Execution, v: &Value) -> CallResult<Option<usize>> {
        let Some(item) = v
            .as_object()
            .and_then(|o| o.as_any().downcast_ref::<ValueListItem>())
        else {
            return Err(type_mismatch(1).into());
        };
        ex.charge(self.len() as u64)?;
        Ok(self
            .items
            .borrow()
            .iter()
            .position(|other| std::ptr::eq(&**other, item)))
    }
    fn selector(&self, ex: &mut Execution, v: &Value) -> CallResult<usize> {
        if matches!(v, Value::Number(_)) {
            return self.checked_number(ex, v, false, 0);
        }
        self.find_item(ex, v)?
            .ok_or_else(|| value_mismatch(1).into())
    }
    fn add(&self, ex: &mut Execution, args: &mut [Arg], insert: bool) -> CallResult<Value> {
        let index = if insert {
            Some(arg_value(ex, args.first())?)
        } else {
            None
        };
        let args = if insert {
            args.get(1..).unwrap_or_default()
        } else {
            args
        };
        let mut live = index.as_ref().map_or(0, scalar_clone_bytes);
        let value = arg_value_cached(ex, args.first(), live)?;
        live = live.saturating_add(scalar_clone_bytes(&value));
        let (presentation, presentation_bytes) = arg_presentation(ex, args.get(1), live)?;
        live = live.saturating_add(presentation_bytes);
        let check = if is_given(args.get(2)) {
            arg_bool_cached(ex, args.get(2), live)?
        } else {
            false
        };
        let picture = arg_value_cached(ex, args.get(3), live)?;
        live = live.saturating_add(scalar_clone_bytes(&picture));
        let revision = self.revision.get();
        let source_bytes = scalar_clone_bytes(&value);
        let source = source_shared_value(&value);
        let value = adjust(ex, &self.policy, value, live.saturating_sub(source_bytes))?;
        live = live
            .saturating_sub(source_bytes)
            .saturating_add(prepared_payload_bytes(&value, source.as_ref()));
        self.check_revision(revision)?;
        let i = match index {
            Some(v) => self.checked_number(ex, &v, true, live)?,
            None => self.len(),
        };
        let id = self.next_id.get();
        let next = id
            .checked_add(1)
            .filter(|n| *n <= i64::MAX as u64)
            .ok_or_else(|| ex.out_of_memory())?;
        ex.reserve(live.saturating_add(
            ((self.len() as u64) + 1).saturating_mul(std::mem::size_of::<ValueListItem>() as u64),
        ))?;
        ex.charge((self.len() - i) as u64 + 1)?;
        let item = ValueListItem::new(
            ItemData {
                value,
                presentation,
                check,
                picture,
            },
            self.policy.clone(),
            self.revision.clone(),
            id,
        );
        {
            let mut items = self.items.borrow_mut();
            items.try_reserve(1).map_err(|_| ex.out_of_memory())?;
            items.insert(i, item.clone());
        }
        self.next_id.set(next);
        bump(&self.revision);
        Ok(Value::Object(item))
    }
    fn set_type(&self, ex: &mut Execution, value: Value) -> CallResult<()> {
        let td = value
            .as_object()
            .and_then(|o| o.as_any().downcast_ref::<TypeDescription>())
            .ok_or_else(|| type_mismatch(1))?;
        ex.reserve((td.types().len() as u64).saturating_mul(std::mem::size_of::<TypeId>() as u64))?;
        ex.charge(td.types().len() as u64 / 64)?;
        let td = td.clone();
        let items = self.snapshot(ex)?;
        let revision = self.revision.get();
        let mut bytes = (items.len() as u64).saturating_mul(
            (2 * std::mem::size_of::<Value>() + std::mem::size_of::<Rc<ValueListItem>>()) as u64,
        );
        ex.reserve(bytes)?;
        let mut prepared = vec_with_capacity(ex, items.len() as u64)?;
        for item in &items {
            let (v, source) = {
                let data = item.data.borrow();
                ex.reserve(bytes.saturating_add(scalar_clone_bytes(&data.value)))?;
                (
                    clone_value(ex, &data.value)?,
                    source_shared_value(&data.value),
                )
            };
            let v = guarded_cast(ex, &td, v, bytes)?;
            self.check_revision(revision)?;
            bytes = bytes.saturating_add(prepared_payload_bytes(&v, source.as_ref()));
            ex.reserve(bytes)?;
            prepared.push(v);
        }
        let mut old = vec_with_capacity(ex, items.len() as u64)?;
        for (item, v) in items.iter().zip(prepared) {
            old.push(std::mem::replace(&mut item.data.borrow_mut().value, v));
        }
        *self.policy.borrow_mut() = Some(td);
        bump(&self.revision);
        release(old);
        Ok(())
    }
    fn copy(&self, ex: &mut Execution) -> CallResult<Value> {
        let count = self.len() as u64;
        ex.charge(count)?;
        let mut bytes = count.saturating_mul(
            (std::mem::size_of::<ValueListItem>() + 4 * std::mem::size_of::<usize>()) as u64,
        );
        for item in self.items.borrow().iter() {
            let data = item.data.borrow();
            for value in [&data.value, &data.picture] {
                if let Value::Number(n) = value {
                    ex.charge(n.size_hint_limbs() / 64)?;
                    bytes = bytes.saturating_add(n.size_hint_limbs().saturating_mul(4));
                }
            }
        }
        ex.reserve(bytes)?;
        let copy = Self::empty();
        *copy.policy.borrow_mut() = clone_policy(ex, &self.policy, bytes)?;
        *copy.available.borrow_mut() = self.available.borrow().clone();
        copy.next_id.set(self.next_id.get());
        let items = self.snapshot(ex)?;
        let mut result = vec_with_capacity(ex, items.len() as u64)?;
        for item in items {
            result.push(ValueListItem::new(
                item.data.borrow().clone(),
                copy.policy.clone(),
                copy.revision.clone(),
                item.id,
            ));
        }
        *copy.items.borrow_mut() = result;
        Ok(Value::Object(Rc::new(copy)))
    }
    fn load(&self, ex: &mut Execution, value: Value) -> CallResult<()> {
        let array = super::array::Array::of(&value)
            .filter(|a| !a.is_fixed())
            .ok_or_else(|| type_mismatch(1))?;
        let count = array.iter_len(ex).unwrap_or(0) as u64;
        ex.charge(count.saturating_add(self.len() as u64))?;
        let snapshot_bytes = array.items_clone_bytes();
        let numeric_bytes = snapshot_bytes
            .saturating_sub(count.saturating_mul(std::mem::size_of::<Value>() as u64));
        ex.charge(numeric_bytes / (4 * 64))?;
        let mut bytes = count
            .saturating_mul(
                (std::mem::size_of::<ValueListItem>() + std::mem::size_of::<Rc<ValueListItem>>())
                    as u64,
            )
            .saturating_add(snapshot_bytes);
        ex.reserve(bytes)?;
        let values = array.items();
        let revision = self.revision.get();
        let start = self.next_id.get();
        let next = start
            .checked_add(values.len() as u64)
            .filter(|n| *n <= i64::MAX as u64)
            .ok_or_else(|| ex.out_of_memory())?;
        let mut prepared = vec_with_capacity(ex, values.len() as u64)?;
        ex.reserve(
            (values.len() as u64).saturating_mul(std::mem::size_of::<ValueListItem>() as u64),
        )?;
        ex.charge(values.len() as u64)?;
        for (i, value) in values.into_iter().enumerate() {
            let source = source_shared_value(&value);
            let source_bytes = scalar_clone_bytes(&value);
            let value = adjust(ex, &self.policy, value, bytes)?;
            self.check_revision(revision)?;
            bytes = bytes
                .saturating_sub(source_bytes)
                .saturating_add(prepared_payload_bytes(&value, source.as_ref()));
            ex.reserve(bytes)?;
            prepared.push(ValueListItem::new(
                ItemData {
                    value,
                    presentation: Str::new(),
                    check: false,
                    picture: Value::Undefined,
                },
                self.policy.clone(),
                self.revision.clone(),
                start + i as u64,
            ));
        }
        self.reserve_cleanup(ex, bytes)?;
        let old = std::mem::replace(&mut *self.items.borrow_mut(), prepared);
        self.next_id.set(next);
        bump(&self.revision);
        release(old.into_iter().map(|item| Value::Object(item)).collect());
        Ok(())
    }
    fn reserve_cleanup(&self, ex: &mut Execution, live: u64) -> Result<(), Exception> {
        ex.reserve(live.saturating_add((self.len() as u64).saturating_mul(
            (std::mem::size_of::<Rc<ValueListItem>>() + std::mem::size_of::<Value>()) as u64,
        )))
    }
    fn sort(&self, ex: &mut Execution, args: &mut [Arg], presentation: bool) -> CallResult<()> {
        let direction = arg_value(ex, args.first())?;
        let desc = if matches!(direction, Value::Undefined) {
            false
        } else {
            let dir = crate::enums::as_enum_value(&direction)
                .filter(|e| e.type_idx() == idx("SortDirection"))
                .ok_or_else(|| type_mismatch(1))?;
            dir.name_en() == "Desc"
        };
        let mut items = self.snapshot(ex)?;
        let revision = self.revision.get();
        let mut keys = vec_with_capacity(ex, items.len() as u64)?;
        let mut cache_bytes = (items.len() as u64).saturating_mul(
            (std::mem::size_of::<Rc<ValueListItem>>()
                + std::mem::size_of::<Vec<crate::sort::SortKey>>()
                + std::mem::size_of::<crate::sort::SortKey>()
                + 2 * std::mem::size_of::<usize>()) as u64,
        );
        ex.reserve(cache_bytes)?;
        for item in &items {
            let key = if presentation {
                let value = Value::String(item.text(ex, cache_bytes)?);
                crate::sort::sort_key(ex, &value, &mut cache_bytes)?
            } else {
                item.key(ex, &mut cache_bytes)?
            };
            keys.push(vec![key]);
            self.check_revision(revision)?;
        }
        crate::sort::sort_by_keys(ex, &mut items, &keys, &[desc])?;
        let old = std::mem::replace(&mut *self.items.borrow_mut(), items);
        bump(&self.revision);
        drop(old);
        Ok(())
    }
}
impl Drop for ValueList {
    fn drop(&mut self) {
        release(self.take_values());
    }
}

impl HostObject for ValueList {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(idx("ValueList"))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        match property_name(idx("ValueList"), m) {
            Some("AvailableValues") => Ok(self.available.borrow().clone()),
            Some("ValueType") => Ok(new_type_description(
                clone_policy(ex, &self.policy, 0)?.unwrap_or_else(|| {
                    TypeDescription::new(&[], None, None, None, None, ex.runtime())
                }),
            )),
            _ => Err(not_implemented(idx("ValueList"), m).into()),
        }
    }
    fn set(&self, ex: &mut Execution, m: MemberId, value: Value) -> CallResult<()> {
        match property_name(idx("ValueList"), m) {
            Some("ValueType") => self.set_type(ex, value),
            Some("AvailableValues") => {
                if !matches!(value, Value::Undefined) && Self::of(&value).is_none() {
                    return Err(type_mismatch(1).into());
                }
                let old = std::mem::replace(&mut *self.available.borrow_mut(), value);
                bump(&self.revision);
                release(vec![old]);
                Ok(())
            }
            _ => Err(not_implemented(idx("ValueList"), m).into()),
        }
    }
    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        match method_name(idx("ValueList"), m) {
            Some("Add") => return self.add(ex, args, false),
            Some("Insert") => return self.add(ex, args, true),
            Some("Count") => return Ok(Value::from(self.len() as i64)),
            Some("Get") => {
                let v = arg_value(ex, args.first())?;
                return self.item_at(self.checked_number(ex, &v, false, 0)?);
            }
            Some("IndexOf") => {
                let v = arg_value(ex, args.first())?;
                return Ok(Value::from(
                    self.find_item(ex, &v)?.map_or(-1, |i| i as i64),
                ));
            }
            Some("FindByValue") => {
                let v = arg_value(ex, args.first())?;
                let live = scalar_clone_bytes(&v).saturating_add(
                    (self.len() as u64)
                        .saturating_mul(std::mem::size_of::<Rc<ValueListItem>>() as u64),
                );
                ex.reserve(live)?;
                let items = self.snapshot(ex)?;
                for item in items {
                    let value = {
                        let data = item.data.borrow();
                        ex.reserve(live.saturating_add(scalar_clone_bytes(&data.value)))?;
                        clone_value(ex, &data.value)?
                    };
                    guard_equal(ex, &value, &v, live)?;
                    if same_value(&value, &v) {
                        return Ok(Value::Object(item));
                    }
                }
                return Ok(Value::Undefined);
            }
            Some("FindByID") => {
                let id = arg_i64(ex, args.first(), 1)?;
                ex.charge(self.len() as u64)?;
                let result = self
                    .items
                    .borrow()
                    .iter()
                    .find(|item| i64::try_from(item.id).ok() == Some(id))
                    .cloned();
                return Ok(result.map_or(Value::Undefined, |item| Value::Object(item)));
            }
            Some("Clear") => {
                ex.charge(self.len() as u64)?;
                self.reserve_cleanup(ex, 0)?;
                let old = std::mem::take(&mut *self.items.borrow_mut());
                bump(&self.revision);
                release(old.into_iter().map(|item| Value::Object(item)).collect());
            }
            Some("Delete") => {
                let v = arg_value(ex, args.first())?;
                let i = self.selector(ex, &v)?;
                ex.charge((self.len() - i) as u64)?;
                let old = self.items.borrow_mut().remove(i);
                bump(&self.revision);
                release(vec![Value::Object(old)]);
            }
            Some("Move") => {
                let v = arg_value(ex, args.first())?;
                let offset = arg_i64_cached(ex, args.get(1), scalar_clone_bytes(&v))?;
                let i = self.selector(ex, &v)?;
                let dest = (i as i64)
                    .checked_add(offset)
                    .and_then(|n| usize::try_from(n).ok())
                    .filter(|n| *n < self.len())
                    .ok_or_else(|| value_mismatch(2))?;
                ex.charge(self.len() as u64)?;
                let mut items = self.items.borrow_mut();
                let item = items.remove(i);
                items.insert(dest, item);
                bump(&self.revision);
            }
            Some("FillChecks") => {
                let check = arg_bool(ex, args.first())?;
                ex.charge(self.len() as u64)?;
                for item in self.items.borrow().iter() {
                    item.data.borrow_mut().check = check;
                }
                bump(&self.revision);
            }
            Some("UnloadValues") => {
                ex.charge(self.len() as u64)?;
                let mut bytes =
                    (self.len() as u64).saturating_mul(std::mem::size_of::<Value>() as u64);
                for item in self.items.borrow().iter() {
                    if let Value::Number(n) = &item.data.borrow().value {
                        ex.charge(n.size_hint_limbs() / 64)?;
                        bytes = bytes.saturating_add(n.size_hint_limbs().saturating_mul(4));
                    }
                }
                ex.reserve(bytes)?;
                let mut values = vec_with_capacity(ex, self.len() as u64)?;
                values.extend(
                    self.items
                        .borrow()
                        .iter()
                        .map(|item| item.data.borrow().value.clone()),
                );
                return Ok(super::new_array(values));
            }
            Some("LoadValues") => {
                let value = arg_value(ex, args.first())?;
                self.load(ex, value)?;
            }
            Some("Copy") => return self.copy(ex),
            Some("SortByValue") => self.sort(ex, args, false)?,
            Some("SortByPresentation") => self.sort(ex, args, true)?,
            _ => return Err(not_implemented(idx("ValueList"), m).into()),
        }
        Ok(Value::Undefined)
    }
    fn index_get(&self, ex: &mut Execution, key: &Value) -> CallResult<Value> {
        let Value::Number(n) = key else {
            let text = guarded_present(ex, key, 0)?;
            let len = text.len() as u64;
            ex.charge(len / 32 + 1)?;
            ex.reserve(
                crate::present::collection_presentation_live()
                    .saturating_add(len.saturating_mul(32).saturating_add(2048)),
            )?;
            return Err(HostError::key(msg::PROP_NOT_FOUND)
                .with_arg(text.to_string_lossy())
                .into());
        };
        match position(n) {
            Some(i) => self.item_at(i),
            None => Err(HostError::key(msg::INDEX_OFFBOUND).into()),
        }
    }
    fn index_set(&self, _: &mut Execution, _: &Value, _: Value) -> CallResult<()> {
        Err(HostError::key(msg::INDEXED_READONLY).into())
    }
    fn iter_len(&self, _: &mut Execution) -> Option<usize> {
        Some(self.len())
    }
    fn iter_get(&self, _: &mut Execution, i: usize) -> CallResult<Value> {
        self.item_at(i)
    }
    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<Str>> {
        let _guard = PresentGuard::enter()?;
        let inherited = crate::present::collection_presentation_live();
        ex.reserve(inherited.saturating_add(
            (self.len() as u64).saturating_mul(std::mem::size_of::<Value>() as u64),
        ))?;
        let items = self.snapshot(ex)?;
        let mut text = Vec::new();
        for (i, item) in items.iter().enumerate() {
            let live = (items.len() as u64)
                .saturating_mul(std::mem::size_of::<Rc<ValueListItem>>() as u64)
                .saturating_add((text.capacity() as u64).saturating_mul(2));
            let shown = item.text(ex, live)?;
            let extra = shown.len().saturating_add(if i == 0 { 0 } else { 2 });
            ex.charge(extra as u64 / 64)?;
            let next_len = text.len().saturating_add(extra);
            let allocation = if next_len > text.capacity() {
                (next_len as u64).saturating_mul(2)
            } else {
                0
            };
            ex.reserve(
                inherited
                    .saturating_add(live)
                    .saturating_add((shown.len() as u64).saturating_mul(2))
                    .saturating_add(allocation),
            )?;
            text.try_reserve_exact(extra)
                .map_err(|_| ex.out_of_memory())?;
            if i != 0 {
                text.extend_from_slice(&[u16::from(b';'), u16::from(b' ')]);
            }
            text.extend_from_slice(shown.units());
        }
        Ok(Some(Str::from_vec(text)))
    }
    fn boost_hash(&self) -> Option<i32> {
        Some(self.hash)
    }
}

impl HostObject for ValueListItem {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(idx("ValueListItem"))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let data = self.data.borrow();
        Ok(match property_name(idx("ValueListItem"), m) {
            Some("Value") => clone_value(ex, &data.value)?,
            Some("Presentation") => Value::String(data.presentation.clone()),
            Some("Check") => Value::Bool(data.check),
            Some("Picture") => clone_value(ex, &data.picture)?,
            _ => return Err(not_implemented(idx("ValueListItem"), m).into()),
        })
    }
    fn set(&self, ex: &mut Execution, m: MemberId, value: Value) -> CallResult<()> {
        let revision = self.revision.get();
        let (value, property) = match property_name(idx("ValueListItem"), m) {
            Some("Value") => (adjust(ex, &self.policy, value, 0)?, "Value"),
            Some("Presentation") => (
                Value::String(guarded_present(ex, &value, 0)?),
                "Presentation",
            ),
            Some("Check") => (Value::Bool(bool_value(ex, &value)?), "Check"),
            Some("Picture") => (value, "Picture"),
            _ => return Err(not_implemented(idx("ValueListItem"), m).into()),
        };
        if self.revision.get() != revision {
            return Err(reentrant().into());
        }
        let old = {
            let mut data = self.data.borrow_mut();
            match (property, value) {
                ("Value", v) => std::mem::replace(&mut data.value, v),
                ("Picture", v) => std::mem::replace(&mut data.picture, v),
                ("Presentation", Value::String(s)) => {
                    data.presentation = s;
                    Value::Undefined
                }
                ("Check", Value::Bool(b)) => {
                    data.check = b;
                    Value::Undefined
                }
                _ => Value::Undefined,
            }
        };
        bump(&self.revision);
        release(vec![old]);
        Ok(())
    }
    fn call(&self, _: &mut Execution, m: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        match method_name(idx("ValueListItem"), m) {
            Some("GetID") => Ok(Value::from(self.id as i64)),
            _ => Err(not_implemented(idx("ValueListItem"), m).into()),
        }
    }
    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<Str>> {
        let _guard = PresentGuard::enter()?;
        Ok(Some(self.text(ex, 0)?))
    }
    fn boost_hash(&self) -> Option<i32> {
        Some(self.hash)
    }
}

pub(super) fn ctor(_: &mut Execution, _: &mut [Arg]) -> CallResult<Value> {
    Ok(Value::Object(Rc::new(ValueList::empty())))
}

pub fn new_value_list(items: Vec<(Value, String, bool)>) -> Value {
    let list = ValueList::empty();
    let mut result = Vec::with_capacity(items.len());
    for (i, (value, text, check)) in items.into_iter().enumerate() {
        result.push(ValueListItem::new(
            ItemData {
                value,
                presentation: Str::from(text),
                check,
                picture: Value::Undefined,
            },
            list.policy.clone(),
            list.revision.clone(),
            i as u64,
        ));
    }
    list.next_id.set(result.len() as u64);
    *list.items.borrow_mut() = result;
    Value::Object(Rc::new(list))
}

pub fn value_list_items(value: &Value) -> Option<Vec<(Value, String, bool)>> {
    let list = ValueList::of(value)?;
    Some(
        list.items
            .borrow()
            .iter()
            .map(|item| {
                let data = item.data.borrow();
                (
                    data.value.clone(),
                    data.presentation.to_string_lossy(),
                    data.check,
                )
            })
            .collect(),
    )
}
