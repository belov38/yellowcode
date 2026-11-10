use super::value_list::arg_value_cached;
use super::*;
use crate::bind::{type_mismatch, value_mismatch};
use bsl_prim::{Str, fold_name};
use bsl_vm::{Arg, CallResult, HostObject, Lang, MemberKind};
use std::mem::size_of;

struct Write {
    name: String,
    member: bsl_vm::MemberId,
    stamp: [u64; 2],
    value: Value,
    extra: Extra,
}

enum Extra {
    None,
    Column(String),
    List(Option<value_list::PreparedType>),
}

#[derive(Clone, Copy)]
enum Target<'a> {
    Structure(&'a structure::Structure),
    Table(&'a value_table::row::TableRow),
    Tree(&'a value_tree::row::TreeRow),
    Item(&'a value_list::ValueListItem),
    Column(&'a columns::Column),
    List(&'a value_list::ValueList),
}

impl<'a> Target<'a> {
    fn of(value: &'a Value) -> Option<Self> {
        let any = value.as_object()?.as_any();
        if let Some(value) = any.downcast_ref::<structure::Structure>() {
            Some(Self::Structure(value))
        } else if let Some(value) = any.downcast_ref::<value_table::row::TableRow>() {
            Some(Self::Table(value))
        } else if let Some(value) = any.downcast_ref::<value_tree::row::TreeRow>() {
            Some(Self::Tree(value))
        } else if let Some(value) = any.downcast_ref::<value_list::ValueListItem>() {
            Some(Self::Item(value))
        } else if let Some(value) = any.downcast_ref::<columns::Column>() {
            Some(Self::Column(value))
        } else {
            any.downcast_ref::<value_list::ValueList>().map(Self::List)
        }
    }

    fn prepare(
        self,
        ex: &mut Execution,
        member: bsl_vm::MemberId,
        value: Value,
        retained: u64,
    ) -> CallResult<(Value, [u64; 2], Extra)> {
        if let Self::Column(target) = self {
            return target
                .prepare_fill(ex, member, value, retained)
                .map(|(value, stamp, folded)| (value, stamp, Extra::Column(folded)));
        }
        if let Self::List(target) = self {
            return target
                .prepare_fill(ex, member, value, retained)
                .map(|(value, stamp, prepared)| (value, stamp, Extra::List(prepared)));
        }
        match self {
            Self::Structure(target) => target.prepare_fill(ex, member, value, retained),
            Self::Table(target) => target.prepare_fill(ex, member, value, retained),
            Self::Tree(target) => target.prepare_fill(ex, member, value, retained),
            Self::Item(target) => target.prepare_fill(ex, member, value, retained),
            Self::Column(_) | Self::List(_) => unreachable!(),
        }
        .map(|(value, stamp)| (value, stamp, Extra::None))
    }

    fn check(self, write: &Write) -> CallResult<()> {
        match self {
            Self::Structure(target) => target.check_fill(write.member, write.stamp),
            Self::Table(target) => target.check_fill(write.member, write.stamp),
            Self::Tree(target) => target.check_fill(write.member, write.stamp),
            Self::Item(target) => target.check_fill(write.member, write.stamp),
            Self::Column(target) => target.check_fill(write.member, write.stamp),
            Self::List(target) => target.check_fill(write.member, write.stamp),
        }
    }

    fn publish(self, write: &mut Write) {
        match self {
            Self::Structure(target) => target.publish_fill(write.member, &mut write.value),
            Self::Table(target) => target.publish_fill(write.member, &mut write.value),
            Self::Tree(target) => target.publish_fill(write.member, &mut write.value),
            Self::Item(target) => target.publish_fill(write.member, &mut write.value),
            Self::Column(target) => {
                if let Extra::Column(folded) = &mut write.extra {
                    target.publish_fill(write.member, &mut write.value, folded);
                } else {
                    unreachable!()
                }
            }
            Self::List(target) => {
                if let Extra::List(prepared) = &mut write.extra {
                    target.publish_fill(write.member, &mut write.value, prepared);
                } else {
                    unreachable!()
                }
            }
        }
    }
}

fn live(
    ex: &mut Execution,
    args: &[Arg],
    cached: &[Value; 4],
    names: &Vec<Str>,
    writes: &Vec<Write>,
    current: &Value,
    scratch: u64,
) -> CallResult<u64> {
    let scratch = scratch
        .saturating_add((names.capacity() as u64).saturating_mul(size_of::<Str>() as u64))
        .saturating_add((writes.capacity() as u64).saturating_mul(size_of::<Write>() as u64))
        .saturating_add(writes.iter().fold(0u64, |sum, write| {
            sum.saturating_add(write.name.capacity() as u64)
        }));
    let bytes = ex.measure_retained_lenient(scratch, |visit| {
        for arg in args {
            if let Arg::Val(value) = arg {
                visit.value(value)?;
            }
        }
        for value in cached {
            visit.value(value)?;
        }
        visit.value(current)?;
        for write in writes {
            visit.value(&write.value)?;
            match &write.extra {
                Extra::Column(folded) => visit.bytes(folded.capacity() as u64)?,
                Extra::List(Some(prepared)) => prepared.retained(visit)?,
                _ => (),
            }
        }
        for name in names {
            visit.string(name)?;
        }
        Ok(())
    })?;
    let total = bytes.saturating_add(scratch);
    if total == u64::MAX {
        return Err(ex.out_of_memory().into());
    }
    ex.reserve(total)?;
    Ok(total)
}

fn field(ex: &mut Execution, object: &Value, name: &str) -> CallResult<Option<(bool, bool)>> {
    let Some(object) = object.as_object() else {
        return Ok(None);
    };
    let descriptor = ex
        .runtime()
        .types()
        .member(HostObject::type_id(&**object), name);
    let kind = match descriptor {
        Some((_, kind @ MemberKind::Property { .. })) => kind,
        _ => {
            let Some(member) = object.find_member_guarded(ex, name)? else {
                return Ok(None);
            };
            object.member_kind(member).unwrap_or(MemberKind::Property {
                readable: true,
                writable: true,
            })
        }
    };
    Ok(match kind {
        MemberKind::Property { readable, writable } => Some((readable, writable)),
        MemberKind::Method { .. } => None,
    })
}

fn property(ex: &mut Execution, object: &Value, name: &str, write: bool) -> CallResult<bool> {
    Ok(field(ex, object, name)?
        .is_some_and(|(readable, writable)| if write { writable } else { readable }))
}

fn lacks(ex: &Execution, param: usize, name: &str, object: &Value, read_only: bool) -> HostError {
    let kind = object.as_object().map_or("", |object| {
        ex.runtime()
            .types()
            .presentation(HostObject::type_id(&**object), ex.lang())
    });
    let text = match (ex.lang(), read_only) {
        (Lang::Ru, false) => format!("Поле '{name}' объекта '{kind}' не обнаружено"),
        (Lang::Ru, true) => format!("Поле '{name}' объекта '{kind}' недоступно для записи"),
        (Lang::En, false) => format!("Field '{name}' not found in object '{kind}'"),
        (Lang::En, true) => format!("Field '{name}' not writable in object '{kind}'"),
    };
    value_mismatch(param).with_arg(text)
}

fn refusal(
    ex: &mut Execution,
    source: &Value,
    receiver: &Value,
    name: &str,
) -> CallResult<Option<HostError>> {
    match field(ex, source, name)? {
        None => return Ok(Some(lacks(ex, 3, name, source, false))),
        Some((false, _)) => {
            return Ok(Some(HostError::not_implemented(
                "ЗаполнитьЗначенияСвойств",
                "поле источника недоступно для чтения",
            )));
        }
        Some(_) => {}
    }
    Ok(match field(ex, receiver, name)? {
        None => Some(lacks(ex, 3, name, receiver, false)),
        Some((_, false)) => Some(lacks(ex, 3, name, receiver, true)),
        Some(_) => None,
    })
}

fn check_exclusions(
    ex: &mut Execution,
    list: &Str,
    source: &Value,
    receiver: &Value,
    retained: u64,
) -> CallResult<()> {
    ex.charge(list.len() as u64 + 1)?;
    ex.reserve(
        retained
            .saturating_add((list.len() as u64).saturating_mul(8))
            .saturating_add(2048),
    )?;
    let text = list.to_string_lossy();
    for name in text
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        for object in [source, receiver] {
            if field(ex, object, name)?.is_none() {
                return Err(lacks(ex, 4, name, object, false).into());
            }
        }
    }
    Ok(())
}

const LISTED: usize = 32;

fn walk_order(ex: &mut Execution, names: &mut Vec<Str>, retained: u64) -> CallResult<()> {
    ex.charge((names.len() as u64).saturating_mul(LISTED as u64 + 1))?;
    let total = names.iter().map(|name| name.len() as u64).sum::<u64>();
    ex.reserve(
        retained
            .saturating_add(total.saturating_mul(8))
            .saturating_add(4096),
    )?;
    let mut distinct: Vec<&Str> = Vec::new();
    for name in names.iter() {
        if distinct.contains(&name) {
            continue;
        }
        if distinct.len() == LISTED {
            return Err(HostError::not_implemented(
                "ЗаполнитьЗначенияСвойств",
                "список длиннее 32 имён",
            )
            .into());
        }
        distinct.push(name);
    }
    let hashes: Vec<usize> = distinct
        .iter()
        .map(|name| {
            name.to_string_lossy()
                .to_lowercase()
                .encode_utf16()
                .fold(0, |h, unit| (h * 37 + usize::from(unit)) % 64)
        })
        .collect();
    let mut walk: Vec<usize> = Vec::new();
    let mut buckets = 8;
    let place = |walk: &mut Vec<usize>, i: usize, buckets: usize| {
        let at = walk
            .iter()
            .position(|&j| hashes[j] % buckets == hashes[i] % buckets);
        walk.insert(at.unwrap_or(walk.len()), i);
    };
    for i in 0..distinct.len() {
        place(&mut walk, i, buckets);
        while walk.len() > buckets {
            buckets *= 8;
            for j in std::mem::take(&mut walk) {
                place(&mut walk, j, buckets);
            }
        }
    }
    let ordered: Vec<Str> = walk.into_iter().map(|i| distinct[i].clone()).collect();
    *names = ordered;
    Ok(())
}

fn contains(ex: &mut Execution, list: &Str, name: &str, retained: u64) -> CallResult<bool> {
    let units = (list.len() as u64).saturating_add(name.len() as u64);
    ex.charge(units.saturating_add(1))?;
    ex.reserve(
        retained
            .saturating_add(units.saturating_mul(32))
            .saturating_add(2048),
    )?;
    let name = fold_name(name);
    Ok(list
        .to_string_lossy()
        .split(',')
        .any(|part| fold_name(part.trim()) == name))
}

fn inventory(
    ex: &mut Execution,
    receiver: &Value,
    source: &Value,
    retained: u64,
) -> CallResult<Vec<Str>> {
    let object = receiver.as_object().ok_or_else(|| type_mismatch(1))?;
    if let Some(names) = object.property_names(ex, retained)? {
        return Ok(names);
    }
    if let Some(source) = source.as_object()
        && let Some(names) = source.property_names(ex, retained)?
    {
        return Ok(names);
    }
    let Some(t) = ex.runtime().types().catalog_type(receiver.type_id()) else {
        return Ok(Vec::new());
    };
    let properties = &catalog().types()[usize::from(t.0)].properties;
    let bytes = properties.iter().filter(|p| p.writable).fold(0u64, |n, p| {
        n.saturating_add(
            (p.name_ru.len() as u64)
                .saturating_mul(2)
                .saturating_add(64),
        )
    });
    let count = properties.iter().filter(|p| p.writable).count() as u64;
    ex.charge(properties.len() as u64)?;
    let live = retained
        .saturating_add(bytes)
        .saturating_add(count.saturating_mul(size_of::<Str>() as u64));
    ex.reserve(live)?;
    let mut names = vec_with_capacity(ex, count)?;
    ex.reserve(
        live.saturating_add((names.capacity() as u64).saturating_mul(size_of::<Str>() as u64)),
    )?;
    names.extend(
        properties
            .iter()
            .filter(|p| p.writable)
            .map(|p| Str::from(p.name_ru.as_str())),
    );
    Ok(names)
}

pub(super) fn fill(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    let mut cached = std::array::from_fn(|_| Value::Undefined);
    let mut names = Vec::new();
    let mut writes = Vec::new();
    for index in 0..4 {
        let retained = live(ex, args, &cached, &names, &writes, &Value::Undefined, 0)?;
        let result = arg_value_cached(ex, args.get(index), retained);
        if let Ok(value) = result.as_ref() {
            live(ex, args, &cached, &names, &writes, value, 0)?;
        }
        cached[index] = result?;
    }
    for (index, value) in cached.iter().take(2).enumerate() {
        if !matches!(value, Value::Object(_)) {
            return Err(value_mismatch(index + 1).into());
        }
    }
    for (index, value) in cached.iter().enumerate().skip(2) {
        if !matches!(value, Value::String(_) | Value::Undefined) {
            return Err(type_mismatch(index + 1).into());
        }
    }
    let retained = live(ex, args, &cached, &names, &writes, &Value::Undefined, 0)?;
    let listed = matches!(&cached[2], Value::String(_));
    names = match &cached[2] {
        Value::String(list) => {
            ex.charge(list.len() as u64 + 1)?;
            let count = list
                .units()
                .iter()
                .filter(|u| **u == u16::from(b','))
                .count() as u64
                + 1;
            let extra = (list.len() as u64)
                .saturating_mul(16)
                .saturating_add(count.saturating_mul(128));
            ex.reserve(retained.saturating_add(extra))?;
            let mut selected = vec_with_capacity(ex, count)?;
            ex.reserve(retained.saturating_add(extra).saturating_add(
                (selected.capacity() as u64).saturating_mul(size_of::<Str>() as u64),
            ))?;
            let text = list.to_string_lossy();
            selected.extend(
                text.split(',')
                    .map(str::trim)
                    .filter(|part| !part.is_empty())
                    .map(Str::from),
            );
            selected
        }
        _ => inventory(ex, &cached[0], &cached[1], retained)?,
    };
    if listed {
        walk_order(ex, &mut names, retained)?;
    } else if let Value::String(list) = &cached[3] {
        check_exclusions(ex, list, &cached[1], &cached[0], retained)?;
    }
    live(ex, args, &cached, &names, &writes, &Value::Undefined, 0)?;
    let retained = live(ex, args, &cached, &names, &writes, &Value::Undefined, 0)?;
    ex.reserve(
        retained.saturating_add((names.len() as u64).saturating_mul(size_of::<Write>() as u64)),
    )?;
    writes = vec_with_capacity(ex, names.len() as u64)?;
    let mut pending = None;
    for name in &names {
        let retained = live(ex, args, &cached, &names, &writes, &Value::Undefined, 0)?;
        let scratch = (name.len() as u64).saturating_mul(4).saturating_add(64);
        ex.reserve(retained.saturating_add(scratch))?;
        ex.charge(name.len() as u64 + 1)?;
        let name = name.to_string_lossy();
        if let Value::String(list) = &cached[2]
            && !list.is_empty()
            && !contains(ex, list, &name, retained.saturating_add(scratch))?
        {
            continue;
        }
        if !listed
            && let Value::String(list) = &cached[3]
            && contains(ex, list, &name, retained.saturating_add(scratch))?
        {
            continue;
        }
        if listed {
            let refused = refusal(ex, &cached[1], &cached[0], &name);
            live(
                ex,
                args,
                &cached,
                &names,
                &writes,
                &Value::Undefined,
                scratch,
            )?;
            if let Some(error) = refused? {
                pending = Some(error);
                break;
            }
        }
        let writable = property(ex, &cached[0], &name, true);
        live(
            ex,
            args,
            &cached,
            &names,
            &writes,
            &Value::Undefined,
            scratch,
        )?;
        if !writable? {
            continue;
        }
        let readable = property(ex, &cached[1], &name, false);
        let retained = live(
            ex,
            args,
            &cached,
            &names,
            &writes,
            &Value::Undefined,
            scratch,
        )?;
        if !readable? {
            continue;
        }
        let result = ex.get_member_with_retained(&cached[1], &name, retained);
        live(
            ex,
            args,
            &cached,
            &names,
            &writes,
            result.as_ref().unwrap_or(&Value::Undefined),
            scratch,
        )?;
        let value = result?;
        writes.push(Write {
            name,
            value,
            member: bsl_vm::MemberId(0),
            stamp: [0; 2],
            extra: Extra::None,
        });
    }
    if writes.is_empty() {
        return pending.map_or(Ok(Value::Undefined), |error| Err(error.into()));
    }
    if let Some(target) = Target::of(&cached[0]) {
        for index in 0..writes.len() {
            let retained = live(ex, args, &cached, &names, &writes, &Value::Undefined, 0)?;
            let value = std::mem::take(&mut writes[index].value);
            let prepared =
                ex.prepare_member_write(&cached[0], &writes[index].name, |ex, member| {
                    let (value, stamp, extra) = target.prepare(ex, member, value, retained)?;
                    Ok((member, value, stamp, extra))
                });
            match prepared {
                Ok((member, value, stamp, extra)) => {
                    writes[index].member = member;
                    writes[index].value = value;
                    writes[index].stamp = stamp;
                    writes[index].extra = extra;
                    live(ex, args, &cached, &names, &writes, &Value::Undefined, 0)?;
                }
                Err(error) => {
                    live(ex, args, &cached, &names, &writes, &Value::Undefined, 0)?;
                    return Err(error.into());
                }
            }
        }
        for write in &writes {
            target.check(write)?;
        }
        ex.with_reuse_check(|_| Ok(()))?;
        let retained = live(ex, args, &cached, &names, &writes, &Value::Undefined, 0)?;
        let mut retired = super::retire::prepare(ex, retained, |visit| {
            visit(&cached[0])?;
            for write in &writes {
                visit(&write.value)?;
                if let Extra::List(Some(prepared)) = &write.extra {
                    prepared.visit_values(visit)?;
                }
            }
            Ok(())
        })?;
        live(
            ex,
            args,
            &cached,
            &names,
            &writes,
            &Value::Undefined,
            (retired.capacity() as u64).saturating_mul(size_of::<Value>() as u64),
        )?;
        for write in &writes {
            target.check(write)?;
        }
        for write in &mut writes {
            target.publish(write);
        }
        ex.mark_retained_mutation();
        for write in &mut writes {
            super::retire::push(&mut retired, std::mem::take(&mut write.value));
            if let Extra::List(Some(prepared)) = std::mem::replace(&mut write.extra, Extra::None) {
                prepared.retire(&mut retired);
            }
        }
        super::retire::run(retired);
    } else {
        for write in writes {
            ex.set_member(&cached[0], &write.name, write.value)?;
        }
    }
    pending.map_or(Ok(Value::Undefined), |error| Err(error.into()))
}
