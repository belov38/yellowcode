pub(crate) const MAX_VALUE_DEPTH: usize = 1000;

pub(crate) const MAX_WALK_STACK: usize = 3 << 19;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DepthExceeded;

pub(crate) struct DepthGuard {
    depth: usize,
    max: usize,
    _thread: std::marker::PhantomData<*const ()>,
}

struct Origin {
    base: std::cell::Cell<usize>,
    alive: std::cell::Cell<usize>,
}

thread_local! {
    static ORIGIN: Origin = const {
        Origin {
            base: std::cell::Cell::new(0),
            alive: std::cell::Cell::new(0),
        }
    };
}

#[inline(always)]
fn stack_position() -> usize {
    let probe = 0u8;
    std::hint::black_box(&probe) as *const u8 as usize
}

#[inline(always)]
fn used_stack() -> usize {
    let here = stack_position();
    ORIGIN.with(|o| {
        let base = o.base.get().max(here);
        o.base.set(base);
        base - here
    })
}

impl DepthGuard {
    pub(crate) fn new() -> DepthGuard {
        DepthGuard::with_max(MAX_VALUE_DEPTH)
    }

    pub(crate) fn with_max(max: usize) -> DepthGuard {
        let here = stack_position();
        ORIGIN.with(|o| {
            let alive = o.alive.get();
            o.base.set(if alive == 0 {
                here
            } else {
                o.base.get().max(here)
            });
            o.alive.set(alive.saturating_add(1));
        });
        DepthGuard {
            depth: 0,
            max: max.min(MAX_VALUE_DEPTH),
            _thread: std::marker::PhantomData,
        }
    }

    pub(crate) fn enter(&mut self) -> Result<(), DepthExceeded> {
        self.enter_many(1)
    }

    pub(crate) fn check(&self, levels: usize) -> Result<(), DepthExceeded> {
        if levels > self.max - self.depth {
            return Err(DepthExceeded);
        }
        Ok(())
    }

    pub(crate) fn enter_many(&mut self, levels: usize) -> Result<(), DepthExceeded> {
        self.check(levels)?;
        if used_stack() > MAX_WALK_STACK {
            return Err(DepthExceeded);
        }
        self.depth += levels;
        Ok(())
    }

    pub(crate) fn leave_many(&mut self, levels: usize) {
        self.depth = self.depth.saturating_sub(levels);
    }

    pub(crate) fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }
}

impl Drop for DepthGuard {
    fn drop(&mut self) {
        let _ = ORIGIN.try_with(|o| o.alive.set(o.alive.get().saturating_sub(1)));
    }
}
