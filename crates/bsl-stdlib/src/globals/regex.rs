mod cache;
mod result;

use super::{enum_arg, reserve_units, str_value};
use crate::bind::{arg_bool, arg_i64, arg_str, global, is_given, key_error, value_mismatch};
use crate::collections::new_array;
use bsl_prim::Str;
use bsl_vm::{Arg, CallError, CallResult, Execution, Registry, Value};
use icu46_sys::regex::{Regex, RegexError, RegexFlags};
use std::mem::size_of;
use std::sync::atomic::Ordering;

pub(super) fn register(reg: &mut Registry) {
    global(
        reg,
        "СтрНайтиПоРегулярномуВыражению",
        "StrFindByRegularExpression",
        find,
    );
    global(
        reg,
        "СтрНайтиВсеПоРегулярномуВыражению",
        "StrFindAllByRegularExpression",
        find_all,
    );
    global(
        reg,
        "СтрЗаменитьПоРегулярномуВыражению",
        "StrReplaceByRegularExpression",
        replace,
    );
    global(
        reg,
        "СтрПодобнаПоРегулярномуВыражению",
        "StrLikeByRegularExpression",
        like,
    );
}

fn native_error(ex: &Execution, pattern: &Str, error: RegexError) -> CallError {
    use icu46_sys::managed::{
        FailureCause,
        runtime::{NativeCause, quarantined},
    };
    if let Some(first) = quarantined() {
        return ex
            .collation_error(bsl_prim::CollationError::Restart {
                memory: matches!(
                    first.cause,
                    NativeCause::Allocation(FailureCause::MemoryLimit(_))
                ),
            })
            .into();
    }
    if error.code == 0x10313 {
        return ex.cancellation_error().into();
    }
    if error.code == 7 {
        return ex.out_of_memory().into();
    }
    key_error(
        "core85:IDS_REGEX_ERROR",
        vec![format!("{:x}", error.code), error.name.into()],
        format!("\n{}", pattern.to_string_lossy()),
    )
    .into()
}

fn flags(ex: &mut Execution, args: &[Arg], offset: usize) -> CallResult<RegexFlags> {
    let mut boolean = |index| {
        if is_given(args.get(index)) {
            arg_bool(ex, args.get(index))
        } else {
            Ok(false)
        }
    };
    Ok(RegexFlags {
        case_insensitive: boolean(offset)?,
        multiline: boolean(offset + 1)?,
    })
}

fn stack_limit(ex: &Execution) -> i32 {
    ex.options().memory_limit.map_or(16 * 1024 * 1024, |limit| {
        (limit / 4).clamp(1, 16 * 1024 * 1024)
    }) as i32
}

fn limits(ex: &mut Execution, regex: &mut Regex, pattern: &Str) -> CallResult<()> {
    let work = ex.remaining_budget().unwrap_or(640).min(640);
    ex.charge(work.max(1))?;
    let time = (work / 64).max(1) as i32;
    regex
        .set_limits(time, stack_limit(ex))
        .map_err(|e| native_error(ex, pattern, e))
}

fn replacement_cancel(ex: &Execution) -> CallResult<()> {
    if ex
        .options()
        .cancel
        .as_ref()
        .is_some_and(|c| c.load(Ordering::Relaxed))
    {
        return Err(ex.cancellation_error().into());
    }
    Ok(())
}

fn replacement_work(
    ex: &mut Execution,
    text: &Str,
    replacement: &Str,
    output: usize,
) -> CallResult<()> {
    replacement_cancel(ex)?;
    let work = (text.len() as u64)
        .saturating_add(1)
        .saturating_mul((replacement.len() as u64).saturating_add(1))
        .saturating_mul(8)
        .saturating_add(text.len() as u64)
        .saturating_add((output as u64).saturating_mul(2));
    ex.charge(work / 64 + 1)?;
    Ok(())
}

fn input(ex: &mut Execution, regex: &mut Regex, text: &Str, pattern: &Str) -> CallResult<u64> {
    let live = (stack_limit(ex) as u64).saturating_add(
        (text.len() as u64)
            .saturating_add(1)
            .saturating_mul(2)
            .saturating_add(
                (pattern.len() as u64).saturating_mul(size_of::<Option<(usize, usize)>>() as u64),
            ),
    );
    ex.reserve(live)?;
    ex.charge(text.len() as u64 / 64 + 1)?;
    regex
        .set_input(text.units())
        .map_err(|e| native_error(ex, pattern, e))?;
    Ok(live)
}

fn like(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    let text = arg_str(ex, args.first())?;
    let pattern = arg_str(ex, args.get(1))?;
    let flags = flags(ex, args, 2)?;
    if pattern.is_empty() {
        return Ok(Value::Bool(text.is_empty()));
    }
    let mut regex = cache::matcher(ex, &pattern, flags)?;
    ex.reserve(
        (text.len() as u64 + 1)
            .saturating_mul(2)
            .saturating_add(stack_limit(ex) as u64),
    )?;
    ex.charge(text.len() as u64 / 64 + 1)?;
    limits(ex, &mut regex, &pattern)?;
    let matched = regex
        .matches(text.units())
        .map_err(|e| native_error(ex, &pattern, e))?;
    Ok(Value::Bool(matched))
}

fn find(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    let text = arg_str(ex, args.first())?;
    let pattern = arg_str(ex, args.get(1))?;
    let reverse = enum_arg(ex, args.get(2), "НаправлениеПоиска", 3)? == Some("СКонца");
    let start = if is_given(args.get(3)) {
        let start = arg_i64(ex, args.get(3), 4)?;
        if start < 1 || start as u64 > text.len() as u64 {
            return Err(value_mismatch(4).into());
        }
        start as usize - 1
    } else if reverse {
        text.len().saturating_sub(1)
    } else {
        0
    };
    let occurrence = if is_given(args.get(4)) {
        arg_i64(ex, args.get(4), 5)?
    } else {
        1
    };
    if occurrence < 1 {
        return Err(value_mismatch(5).into());
    }
    let flags = flags(ex, args, 5)?;
    let mut live = 0;
    if pattern.is_empty() {
        return result::make(ex, &text, None, &mut live);
    }
    let mut regex = cache::matcher(ex, &pattern, flags)?;
    live = input(ex, &mut regex, &text, &pattern)?;
    let mut selected = None;
    let mut remaining = occurrence;
    if reverse {
        let mut boundary = text.len();
        for position in (0..=start).rev() {
            limits(ex, &mut regex, &pattern)?;
            if let Some(found) = regex
                .match_from(position)
                .map_err(|e| native_error(ex, &pattern, e))?
            {
                if found.end > boundary {
                    continue;
                }
                remaining -= 1;
                boundary = found.start;
                if remaining == 0 {
                    selected = Some(found);
                    break;
                }
            }
        }
    } else {
        limits(ex, &mut regex, &pattern)?;
        let mut found = regex
            .find_from(start)
            .map_err(|e| native_error(ex, &pattern, e))?;
        while let Some(current) = found {
            remaining -= 1;
            if remaining == 0 {
                selected = Some(current);
                break;
            }
            limits(ex, &mut regex, &pattern)?;
            found = regex
                .find_next()
                .map_err(|e| native_error(ex, &pattern, e))?;
        }
    }
    result::make(ex, &text, selected, &mut live)
}

fn find_all(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    let text = arg_str(ex, args.first())?;
    let pattern = arg_str(ex, args.get(1))?;
    let flags = flags(ex, args, 2)?;
    let mut values = Vec::new();
    if pattern.is_empty() {
        return Ok(new_array(values));
    }
    let mut regex = cache::matcher(ex, &pattern, flags)?;
    let mut live = input(ex, &mut regex, &text, &pattern)?;
    limits(ex, &mut regex, &pattern)?;
    let mut found = regex
        .find_from(0)
        .map_err(|e| native_error(ex, &pattern, e))?;
    while let Some(current) = found {
        if values.len() == values.capacity() {
            let capacity = values.capacity().saturating_mul(2).max(4);
            crate::collections::reserve_values(ex, capacity as u64)?;
            let extra = (capacity * size_of::<Value>()) as u64;
            ex.reserve(live.saturating_add(extra))?;
            values
                .try_reserve_exact(capacity - values.len())
                .map_err(|_| ex.out_of_memory())?;
            live = live.saturating_add(extra);
        }
        values.push(result::make(ex, &text, Some(current), &mut live)?);
        limits(ex, &mut regex, &pattern)?;
        found = regex
            .find_next()
            .map_err(|e| native_error(ex, &pattern, e))?;
    }
    Ok(new_array(values))
}

fn replace(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    let text = arg_str(ex, args.first())?;
    let pattern = arg_str(ex, args.get(1))?;
    let replacement = arg_str(ex, args.get(2))?;
    let flags = flags(ex, args, 3)?;
    if pattern.is_empty() {
        let length = text.len().saturating_add(
            text.len()
                .saturating_add(1)
                .saturating_mul(replacement.len()),
        );
        replacement_work(ex, &text, &replacement, length)?;
        reserve_units(ex, length, 1)?;
        let mut output = Vec::new();
        output
            .try_reserve_exact(length)
            .map_err(|_| ex.out_of_memory())?;
        output.extend_from_slice(replacement.units());
        for unit in text.units() {
            output.push(*unit);
            output.extend_from_slice(replacement.units());
        }
        replacement_cancel(ex)?;
        return Ok(str_value(output));
    }
    let mut regex = cache::matcher(ex, &pattern, flags)?;
    let live = input(ex, &mut regex, &text, &pattern)?;
    replacement_work(ex, &text, &replacement, 0)?;
    limits(ex, &mut regex, &pattern)?;
    let length = regex
        .replacement_length(replacement.units())
        .map_err(|e| native_error(ex, &pattern, e))?;
    replacement_cancel(ex)?;
    let capacity = length.checked_add(1).ok_or_else(|| ex.out_of_memory())?;
    ex.reserve(live.saturating_add((capacity as u64).saturating_mul(2)))?;
    replacement_work(ex, &text, &replacement, capacity)?;
    let mut output = Vec::new();
    output
        .try_reserve_exact(capacity)
        .map_err(|_| ex.out_of_memory())?;
    output.resize(capacity, 0);
    limits(ex, &mut regex, &pattern)?;
    let written = regex
        .replace_into(replacement.units(), &mut output)
        .map_err(|e| native_error(ex, &pattern, e))?;
    replacement_cancel(ex)?;
    output.truncate(written);
    Ok(str_value(output))
}
