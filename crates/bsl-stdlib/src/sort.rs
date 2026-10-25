use bsl_prim::{BslDate, Number, Strength};
use bsl_vm::{Exception, Execution, HostError, Value};
use std::cmp::Ordering;

#[derive(PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum SortKey {
    Undefined,
    Bool(bool),
    Number(Number),
    String(Vec<u8>),
    Date(BslDate),
    Other(Vec<u8>),
}
impl SortKey {
    pub(crate) fn payload_work(&self) -> u64 {
        match self {
            Self::String(bytes) | Self::Other(bytes) => bytes.len() as u64 / 64,
            Self::Number(n) => n.size_hint_limbs() / 64,
            _ => 0,
        }
    }
}

fn text_key(ex: &mut Execution, text: &[u16], cache_bytes: &mut u64) -> Result<Vec<u8>, Exception> {
    ex.charge((text.len() as u64 / 32).saturating_add(1))?;
    let end = text.iter().rposition(|&u| u != 0x20).map_or(0, |i| i + 1);
    let text = &text[..end];
    let source_bytes = (text.len() as u64).saturating_mul(2);
    let input_bytes = (text.len() as u64).saturating_add(1).saturating_mul(2);
    ex.reserve(
        cache_bytes
            .saturating_add(source_bytes)
            .saturating_add(input_bytes),
    )?;
    let bytes = ex
        .env()
        .collation()
        .sort_key_len(Strength::Secondary, text)
        .map(|n| n as u64)
        .unwrap_or_else(|| (text.len() as u64).saturating_mul(32).saturating_add(64));
    let work_bytes = (text.len() as u64)
        .saturating_mul(4)
        .max(bytes)
        .saturating_add(64);
    ex.reserve(
        cache_bytes
            .saturating_add(source_bytes)
            .saturating_add(input_bytes)
            .saturating_add(work_bytes)
            .saturating_add(bytes),
    )?;
    let key = ex.env().collation().sort_key_for_sorting(text);
    *cache_bytes = cache_bytes.saturating_add(key.len() as u64);
    ex.reserve(*cache_bytes)?;
    Ok(key)
}

pub(crate) fn sort_key(
    ex: &mut Execution,
    value: &Value,
    cache_bytes: &mut u64,
) -> Result<SortKey, Exception> {
    ex.charge(1)?;
    Ok(match value {
        Value::Undefined => SortKey::Undefined,
        Value::Bool(b) => SortKey::Bool(*b),
        Value::Number(n) => {
            ex.charge(n.size_hint_limbs() / 64)?;
            *cache_bytes = cache_bytes.saturating_add(n.size_hint_limbs().saturating_mul(4));
            ex.reserve(*cache_bytes)?;
            SortKey::Number(n.clone())
        }
        Value::String(s) => SortKey::String(text_key(ex, s.units(), cache_bytes)?),
        Value::Date(d) => SortKey::Date(*d),
        other => {
            let text = crate::present::present_for_collection(ex, other, *cache_bytes)?;
            SortKey::Other(text_key(ex, text.units(), cache_bytes)?)
        }
    })
}

pub(crate) fn sort_by_keys<T>(
    ex: &mut Execution,
    items: &mut [T],
    keys: &[Vec<SortKey>],
    desc: &[bool],
) -> Result<(), Exception> {
    ex.charge(keys.len() as u64)?;
    if keys.len() != items.len() || keys.iter().any(|row| row.len() != desc.len()) {
        return Err(ex.error(HostError::not_implemented(
            "Сортировка",
            "Размерность ключей",
        )));
    }
    let n = items.len();
    let levels = usize::BITS - n.max(1).leading_zeros();
    ex.reserve((n as u64).saturating_mul(2 * std::mem::size_of::<usize>() as u64))?;
    ex.charge(
        (n as u64)
            .saturating_mul(u64::from(levels))
            .saturating_mul(desc.len().max(1) as u64),
    )?;
    let mut order = Vec::new();
    order.try_reserve_exact(n).map_err(|_| ex.out_of_memory())?;
    order.extend(0..n);
    let mut comparison_error = None;
    bsl_lib::sort::msvc_sort(&mut order, &mut |&a, &b| {
        if comparison_error.is_some() {
            return false;
        }
        for ((a, b), reverse) in keys[a].iter().zip(&keys[b]).zip(desc) {
            if let Err(error) = ex.charge(1 + a.payload_work().max(b.payload_work())) {
                comparison_error = Some(error);
                return false;
            }
            let cmp = a.cmp(b);
            if cmp != Ordering::Equal {
                return if *reverse {
                    cmp == Ordering::Greater
                } else {
                    cmp == Ordering::Less
                };
            }
        }
        false
    });
    if let Some(error) = comparison_error {
        return Err(error);
    }
    for i in 0..n {
        let mut cur = i;
        while order[cur] != cur {
            let next = order[cur];
            if next == i {
                order[cur] = cur;
                break;
            }
            items.swap(cur, next);
            order[cur] = cur;
            cur = next;
        }
    }
    Ok(())
}
