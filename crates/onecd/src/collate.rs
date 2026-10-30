#[cfg(feature = "icu")]
use std::collections::HashMap;
#[cfg(feature = "icu")]
use std::collections::hash_map::Entry;
#[cfg(feature = "icu")]
use std::sync::{Mutex, OnceLock, PoisonError};

use crate::error::Result;
#[cfg(feature = "icu")]
use crate::error::{Error, Location, Unsupported};
#[cfg(feature = "icu")]
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Strength {
    Secondary,
    Tertiary,
}

pub trait Collate: Send + Sync {
    fn sort_key(&self, locale: &str, strength: Strength, text: &[u16]) -> Result<Vec<u8>>;

    #[cfg(feature = "icu")]
    #[doc(hidden)]
    fn paid_sort_key(
        &self,
        _locale: &str,
        _strength: Strength,
        _text: &[u16],
    ) -> Option<Result<memory_budget::BudgetedVec<u8>>> {
        None
    }
}

pub fn field_string_key(
    collate: &dyn Collate,
    locale: &str,
    field: &crate::FieldDescr,
    text: &str,
) -> Result<Vec<u8>> {
    let strength = if field.case_sensitive {
        Strength::Tertiary
    } else {
        Strength::Secondary
    };
    collate.sort_key(
        locale,
        strength,
        &text
            .trim_end_matches(' ')
            .encode_utf16()
            .collect::<Vec<_>>(),
    )
}

#[cfg(feature = "icu")]
fn non_empty(key: Vec<u8>) -> Result<Vec<u8>> {
    if key.is_empty() {
        Err(Error::corrupt(Location::Root, "ICU returned no sort key"))
    } else {
        Ok(key)
    }
}

#[cfg(feature = "icu")]
#[derive(Debug, Clone, Copy, Default)]
pub struct IcuCollate;

#[cfg(feature = "icu")]
type CollatorCache = Mutex<HashMap<(String, Strength), icu46_sys::Collator>>;

#[cfg(feature = "icu")]
static COLLATORS: OnceLock<CollatorCache> = OnceLock::new();

#[cfg(feature = "icu")]
impl Collate for IcuCollate {
    fn sort_key(&self, locale: &str, strength: Strength, text: &[u16]) -> Result<Vec<u8>> {
        if i32::try_from(text.len()).is_err() {
            return Err(Error::corrupt(
                Location::Root,
                "ICU: string is too long for a sort key",
            ));
        }
        let cache = COLLATORS.get_or_init(Default::default);
        let mut cache = cache.lock().unwrap_or_else(PoisonError::into_inner);
        let key = (locale.to_string(), strength);
        let collator = match cache.entry(key) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => {
                let strength = match strength {
                    Strength::Secondary => icu46_sys::Strength::Secondary,
                    Strength::Tertiary => icu46_sys::Strength::Tertiary,
                };
                let c = icu46_sys::Collator::new(locale, strength)
                    .map_err(|_| Error::Unsupported(Unsupported::NoCollator))?;
                e.insert(c)
            }
        };
        non_empty(collator.sort_key(text))
    }
}

pub fn with_key_cancel<R>(cancel: &std::sync::atomic::AtomicBool, f: impl FnOnce() -> R) -> R {
    struct Restore(*const std::sync::atomic::AtomicBool);
    impl Drop for Restore {
        fn drop(&mut self) {
            KEY_CANCEL.with(|scoped| scoped.set(self.0));
        }
    }
    let _restore = Restore(KEY_CANCEL.with(|scoped| scoped.replace(cancel)));
    f()
}

thread_local! {
    static KEY_CANCEL: std::cell::Cell<*const std::sync::atomic::AtomicBool> =
        const { std::cell::Cell::new(std::ptr::null()) };
}

#[doc(hidden)]
pub fn scoped_key_cancel<R>(f: impl FnOnce(&std::sync::atomic::AtomicBool) -> R) -> R {
    static NEVER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let scoped = KEY_CANCEL.with(std::cell::Cell::get);
    let flag = unsafe { scoped.as_ref() }.unwrap_or(&NEVER);
    f(flag)
}

#[cfg(feature = "icu")]
pub struct KeyRequest {
    budget: memory_budget::MemoryBudget,
    handles: memory_budget::Lock<Handles>,
}

#[cfg(feature = "icu")]
#[derive(Default)]
struct Handles {
    locale: Option<memory_budget::BudgetedVec<u8>>,
    secondary: Option<icu46_sys::Collator>,
    tertiary: Option<icu46_sys::Collator>,
}

#[cfg(feature = "icu")]
impl fmt::Debug for KeyRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeyRequest").finish_non_exhaustive()
    }
}

#[cfg(feature = "icu")]
impl KeyRequest {
    pub fn new(budget: &memory_budget::MemoryBudget) -> KeyRequest {
        KeyRequest {
            budget: budget.clone(),
            handles: memory_budget::Lock::new(Handles {
                locale: None,
                secondary: None,
                tertiary: None,
            }),
        }
    }

    pub fn when_bound(budget: Option<&memory_budget::MemoryBudget>) -> Option<KeyRequest> {
        use icu46_sys::managed::runtime::{Mode, mode, process_root};
        if !matches!(mode(), Mode::Managed | Mode::Quarantined(_)) {
            return None;
        }
        match budget {
            Some(budget) => Some(KeyRequest::new(budget)),
            None => process_root().map(|root| KeyRequest::new(&root)),
        }
    }

    pub fn budget(&self) -> &memory_budget::MemoryBudget {
        &self.budget
    }

    pub fn try_reserve(&self, bytes: u64) -> Result<memory_budget::Reservation> {
        prepared(
            self.budget
                .try_reserve(memory_budget::Category::Execution, bytes)
                .map_err(memory_budget::BudgetError::Limit),
        )
    }

    pub fn sort_key(
        &self,
        locale: &str,
        strength: Strength,
        text: &[u16],
    ) -> Result<memory_budget::BudgetedVec<u8>> {
        not_quarantined()?;
        if i32::try_from(text.len()).is_err() {
            return Err(Error::corrupt(
                Location::Root,
                "ICU: string is too long for a sort key",
            ));
        }
        let mut handles = std::mem::take(&mut *self.handles.lock());
        let key = self.key_with(&mut handles, locale, strength, text);
        let replaced = std::mem::replace(&mut *self.handles.lock(), handles);
        drop(replaced);
        key
    }

    fn key_with(
        &self,
        handles: &mut Handles,
        locale: &str,
        strength: Strength,
        text: &[u16],
    ) -> Result<memory_budget::BudgetedVec<u8>> {
        use icu46_sys::{SORT_KEY_SLACK, SortKeyWrite};
        use memory_budget::{BudgetError, BudgetedVec, Category};
        let budget = &self.budget;
        let opened = handles
            .locale
            .as_ref()
            .is_some_and(|paid| paid.as_slice().strip_suffix(&[0]) == Some(locale.as_bytes()));
        if !opened {
            *handles = Handles::default();
            if locale.as_bytes().contains(&0) {
                return Err(Error::Unsupported(Unsupported::NoCollator));
            }
            let mut paid = prepared(BudgetedVec::try_with_capacity(
                budget,
                Category::Execution,
                locale.len() + 1,
            ))?;
            prepared(paid.try_extend_from_slice(locale.as_bytes()))?;
            prepared(paid.try_push(0))?;
            handles.locale = Some(paid);
        }
        let Handles {
            locale: Some(paid),
            secondary,
            tertiary,
        } = handles
        else {
            return Err(Error::Unsupported(Unsupported::NoCollator));
        };
        let (slot, native) = match strength {
            Strength::Secondary => (secondary, icu46_sys::Strength::Secondary),
            Strength::Tertiary => (tertiary, icu46_sys::Strength::Tertiary),
        };
        let collator = match slot {
            Some(collator) => collator,
            None => {
                let name = std::ffi::CStr::from_bytes_with_nul(paid.as_slice())
                    .map_err(|_| Error::Unsupported(Unsupported::NoCollator))?;
                let opened = scoped_key_cancel(|cancel| {
                    icu46_sys::Collator::open_managed_cancellable(name, native, budget, cancel)
                })
                .map_err(|e| runtime_error(e, Error::Unsupported(Unsupported::NoCollator)))?;
                slot.insert(opened)
            }
        };
        let no_key = || Error::corrupt(Location::Root, "ICU returned no sort key");
        let padded = text.len().checked_add(1).ok_or(BudgetError::Overflow)?;
        let mut source = prepared(BudgetedVec::try_with_capacity(
            budget,
            Category::Execution,
            padded,
        ))?;
        prepared(source.try_extend_from_slice(text))?;
        prepared(source.try_push(0))?;
        let mut probe = [0; SORT_KEY_SLACK + 1];
        let needed = match scoped_key_cancel(|cancel| {
            collator.sort_key_managed_cancellable(&source, &mut probe, budget, cancel)
        })
        .map_err(|e| runtime_error(e, no_key()))?
        {
            SortKeyWrite::Complete(n) | SortKeyWrite::Required(n) => n,
        };
        let capacity = needed
            .checked_add(SORT_KEY_SLACK)
            .ok_or(BudgetError::Overflow)?;
        let mut output = prepared(
            BudgetedVec::try_with_capacity(budget, Category::Execution, capacity)
                .and_then(|mut output| output.try_resize(capacity, 0).map(|()| output)),
        )?;
        match scoped_key_cancel(|cancel| {
            collator.sort_key_managed_cancellable(&source, &mut output, budget, cancel)
        })
        .map_err(|e| runtime_error(e, no_key()))?
        {
            SortKeyWrite::Complete(n) if n > 0 => output.truncate(n),
            _ => return Err(no_key()),
        }
        Ok(output)
    }
}

#[cfg(feature = "icu")]
fn restart(first: icu46_sys::managed::runtime::NativeFailure) -> Error {
    use icu46_sys::managed::FailureCause;
    use icu46_sys::managed::runtime::NativeCause;
    Error::NativeRestart {
        memory: match first.cause {
            NativeCause::Allocation(FailureCause::MemoryLimit(limit)) => Some(limit),
            _ => None,
        },
    }
}

#[cfg(feature = "icu")]
fn not_quarantined() -> Result<()> {
    match icu46_sys::managed::runtime::quarantined() {
        Some(first) => Err(restart(first)),
        None => Ok(()),
    }
}

#[cfg(feature = "icu")]
pub(crate) fn prepared<T>(result: std::result::Result<T, memory_budget::BudgetError>) -> Result<T> {
    result.map_err(|error| match icu46_sys::managed::runtime::quarantined() {
        Some(first) => restart(first),
        None => Error::Memory(error),
    })
}

#[cfg(feature = "icu")]
fn runtime_error(error: icu46_sys::managed::runtime::RuntimeError, other: Error) -> Error {
    match error {
        icu46_sys::managed::runtime::RuntimeError::RestartRequired(first) => restart(first),
        icu46_sys::managed::runtime::RuntimeError::Cancelled => Error::Cancelled,
        _ => other,
    }
}

#[cfg(feature = "icu")]
impl Collate for KeyRequest {
    fn sort_key(&self, locale: &str, strength: Strength, text: &[u16]) -> Result<Vec<u8>> {
        Ok(KeyRequest::sort_key(self, locale, strength, text)?
            .as_slice()
            .to_vec())
    }

    fn paid_sort_key(
        &self,
        locale: &str,
        strength: Strength,
        text: &[u16],
    ) -> Option<Result<memory_budget::BudgetedVec<u8>>> {
        Some(KeyRequest::sort_key(self, locale, strength, text))
    }
}
