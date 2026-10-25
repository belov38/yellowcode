use std::{marker::PhantomData, rc::Rc};

pub const OWNER_WORKER_STACK_BYTES: usize = 16 * 1024 * 1024;

struct WorkerAnchor;

pub struct WorkerContext<'w> {
    _anchor: &'w WorkerAnchor,
    _confined: PhantomData<Rc<()>>,
    _invariant: PhantomData<fn(&'w ()) -> &'w ()>,
}

pub unsafe fn with_current_worker<R>(job: impl for<'w> FnOnce(&'w WorkerContext<'w>) -> R) -> R {
    let anchor = WorkerAnchor;
    let context = WorkerContext {
        _anchor: &anchor,
        _confined: PhantomData,
        _invariant: PhantomData,
    };
    job(&context)
}
