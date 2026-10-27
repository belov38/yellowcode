use memory_budget::{BudgetError, Shared};
use yellowcode_core::viewer::body::ViewerBody;
pub(super) const CHECK_BYTES: u64 = 8 << 20;
pub(super) type CheckCache = std::sync::Mutex<AnswerCache<mdconf::startup::ManagedModuleKey>>;
pub(super) struct AnswerCache<K> {
    entry: Option<(K, Shared<Vec<u8>>)>,
    ceiling: u64,
}
impl<K: Copy + Eq> AnswerCache<K> {
    pub(super) fn new(ceiling: u64) -> Self {
        Self {
            entry: None,
            ceiling,
        }
    }
    pub(super) fn clear(&mut self) {
        self.entry = None;
    }
    pub(super) fn lookup(&mut self, key: K) -> Option<ViewerBody> {
        if let Some((stored, body)) = &self.entry
            && *stored == key
        {
            return Some(ViewerBody::Shared(body.clone()));
        }
        self.clear();
        None
    }
    pub(super) fn insert(&mut self, key: K, body: ViewerBody) -> Result<ViewerBody, BudgetError> {
        self.clear();
        let shared = match body {
            ViewerBody::Owned(bytes) if bytes.reservation().bytes() <= self.ceiling => {
                bytes.try_into_shared()?
            }
            ViewerBody::Shared(bytes) => bytes,
            ViewerBody::Eager(_) => return Err(BudgetError::Overflow),
            other => return Ok(other),
        };
        if shared.reservation().bytes() <= self.ceiling {
            self.entry = Some((key, shared.clone()));
        }
        Ok(ViewerBody::Shared(shared))
    }
}
