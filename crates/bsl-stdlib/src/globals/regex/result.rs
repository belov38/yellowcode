use crate::collections::{method_name, new_array, not_implemented, property_name};
use crate::globals::int_value;
use bsl_prim::Str;
use bsl_vm::{
    Arg, CallResult, Exception, Execution, HostObject, MemberId, RetainedMemory, TypeId, TypeIdx,
    Value,
};
use icu46_sys::regex::Match;
use std::{any::Any, mem::size_of, rc::Rc};

struct ResultValue {
    group: bool,
    position: usize,
    length: usize,
    value: Str,
    groups: Vec<Value>,
}

fn ty(group: bool) -> TypeIdx {
    crate::typedesc::idx(if group {
        "ResultOfSearchByRegularExpressionGroup"
    } else {
        "ResultOfSearchByRegularExpression"
    })
}

impl HostObject for ResultValue {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(ty(self.group))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, _: &mut Execution, member: MemberId) -> CallResult<Value> {
        Ok(match property_name(ty(self.group), member) {
            Some("StartIndex") => int_value(self.position as i64),
            Some("Length") => int_value(self.length as i64),
            Some("Value") => Value::String(self.value.clone()),
            _ => return Err(not_implemented(ty(self.group), member).into()),
        })
    }
    fn set(&self, _: &mut Execution, member: MemberId, _: Value) -> CallResult<()> {
        Err(not_implemented(ty(self.group), member).into())
    }
    fn call(&self, ex: &mut Execution, member: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        if !self.group && method_name(ty(false), member) == Some("GetGroups") {
            crate::collections::reserve_values(ex, self.groups.len() as u64)?;
            ex.charge(self.groups.len() as u64)?;
            return Ok(new_array(self.groups.clone()));
        }
        Err(not_implemented(ty(self.group), member).into())
    }
    fn presentation(&self, _: &mut Execution) -> CallResult<Option<Str>> {
        Ok((!self.group).then(|| Str::from("РезультатПоискаПоРегулярномуВыражению")))
    }
    fn retained_memory(&self, visit: &mut dyn RetainedMemory) -> Result<bool, Exception> {
        visit.string(&self.value)?;
        visit.bytes((self.groups.capacity() * size_of::<Value>()) as u64)?;
        for value in &self.groups {
            visit.value(value)?;
        }
        Ok(true)
    }
}

fn node_bytes(units: usize) -> u64 {
    crate::collections::accounting::retained_rc_size::<ResultValue>()
        .saturating_add(crate::collections::accounting::retained_rc_size::<Vec<u16>>())
        .saturating_add((units as u64).saturating_mul(2))
}

pub(super) fn make(
    ex: &mut Execution,
    text: &Str,
    found: Option<Match>,
    live: &mut u64,
) -> CallResult<Value> {
    let mut bytes = node_bytes(found.as_ref().map_or(0, |m| m.end - m.start));
    if let Some(m) = &found {
        bytes = bytes.saturating_add((m.groups.len() * size_of::<Value>()) as u64);
        for group in &m.groups {
            bytes = bytes.saturating_add(node_bytes(group.map_or(0, |(a, b)| b - a)));
        }
        ex.charge(m.groups.len() as u64)?;
    }
    ex.charge(bytes / 128 + 1)?;
    let scratch = found.as_ref().map_or(0, |m| {
        (m.groups.capacity() * size_of::<Option<(usize, usize)>>()) as u64
    });
    ex.reserve(live.saturating_add(bytes).saturating_add(scratch))?;
    let (position, value, groups) = if let Some(m) = found {
        let mut groups = Vec::new();
        groups
            .try_reserve_exact(m.groups.len())
            .map_err(|_| ex.out_of_memory())?;
        for range in m.groups {
            let (position, units) =
                range.map_or((0, &[][..]), |(a, b)| (a + 1, &text.units()[a..b]));
            groups.push(Value::Object(Rc::new(ResultValue {
                group: true,
                position,
                length: units.len(),
                value: Str::from_units(units),
                groups: Vec::new(),
            })));
        }
        (
            m.start + 1,
            Str::from_units(&text.units()[m.start..m.end]),
            groups,
        )
    } else {
        (0, Str::new(), Vec::new())
    };
    *live = live.saturating_add(bytes);
    Ok(Value::Object(Rc::new(ResultValue {
        group: false,
        position,
        length: value.len(),
        value,
        groups,
    })))
}
