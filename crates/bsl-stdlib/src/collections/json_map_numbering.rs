use std::collections::HashMap;

type Numbers = HashMap<usize, i32>;

pub(super) fn growth_bound(numbers: &Numbers, needed: bool) -> Option<u64> {
    if !needed {
        return Some(0);
    }
    let capacity = numbers.capacity().checked_add(1)?.checked_mul(2)?.max(3);
    bsl_vm::retained_hash_bytes::<(usize, i32)>(capacity)
}

pub(super) fn reserve(numbers: &mut Numbers, needed: bool, admitted: u64) -> Option<()> {
    if needed {
        numbers.try_reserve(1).ok()?;
        let actual = bsl_vm::retained_hash_bytes::<(usize, i32)>(numbers.capacity())?;
        if actual > admitted {
            return None;
        }
    }
    Some(())
}
