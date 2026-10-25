pub(crate) const MAX_VALUE_DEPTH: usize = 1000;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DepthExceeded;

pub(crate) struct DepthGuard {
    depth: usize,
    max: usize,
}

impl DepthGuard {
    pub(crate) fn new() -> DepthGuard {
        DepthGuard::with_max(MAX_VALUE_DEPTH)
    }

    pub(crate) fn with_max(max: usize) -> DepthGuard {
        DepthGuard {
            depth: 0,
            max: max.min(MAX_VALUE_DEPTH),
        }
    }

    pub(crate) fn enter(&mut self) -> Result<(), DepthExceeded> {
        if self.depth >= self.max {
            return Err(DepthExceeded);
        }
        self.depth += 1;
        Ok(())
    }

    pub(crate) fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }
}
