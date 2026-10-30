use bsl_prim::Str;
use std::mem::size_of;

pub(super) fn rc_bytes<T: ?Sized>(value: &T) -> u64 {
    rc_layout(value).map_or(u64::MAX, |(layout, _)| layout.size() as u64)
}

pub(super) fn rc_layout<T: ?Sized>(value: &T) -> Option<(std::alloc::Layout, usize)> {
    rc_layout_for(std::alloc::Layout::for_value(value))
}

pub(super) fn rc_layout_for(payload: std::alloc::Layout) -> Option<(std::alloc::Layout, usize)> {
    std::alloc::Layout::new::<[usize; 2]>()
        .extend(payload)
        .ok()
        .map(|(layout, offset)| (layout.pad_to_align(), offset))
}

pub(super) fn text_bytes(text: &Str) -> u64 {
    ((size_of::<Vec<u16>>() + 2 * size_of::<usize>()) as u64)
        .saturating_add((text.backing_capacity_units() as u64).saturating_mul(2))
}

pub(super) fn text_extra(text: &Str) -> u64 {
    text_bytes(text).saturating_sub((text.len() as u64).saturating_mul(2))
}
