use crate::ifile::IFile;
use memory_budget::{BudgetedVec, Category, MemoryBudget};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{PoisonError, RwLock};

const MAX_FILES: usize = 512;

#[derive(Default)]
struct State {
    epoch: u64,
    files: Option<BudgetedVec<IFile>>,
}

pub struct FileCache {
    state: RwLock<State>,
    budget: Option<MemoryBudget>,
    keep: AtomicBool,
}

impl std::fmt::Debug for FileCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileCache").finish_non_exhaustive()
    }
}

impl FileCache {
    pub(crate) fn new(budget: Option<MemoryBudget>) -> Self {
        Self {
            state: RwLock::new(State::default()),
            budget,
            keep: AtomicBool::new(false),
        }
    }
    pub(crate) fn keep(&self) {
        self.keep.store(true, Ordering::Relaxed);
    }
    pub(crate) fn get(&self, epoch: u64, header_page: u32) -> Option<IFile> {
        let state = self.state.read().unwrap_or_else(PoisonError::into_inner);
        if state.epoch != epoch {
            return None;
        }
        state
            .files
            .as_ref()?
            .iter()
            .find(|f| f.header_page == header_page)
            .cloned()
    }

    pub(crate) fn insert(&self, epoch: u64, file: &IFile) {
        let Some(budget) = self.budget.as_ref() else {
            return;
        };
        if !self.keep.load(Ordering::Relaxed) {
            return;
        }
        let mut replaced = None;
        {
            let mut state = self.state.write().unwrap_or_else(PoisonError::into_inner);
            if epoch < state.epoch {
                return;
            }
            if epoch > state.epoch {
                state.epoch = epoch;
                replaced = state.files.take();
            }
            if state.files.is_none() {
                state.files = BudgetedVec::try_with_capacity(budget, Category::Metadata, 16).ok();
            }
            if let Some(files) = state.files.as_mut()
                && files.len() < MAX_FILES
                && !files.iter().any(|f| f.header_page == file.header_page)
            {
                let _ = files.try_push(file.clone());
            }
        }
        drop(replaced);
    }

    pub(crate) fn clear(&self) {
        let dropped = self
            .state
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .files
            .take();
        drop(dropped);
    }

    pub fn len(&self) -> usize {
        self.state
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .files
            .as_ref()
            .map_or(0, |f| f.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
