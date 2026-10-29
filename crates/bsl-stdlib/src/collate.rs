use bsl_prim::Str;
use bsl_vm::{Exception, Execution, HostError};
use icu46_sys::{Collator, IcuStatus, SORT_KEY_SLACK, SortKeyWrite, Strength};
use std::ffi::CStr;
use std::mem::size_of;

const INPUT_LIMIT: usize = 16 << 20;
const WORK_LIMIT: usize = 256 << 20;
const MAX_STRINGS: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollateMode {
    Matrix,
    Sort,
}

#[derive(Default)]
struct Keys {
    collection: Vec<u8>,
    secondary: Vec<u8>,
    tertiary: Vec<u8>,
}

struct Live(usize);
impl Live {
    fn set(&mut self, ex: &mut Execution, total: Option<usize>) -> Result<(), Exception> {
        let total = amount(ex, total)?;
        if total > WORK_LIMIT {
            return Err(ex.out_of_memory());
        }
        ex.reserve(total as u64)?;
        self.0 = total;
        Ok(())
    }
    fn add(&mut self, ex: &mut Execution, bytes: usize) -> Result<(), Exception> {
        self.set(ex, self.0.checked_add(bytes))
    }
    fn replace(&mut self, ex: &mut Execution, old: usize, new: usize) -> Result<(), Exception> {
        self.set(ex, self.0.checked_sub(old).and_then(|n| n.checked_add(new)))
    }
}
fn amount(ex: &Execution, bytes: Option<usize>) -> Result<usize, Exception> {
    bytes.ok_or_else(|| ex.out_of_memory())
}
fn bytes<T>(count: usize) -> Option<usize> {
    count.checked_mul(size_of::<T>())
}
fn kernel_error(ex: &mut Execution) -> Exception {
    ex.error(HostError::text(
        "Ошибка ограниченного сравнения строк",
        "Bounded collation failed",
    ))
}

macro_rules! trace {
    ($update:expr) => {
        #[cfg(test)]
        TRACE.with(|cell| {
            let mut value = cell.get();
            ($update)(&mut value);
            cell.set(value);
        });
    };
}
fn charge(ex: &mut Execution, units: u64) -> Result<(), Exception> {
    ex.charge(units).inspect_err(|_| {
        trace!(|t: &mut Trace| t.refused = true);
    })?;
    trace!(|t: &mut Trace| t.charged = t.charged.saturating_add(units));
    Ok(())
}

fn work(ex: &mut Execution, units: usize) -> Result<(), Exception> {
    charge(ex, (units / 32) as u64 + 1)
}

#[derive(Clone, Copy)]
enum IcuOp {
    OriginalOpen,
    RootOpen,
    Key,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IcuAction {
    Memory,
    RetryRoot,
    Host,
}
fn icu_action(op: IcuOp, status: IcuStatus) -> IcuAction {
    if status.code == 7 {
        IcuAction::Memory
    } else if matches!(op, IcuOp::OriginalOpen) {
        IcuAction::RetryRoot
    } else {
        IcuAction::Host
    }
}
fn icu_error(ex: &mut Execution, op: IcuOp, status: IcuStatus) -> Exception {
    match icu_action(op, status) {
        IcuAction::Memory => ex.out_of_memory(),
        _ => kernel_error(ex),
    }
}
fn open(ex: &mut Execution, locale: &CStr, strength: Strength) -> Result<Collator, Exception> {
    work(ex, locale.to_bytes().len())?;
    trace!(|t: &mut Trace| t.opens += 1);
    match Collator::try_new_cstr(locale, strength) {
        Ok(collator) => Ok(collator),
        Err(status) => match icu_action(IcuOp::OriginalOpen, status) {
            IcuAction::Memory => Err(ex.out_of_memory()),
            IcuAction::RetryRoot => {
                charge(ex, 1)?;
                trace!(|t: &mut Trace| t.opens += 1);
                Collator::try_new_cstr(c"root", strength)
                    .map_err(|e| icu_error(ex, IcuOp::RootOpen, e))
            }
            IcuAction::Host => Err(kernel_error(ex)),
        },
    }
}

fn reserve_vec<T>(ex: &mut Execution, live: &mut Live, count: usize) -> Result<Vec<T>, Exception> {
    let expected = amount(ex, bytes::<T>(count))?;
    let mut value = Vec::new();
    value
        .try_reserve_exact(count)
        .map_err(|_| ex.out_of_memory())?;
    let actual = amount(ex, bytes::<T>(value.capacity()))?;
    live.replace(ex, expected, actual)?;
    Ok(value)
}
fn key(
    ex: &mut Execution,
    live: &mut Live,
    collator: &Collator,
    text: &Str,
    trim: bool,
) -> Result<Vec<u8>, Exception> {
    work(ex, text.len())?;
    trace!(|t: &mut Trace| {
        if let Some(slot) = t.key_before.get_mut(t.key_starts) {
            *slot = before;
        }
        t.key_starts += 1;
    });
    let mut source = text.units();
    if trim {
        while source.last() == Some(&0x20) {
            source = &source[..source.len() - 1];
        }
    }
    let padded = amount(ex, source.len().checked_add(1))?;
    let padding_bytes = amount(ex, bytes::<u16>(padded))?;
    work(ex, padded)?;
    live.add(ex, padding_bytes)?;
    let mut pad = reserve_vec::<u16>(ex, live, padded)?;
    let actual_padding = amount(ex, bytes::<u16>(pad.capacity()))?;
    trace!(|t: &mut Trace| t.copies += 1);
    pad.extend_from_slice(source);
    pad.push(0);
    work(ex, padded)?;
    let mut probe = [0u8; 1 + SORT_KEY_SLACK];
    trace!(|t: &mut Trace| t.probes += 1);
    let needed = match collator
        .try_sort_key_into(&pad, &mut probe)
        .map_err(|e| icu_error(ex, IcuOp::Key, e))?
    {
        SortKeyWrite::Complete(n) | SortKeyWrite::Required(n) if n > 0 => n,
        _ => return Err(kernel_error(ex)),
    };
    let capacity = amount(ex, needed.checked_add(SORT_KEY_SLACK))?;
    work(ex, capacity)?;
    live.add(ex, capacity)?;
    let mut output = reserve_vec::<u8>(ex, live, capacity)?;
    output.resize(capacity, 0);
    work(ex, padded)?;
    trace!(|t: &mut Trace| t.writes += 1);
    match collator
        .try_sort_key_into(&pad, &mut output)
        .map_err(|e| icu_error(ex, IcuOp::Key, e))?
    {
        SortKeyWrite::Complete(n) if n == needed => {}
        _ => return Err(kernel_error(ex)),
    }
    output.truncate(needed);
    if output.last() != Some(&0) {
        return Err(kernel_error(ex));
    }
    drop(pad);
    live.replace(ex, actual_padding, 0)?;
    trace!(|t: &mut Trace| t.completed_keys += 1);
    Ok(output)
}
fn compare(ex: &mut Execution, left: &[u8], right: &[u8]) -> Result<std::cmp::Ordering, Exception> {
    work(ex, left.len().max(right.len()))?;
    trace!(|t: &mut Trace| {
        if t.comparisons == 0 {
            t.first_compare_before = before;
        }
        t.comparisons += 1;
    });
    Ok(left.cmp(right))
}
fn order(
    ex: &mut Execution,
    keys: &[Keys],
    indices: &mut [usize],
    scratch: &mut [usize],
    descending: bool,
) -> Result<(), Exception> {
    charge(ex, indices.len() as u64 + 1)?;
    let mut error = None;
    let valid = bsl_lib::sort::msvc_sort_with_scratch(indices, scratch, &mut |a, b| {
        if error.is_some() {
            return false;
        }
        match compare(ex, &keys[*a].collection, &keys[*b].collection) {
            Ok(value) => {
                if descending {
                    value.is_gt()
                } else {
                    value.is_lt()
                }
            }
            Err(failure) => {
                error = Some(failure);
                false
            }
        }
    });
    if let Some(error) = error {
        return Err(error);
    }
    if !valid {
        return Err(kernel_error(ex));
    }
    Ok(())
}
fn decimal(output: &mut String, mut n: usize) {
    let mut digits = [0u8; 20];
    let mut start = digits.len();
    loop {
        start -= 1;
        digits[start] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    for digit in &digits[start..] {
        output.push(char::from(*digit));
    }
}
fn emit_order(
    ex: &mut Execution,
    output: &mut String,
    indices: &[usize],
    bound: usize,
) -> Result<(), Exception> {
    work(ex, bound)?;
    trace!(|t: &mut Trace| t.outputs += 1);
    for (position, index) in indices.iter().enumerate() {
        if position != 0 {
            output.push(',');
        }
        decimal(output, *index);
    }
    output.push('|');
    Ok(())
}

#[allow(
    clippy::ptr_arg,
    reason = "original retained capacities are part of admission"
)]
pub fn compute(
    ex: &mut Execution,
    strings: &Vec<Str>,
    locale: &String,
    mode: CollateMode,
) -> Result<String, Exception> {
    charge(ex, 1)?;
    let n = strings.len();
    if n > MAX_STRINGS {
        return Err(ex.out_of_memory());
    }
    charge(ex, n as u64 + (locale.len() / 32) as u64 + 1)?;
    let mut source = amount(
        ex,
        bytes::<Str>(strings.capacity())
            .and_then(|v| v.checked_add(size_of::<Vec<Str>>()))
            .and_then(|v| v.checked_add(size_of::<String>()))
            .and_then(|v| v.checked_add(locale.capacity())),
    )?;
    let mut logical = 0usize;
    for text in strings {
        source = amount(
            ex,
            bytes::<u16>(text.backing_capacity_units())
                .and_then(|v| v.checked_add(size_of::<Vec<u16>>() + 2 * size_of::<usize>()))
                .and_then(|v| v.checked_add(source)),
        )?;
        logical = amount(ex, logical.checked_add(text.len()))?;
    }
    if source > INPUT_LIMIT {
        return Err(ex.out_of_memory());
    }
    work(ex, logical)?;
    work(ex, locale.len())?;
    if locale.as_bytes().contains(&0) {
        return Err(kernel_error(ex));
    }
    let digits = if n <= 10 {
        1
    } else if n <= 100 {
        2
    } else if n <= 1000 {
        3
    } else {
        4
    };
    let order_bound = amount(
        ex,
        n.checked_mul(digits)
            .and_then(|v| v.checked_add(n.saturating_sub(1))),
    )?;
    let output_bound = match mode {
        CollateMode::Matrix => amount(
            ex,
            n.checked_mul(n)
                .and_then(|v| v.checked_mul(2))
                .and_then(|v| order_bound.checked_mul(2).and_then(|b| b.checked_add(v)))
                .and_then(|v| v.checked_add(4))
                .and_then(|v| v.checked_add(locale.len())),
        )?,
        CollateMode::Sort => amount(
            ex,
            order_bound.checked_mul(3).and_then(|v| v.checked_add(3)),
        )?,
    };
    let locale_bound = amount(ex, locale.len().checked_add(1))?;
    let key_headers = amount(ex, bytes::<Keys>(n))?;
    let index_bytes = amount(ex, bytes::<usize>(n).and_then(|v| v.checked_mul(3)))?;
    let standing_headers = size_of::<(
        String,
        Vec<u8>,
        Vec<Keys>,
        Vec<usize>,
        Vec<usize>,
        Vec<usize>,
        Vec<u16>,
        Option<Collator>,
        Option<Collator>,
        [u8; 1 + SORT_KEY_SLACK],
    )>();
    let standing = amount(
        ex,
        output_bound
            .checked_add(locale_bound)
            .and_then(|v| v.checked_add(key_headers))
            .and_then(|v| v.checked_add(index_bytes))
            .and_then(|v| v.checked_add(standing_headers)),
    )?;
    let mut live = Live(source);
    live.add(ex, standing)?;
    work(ex, output_bound)?;
    let mut output = String::new();
    output
        .try_reserve_exact(output_bound)
        .map_err(|_| ex.out_of_memory())?;
    live.replace(ex, output_bound, output.capacity())?;
    work(ex, locale_bound)?;
    let mut locale_bytes = reserve_vec::<u8>(ex, &mut live, locale_bound)?;
    trace!(|t: &mut Trace| t.copies += 1);
    locale_bytes.extend_from_slice(locale.as_bytes());
    locale_bytes.push(0);
    work(ex, locale_bytes.len())?;
    let locale_cstr = CStr::from_bytes_with_nul(&locale_bytes).map_err(|_| kernel_error(ex))?;
    charge(ex, n as u64 + 1)?;
    let mut keys = reserve_vec::<Keys>(ex, &mut live, n)?;
    keys.resize_with(n, Keys::default);
    charge(ex, 3 * n as u64 + 1)?;
    let mut list = reserve_vec::<usize>(ex, &mut live, n)?;
    let mut table = reserve_vec::<usize>(ex, &mut live, n)?;
    let mut scratch = reserve_vec::<usize>(ex, &mut live, n)?;
    list.extend(0..n);
    table.extend(0..n);
    scratch.resize(n, 0);
    if n != 0 {
        let secondary = open(ex, locale_cstr, Strength::Secondary)?;
        let tertiary = if mode == CollateMode::Matrix {
            Some(open(ex, locale_cstr, Strength::Tertiary)?)
        } else {
            None
        };
        for (i, text) in strings.iter().enumerate() {
            keys[i].collection = key(ex, &mut live, &secondary, text, true)?;
            if let Some(tertiary) = &tertiary {
                keys[i].secondary = key(ex, &mut live, &secondary, text, false)?;
                keys[i].tertiary = key(ex, &mut live, tertiary, text, false)?;
            }
        }
    }
    order(ex, &keys, &mut list, &mut scratch, false)?;
    order(ex, &keys, &mut table, &mut scratch, false)?;
    emit_order(ex, &mut output, &list, order_bound)?;
    emit_order(ex, &mut output, &table, order_bound)?;
    match mode {
        CollateMode::Sort => {
            order(ex, &keys, &mut list, &mut scratch, true)?;
            emit_order(ex, &mut output, &list, order_bound)?;
        }
        CollateMode::Matrix => {
            let cells = amount(ex, n.checked_mul(n))?;
            work(ex, cells)?;
            trace!(|t: &mut Trace| t.outputs += 1);
            for a in &keys {
                for b in &keys {
                    output.push(if compare(ex, &a.tertiary, &b.tertiary)?.is_lt() {
                        '1'
                    } else {
                        '0'
                    });
                }
            }
            output.push('|');
            work(ex, cells)?;
            trace!(|t: &mut Trace| t.outputs += 1);
            for a in &keys {
                for b in &keys {
                    output.push(match compare(ex, &a.secondary, &b.secondary)? {
                        std::cmp::Ordering::Less => '<',
                        std::cmp::Ordering::Equal => '=',
                        std::cmp::Ordering::Greater => '>',
                    });
                }
            }
            work(ex, locale.len() + 1)?;
            output.push('|');
            output.push_str(locale);
        }
    }
    Ok(output)
}

#[cfg(test)]
std::thread_local! { static TRACE: std::cell::Cell<Trace> = std::cell::Cell::new(Trace::default()); }
