use super::{BUF_SIZE, STREAM_SIZE, Stream, WINDOW_BITS, z};
use memory_budget::{
    BudgetError, BudgetedVec, Category, Charged, MemoryBudget, Reservation, Shared,
};
use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::ptr::{self, NonNull};

#[derive(Debug, thiserror::Error)]
pub enum ManagedInflateError {
    #[error("{0}")]
    Memory(#[from] BudgetError),
    #[error("corrupt deflate stream")]
    Corrupt,
    #[error("deflate stream is truncated")]
    Truncated,
    #[error("data after the end of the deflate stream")]
    Trailing,
    #[error("inflated size exceeds {0} bytes")]
    TooLarge(u64),
    #[error("zlib initialization or execution failed: {0}")]
    Zlib(i32),
}

struct Context {
    budget: MemoryBudget,
    category: Category,
    error: Option<BudgetError>,
}
struct Header {
    reservation: Reservation,
    layout: Layout,
}
fn header_layout() -> Layout {
    Layout::new::<Header>()
        .align_to(16)
        .unwrap_or(Layout::new::<Header>())
        .pad_to_align()
}
fn failure(context: &mut Context, error: BudgetError) -> z::voidpf {
    if context.error.is_none() {
        context.error = Some(error);
    }
    ptr::null_mut()
}
extern "C" fn allocate(opaque: z::voidpf, count: z::uInt, size: z::uInt) -> z::voidpf {
    let context = unsafe { &mut *opaque.cast::<Context>() };
    let Some(bytes) = (count as usize).checked_mul(size as usize) else {
        return failure(context, BudgetError::Overflow);
    };
    let Ok(payload) = Layout::from_size_align(bytes, 16) else {
        return failure(context, BudgetError::Overflow);
    };
    let Ok((layout, offset)) = header_layout().extend(payload) else {
        return failure(context, BudgetError::Overflow);
    };
    let layout = layout.pad_to_align();
    let Ok(bytes) = u64::try_from(layout.size()) else {
        return failure(context, BudgetError::Overflow);
    };
    let reservation = match context.budget.try_reserve(context.category, bytes) {
        Ok(reservation) => reservation,
        Err(error) => return failure(context, error.into()),
    };
    let raw = unsafe { alloc_zeroed(layout) };
    if raw.is_null() {
        return failure(context, BudgetError::AllocationFailed);
    }
    unsafe {
        raw.cast::<Header>().write(Header {
            reservation,
            layout,
        });
        raw.add(offset).cast()
    }
}
unsafe extern "C" fn release(_opaque: z::voidpf, pointer: z::voidpf) {
    if pointer.is_null() {
        return;
    }
    unsafe {
        let raw = pointer.cast::<u8>().sub(header_layout().size());
        let Header {
            reservation,
            layout,
        } = raw.cast::<Header>().read();
        dealloc(raw, layout);
        drop(reservation);
    }
}
struct ManagedStream {
    stream: Stream,
    context: Shared<Context>,
}
impl ManagedStream {
    fn new(budget: &MemoryBudget, category: Category) -> Result<Self, ManagedInflateError> {
        let mut context = BudgetedVec::<u8>::new(budget, category)?
            .into_charged_vec()
            .map(|_| Context {
                budget: budget.clone(),
                category,
                error: None,
            })
            .try_into_shared()?;
        let opaque =
            Shared::get_mut(&mut context).ok_or(BudgetError::AllocationFailed)? as *mut Context;
        let mut backing = BudgetedVec::try_with_capacity(budget, category, 1)?;
        backing.try_push(z::z_stream {
            next_in: ptr::null_mut(),
            avail_in: 0,
            total_in: 0,
            next_out: ptr::null_mut(),
            avail_out: 0,
            total_out: 0,
            msg: ptr::null_mut(),
            state: ptr::null_mut(),
            zalloc: allocate,
            zfree: release,
            opaque: opaque.cast(),
            data_type: 0,
            adler: 0,
            reserved: 0,
        })?;
        let pointer = NonNull::new(backing.as_mut_ptr()).ok_or(BudgetError::AllocationFailed)?;
        let ret = unsafe {
            z::inflateInit2_(pointer.as_ptr(), WINDOW_BITS, z::zlibVersion(), STREAM_SIZE)
        };
        if ret != z::Z_OK {
            return Err(context
                .error
                .map(ManagedInflateError::Memory)
                .unwrap_or(ManagedInflateError::Zlib(ret)));
        }
        Ok(Self {
            stream: Stream {
                s: pointer,
                deflate: false,
                managed: Some(backing.into_charged_vec()),
            },
            context,
        })
    }
    fn error(&self, ret: i32) -> ManagedInflateError {
        if let Some(error) = self.context.error {
            return ManagedInflateError::Memory(error);
        }
        match ret {
            z::Z_DATA_ERROR | z::Z_NEED_DICT => ManagedInflateError::Corrupt,
            z::Z_MEM_ERROR => ManagedInflateError::Memory(BudgetError::AllocationFailed),
            _ => ManagedInflateError::Zlib(ret),
        }
    }
}

pub fn inflate_budgeted(
    data: &[u8],
    limit: u64,
    budget: &MemoryBudget,
    category: Category,
) -> Result<Charged<Vec<u8>>, ManagedInflateError> {
    let mut stream = ManagedStream::new(budget, category)?;
    let mut output = BudgetedVec::new(budget, category)?;
    let mut scratch = [0u8; BUF_SIZE];
    let mut position = 0usize;
    let mut total = 0u64;
    loop {
        let rest = data.get(position..).unwrap_or_default();
        let (used, n, ret) = stream.stream.step(rest, &mut scratch, z::Z_NO_FLUSH);
        if stream.context.error.is_some() || ret == z::Z_MEM_ERROR {
            return Err(stream.error(ret));
        }
        position = position.checked_add(used).ok_or(BudgetError::Overflow)?;
        total = total
            .checked_add(u64::try_from(n).map_err(|_| BudgetError::Overflow)?)
            .ok_or(BudgetError::Overflow)?;
        if total > limit {
            return Err(ManagedInflateError::TooLarge(limit));
        }
        output.try_extend_from_slice(scratch.get(..n).ok_or(BudgetError::Overflow)?)?;
        match ret {
            z::Z_OK => {}
            z::Z_STREAM_END if position < data.len() => return Err(ManagedInflateError::Trailing),
            z::Z_STREAM_END => return Ok(output.into_charged_vec()),
            z::Z_BUF_ERROR => return Err(ManagedInflateError::Truncated),
            _ => return Err(stream.error(ret)),
        }
    }
}
