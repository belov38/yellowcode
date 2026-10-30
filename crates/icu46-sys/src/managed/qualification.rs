use super::NativeOwner;
use super::allocation::{header, native_alignment};
use memory_budget::{Category, MemoryBudget};
use std::ffi::c_void;

pub struct BlockInfo {
    pub payload: usize,
    pub charged: usize,
    pub alignment: usize,
    pub owner: NativeOwner,
    pub category: Category,
    pub budget: MemoryBudget,
}
pub unsafe fn block_info(pointer: *mut c_void) -> BlockInfo {
    let header = unsafe { &*header(pointer) };
    BlockInfo {
        payload: header.capacity,
        charged: header.layout.size(),
        alignment: native_alignment(),
        owner: header.owner,
        category: header.reservation.category(),
        budget: header.reservation.budget().clone(),
    }
}
