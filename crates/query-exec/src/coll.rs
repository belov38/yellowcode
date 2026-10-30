use crate::managed::collation::{CollationError, ManagedCollation};
use bsl_prim::{Collation, Strength};
use icu46_sys::managed::FailureCause;
use icu46_sys::managed::runtime::{Mode, NativeCause, RuntimeError};
use memory_budget::{BudgetError, Category, LimitReason, MemoryBudget, MemoryLimit};
use query_ir::{ExecError, NativeError};
use std::cmp::Ordering;
use std::ffi::CStr;
use std::sync::atomic::AtomicBool;

#[derive(Clone, Copy, Debug)]
pub struct NativeCollation<'a> {
    pub locale: &'a CStr,
    pub budget: &'a MemoryBudget,
}

impl NativeCollation<'_> {
    pub fn when_bound<'a>(
        locale: &'a CStr,
        budget: &'a MemoryBudget,
    ) -> Option<NativeCollation<'a>> {
        matches!(
            icu46_sys::managed::runtime::mode(),
            Mode::Managed | Mode::Quarantined(_)
        )
        .then_some(NativeCollation { locale, budget })
    }

    pub fn open(&self, cancel: &AtomicBool) -> Result<ManagedCollation, ExecError> {
        ManagedCollation::try_open(self.locale, self.budget, cancel).map_err(exec_error)
    }
}

#[derive(Clone, Copy)]
pub struct ManagedRoute<'a> {
    pub collation: &'a ManagedCollation,
    pub budget: &'a MemoryBudget,
    pub cancel: &'a AtomicBool,
}

#[derive(Clone, Copy)]
pub enum ExecCollation<'a> {
    Legacy(&'a dyn Collation),
    Managed(ManagedRoute<'a>),
}

impl<'a> From<&'a dyn Collation> for ExecCollation<'a> {
    fn from(collation: &'a dyn Collation) -> Self {
        Self::Legacy(collation)
    }
}
impl<'a, T: Collation> From<&'a T> for ExecCollation<'a> {
    fn from(collation: &'a T) -> Self {
        Self::Legacy(collation)
    }
}

impl ExecCollation<'_> {
    pub fn sort_key(self, strength: Strength, text: &[u16]) -> Result<Vec<u8>, ExecError> {
        match self {
            Self::Legacy(c) => c.try_sort_key(strength, text).map_err(legacy_error),
            Self::Managed(m) => m
                .collation
                .try_sort_key_cancellable(strength, text, m.budget, m.cancel)
                .map(|key| key.to_vec())
                .map_err(exec_error),
        }
    }

    pub fn sort_key_len(
        self,
        strength: Strength,
        text: &[u16],
    ) -> Result<Option<usize>, ExecError> {
        match self {
            Self::Legacy(c) => Ok(c.sort_key_len(strength, text)),
            Self::Managed(m) => m
                .collation
                .try_sort_key_len(strength, text, m.budget, m.cancel)
                .map(Some)
                .map_err(exec_error),
        }
    }

    pub fn less(self, a: &[u16], b: &[u16]) -> Result<bool, ExecError> {
        match self {
            Self::Legacy(c) => c.try_less(a, b).map_err(legacy_error),
            Self::Managed(_) => {
                Ok(self.sort_key(Strength::Tertiary, a)? < self.sort_key(Strength::Tertiary, b)?)
            }
        }
    }

    pub fn compare_secondary(self, a: &[u16], b: &[u16]) -> Result<Ordering, ExecError> {
        match self {
            Self::Legacy(c) => c.try_compare_secondary(a, b).map_err(legacy_error),
            Self::Managed(_) => Ok(self
                .sort_key(Strength::Secondary, a)?
                .cmp(&self.sort_key(Strength::Secondary, b)?)),
        }
    }

    pub fn sort_key_for_sorting(self, text: &[u16]) -> Result<Vec<u8>, ExecError> {
        match self {
            Self::Legacy(c) => c.try_sort_key_for_sorting(text).map_err(legacy_error),
            Self::Managed(_) => {
                let end = text.iter().rposition(|&u| u != 0x20).map_or(0, |p| p + 1);
                self.sort_key(Strength::Secondary, text.get(..end).unwrap_or(&[]))
            }
        }
    }
}

pub fn exec_error(error: CollationError) -> ExecError {
    match error {
        CollationError::Budget(BudgetError::Limit(limit)) => ExecError::Memory(limit),
        CollationError::Budget(BudgetError::Overflow | BudgetError::AllocationFailed) => {
            ExecError::Memory(MemoryLimit {
                category: Category::Execution,
                requested: 0,
                reason: LimitReason::Overflow,
            })
        }
        CollationError::Icu(status) => ExecError::Native(NativeError::Status(status.code)),
        CollationError::Runtime(error) => runtime_error(error),
    }
}

fn legacy_error(error: bsl_prim::CollationError) -> ExecError {
    match error {
        bsl_prim::CollationError::Memory => ExecError::Memory(MemoryLimit {
            category: Category::Execution,
            requested: 0,
            reason: LimitReason::Category,
        }),
        bsl_prim::CollationError::Restart { memory } => {
            ExecError::Native(NativeError::RestartRequired {
                memory: memory.then_some(MemoryLimit {
                    category: Category::Execution,
                    requested: 0,
                    reason: LimitReason::Category,
                }),
            })
        }
    }
}

fn runtime_error(error: RuntimeError) -> ExecError {
    match error {
        RuntimeError::RestartRequired(first) => ExecError::Native(NativeError::RestartRequired {
            memory: match first.cause {
                NativeCause::Allocation(FailureCause::MemoryLimit(limit)) => Some(limit),
                _ => None,
            },
        }),
        RuntimeError::Cancelled => ExecError::Fatal("выполнение прервано".to_string()),
        RuntimeError::Native(code) => ExecError::Native(NativeError::Status(code)),
        RuntimeError::LateBinding
        | RuntimeError::ForeignRoot
        | RuntimeError::ManagedContextRequired
        | RuntimeError::Unbound
        | RuntimeError::Reentrant => ExecError::Native(NativeError::Status(27)),
    }
}

pub trait Keys: Copy {
    type Error;
    fn key(self, strength: Strength, text: &[u16]) -> Result<Vec<u8>, Self::Error>;
    fn key_len(self, strength: Strength, text: &[u16]) -> Result<Option<usize>, Self::Error>;
}
impl Keys for &dyn Collation {
    type Error = std::convert::Infallible;
    fn key(self, strength: Strength, text: &[u16]) -> Result<Vec<u8>, Self::Error> {
        Ok(self.sort_key(strength, text))
    }
    fn key_len(self, strength: Strength, text: &[u16]) -> Result<Option<usize>, Self::Error> {
        Ok(self.sort_key_len(strength, text))
    }
}
impl Keys for ExecCollation<'_> {
    type Error = ExecError;
    fn key(self, strength: Strength, text: &[u16]) -> Result<Vec<u8>, ExecError> {
        self.sort_key(strength, text)
    }
    fn key_len(self, strength: Strength, text: &[u16]) -> Result<Option<usize>, ExecError> {
        self.sort_key_len(strength, text)
    }
}

pub(crate) fn infallible<T>(result: Result<T, std::convert::Infallible>) -> T {
    match result {
        Ok(value) => value,
        Err(never) => match never {},
    }
}

pub fn open(env: &crate::source::ExecEnv<'_>) -> Result<Option<ManagedCollation>, ExecError> {
    env.native.map(|n| n.open(env.cancel)).transpose()
}

pub fn route<'a>(
    env: &'a crate::source::ExecEnv<'a>,
    managed: Option<&'a ManagedCollation>,
) -> ExecCollation<'a> {
    match (env.native, managed) {
        (Some(native), Some(collation)) => ExecCollation::Managed(ManagedRoute {
            collation,
            budget: native.budget,
            cancel: env.cancel,
        }),
        _ => ExecCollation::Legacy(env.collation),
    }
}

pub(crate) struct NotConsulted;
impl Collation for NotConsulted {
    fn sort_key(&self, _: Strength, _: &[u16]) -> Vec<u8> {
        Vec::new()
    }
}
