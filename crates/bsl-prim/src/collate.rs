use std::cmp::Ordering;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Strength {
    Secondary,
    Tertiary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollationError {
    Memory,
    Restart { memory: bool },
}

impl std::fmt::Display for CollationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Memory => "MEMORY_LIMIT: not enough memory to compare strings",
            Self::Restart { memory: true } => "MEMORY_LIMIT: native: restart the application",
            Self::Restart { memory: false } => {
                "native ICU allocation failed: restart the application"
            }
        })
    }
}

impl std::error::Error for CollationError {}

pub trait Collation: Send + Sync {
    fn sort_key(&self, strength: Strength, text: &[u16]) -> Vec<u8>;

    fn sort_key_len(&self, strength: Strength, text: &[u16]) -> Option<usize> {
        let _ = (strength, text);
        None
    }

    fn less(&self, a: &[u16], b: &[u16]) -> bool {
        self.sort_key(Strength::Tertiary, a) < self.sort_key(Strength::Tertiary, b)
    }

    fn compare_secondary(&self, a: &[u16], b: &[u16]) -> Ordering {
        self.sort_key(Strength::Secondary, a)
            .cmp(&self.sort_key(Strength::Secondary, b))
    }

    fn sort_key_for_sorting(&self, text: &[u16]) -> Vec<u8> {
        let end = text.iter().rposition(|&u| u != 0x20).map_or(0, |p| p + 1);
        self.sort_key(Strength::Secondary, &text[..end])
    }

    fn try_sort_key(&self, strength: Strength, text: &[u16]) -> Result<Vec<u8>, CollationError> {
        Ok(self.sort_key(strength, text))
    }

    fn try_less(&self, a: &[u16], b: &[u16]) -> Result<bool, CollationError> {
        Ok(self.try_sort_key(Strength::Tertiary, a)? < self.try_sort_key(Strength::Tertiary, b)?)
    }

    fn try_compare_secondary(&self, a: &[u16], b: &[u16]) -> Result<Ordering, CollationError> {
        Ok(self
            .try_sort_key(Strength::Secondary, a)?
            .cmp(&self.try_sort_key(Strength::Secondary, b)?))
    }

    fn try_sort_key_for_sorting(&self, text: &[u16]) -> Result<Vec<u8>, CollationError> {
        let end = text.iter().rposition(|&u| u != 0x20).map_or(0, |p| p + 1);
        self.try_sort_key(Strength::Secondary, &text[..end])
    }
}

#[cfg(feature = "icu")]
fn fallback_key(text: &[u16]) -> Vec<u8> {
    let mut k = Vec::with_capacity(text.len() * 2 + 1);
    for u in text {
        k.extend_from_slice(&u.to_be_bytes());
    }
    k.push(0);
    k
}

#[cfg(feature = "icu")]
pub use icu_impl::IcuCollation;

#[cfg(feature = "icu")]
mod icu_impl {
    use super::{Collation, CollationError, Strength, fallback_key};
    use std::cell::RefCell;
    use std::cmp::Ordering;
    use std::collections::HashMap;

    thread_local! {
        static CACHE: RefCell<HashMap<String, [Option<icu46_sys::Collator>; 2]>> =
            RefCell::new(HashMap::new());
    }

    #[derive(Clone, Debug)]
    pub struct IcuCollation {
        locale: String,
    }

    impl IcuCollation {
        pub fn new(locale: &str) -> IcuCollation {
            IcuCollation {
                locale: locale.to_string(),
            }
        }

        pub fn locale(&self) -> &str {
            &self.locale
        }
    }

    fn open(locale: &str, strength: Strength) -> Option<icu46_sys::Collator> {
        let s = match strength {
            Strength::Secondary => icu46_sys::Strength::Secondary,
            Strength::Tertiary => icu46_sys::Strength::Tertiary,
        };
        icu46_sys::Collator::new(locale, s)
            .or_else(|_| icu46_sys::Collator::new("root", s))
            .ok()
    }

    impl Collation for IcuCollation {
        #[allow(
            clippy::panic,
            reason = "documented: bound processes use the try_ methods"
        )]
        fn sort_key(&self, strength: Strength, text: &[u16]) -> Vec<u8> {
            if super::managed::bound() {
                return super::managed::key(&self.locale, strength, text)
                    .map(|key| key.as_slice().to_vec())
                    .unwrap_or_else(|error| panic!("{error}"));
            }
            if i32::try_from(text.len()).is_err() {
                return fallback_key(text);
            }
            let slot = usize::from(strength == Strength::Tertiary);
            let key = CACHE.with(|c| {
                let mut map = c.borrow_mut();
                if !map.contains_key(self.locale.as_str()) {
                    map.insert(self.locale.clone(), [None, None]);
                }
                let entry = map.get_mut(self.locale.as_str())?;
                if entry[slot].is_none() {
                    entry[slot] = open(&self.locale, strength);
                }
                entry[slot].as_ref().map(|coll| coll.sort_key(text))
            });
            match key {
                Some(k) if !k.is_empty() => k,
                _ => fallback_key(text),
            }
        }

        fn try_sort_key(
            &self,
            strength: Strength,
            text: &[u16],
        ) -> Result<Vec<u8>, CollationError> {
            if super::managed::bound() {
                return super::managed::key(&self.locale, strength, text)
                    .map(|key| key.as_slice().to_vec());
            }
            Ok(self.sort_key(strength, text))
        }

        fn try_less(&self, a: &[u16], b: &[u16]) -> Result<bool, CollationError> {
            if super::managed::bound() {
                let a = super::managed::key(&self.locale, Strength::Tertiary, a)?;
                let b = super::managed::key(&self.locale, Strength::Tertiary, b)?;
                return Ok(a.as_slice() < b.as_slice());
            }
            Ok(self.less(a, b))
        }

        fn try_compare_secondary(&self, a: &[u16], b: &[u16]) -> Result<Ordering, CollationError> {
            if super::managed::bound() {
                let a = super::managed::key(&self.locale, Strength::Secondary, a)?;
                let b = super::managed::key(&self.locale, Strength::Secondary, b)?;
                return Ok(a.as_slice().cmp(b.as_slice()));
            }
            Ok(self.compare_secondary(a, b))
        }

        fn sort_key_len(&self, strength: Strength, text: &[u16]) -> Option<usize> {
            if super::managed::bound() {
                return None;
            }
            if i32::try_from(text.len()).is_err() {
                return Some(2 * text.len() + 1);
            }
            let slot = usize::from(strength == Strength::Tertiary);
            CACHE.with(|c| {
                let mut map = c.borrow_mut();
                if !map.contains_key(self.locale.as_str()) {
                    map.insert(self.locale.clone(), [None, None]);
                }
                let entry = map.get_mut(self.locale.as_str())?;
                if entry[slot].is_none() {
                    entry[slot] = open(&self.locale, strength);
                }
                entry[slot]
                    .as_ref()
                    .and_then(|coll| coll.sort_key_len(text))
            })
        }
    }
}

#[cfg(feature = "icu")]
mod managed {
    use super::{CollationError, Strength};
    use icu46_sys::managed::FailureCause;
    use icu46_sys::managed::runtime::{
        Mode, NativeCause, NativeFailure, RuntimeError, mode, process_root, quarantined,
    };
    use icu46_sys::{Collator, SORT_KEY_SLACK, SortKeyWrite};
    use memory_budget::{BudgetError, BudgetedVec, Category, MemoryBudget};
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::sync::atomic::AtomicBool;

    thread_local! {
        static OPENED: RefCell<HashMap<String, [Option<Collator>; 2]>> = RefCell::new(HashMap::new());
    }

    static NEVER: AtomicBool = AtomicBool::new(false);

    pub(super) fn bound() -> bool {
        matches!(mode(), Mode::Managed | Mode::Quarantined(_))
    }

    fn restart(first: NativeFailure) -> CollationError {
        CollationError::Restart {
            memory: matches!(
                first.cause,
                NativeCause::Allocation(FailureCause::MemoryLimit(_))
            ),
        }
    }

    fn prepared<T>(result: Result<T, BudgetError>) -> Result<T, CollationError> {
        result.map_err(|_| quarantined().map_or(CollationError::Memory, restart))
    }

    fn runtime_error(error: RuntimeError) -> CollationError {
        match error {
            RuntimeError::RestartRequired(first) => restart(first),
            _ => quarantined().map_or(CollationError::Restart { memory: false }, restart),
        }
    }

    pub(super) fn key(
        locale: &str,
        strength: Strength,
        text: &[u16],
    ) -> Result<BudgetedVec<u8>, CollationError> {
        if let Some(first) = quarantined() {
            return Err(restart(first));
        }
        let Some(root) = process_root() else {
            return Err(CollationError::Restart { memory: false });
        };
        prepared(i32::try_from(text.len()).map_err(|_| BudgetError::Overflow))?;
        let padded = prepared(text.len().checked_add(1).ok_or(BudgetError::Overflow))?;
        let mut source = prepared(BudgetedVec::try_with_capacity(
            &root,
            Category::Execution,
            padded,
        ))?;
        prepared(source.try_extend_from_slice(text))?;
        prepared(source.try_push(0))?;
        OPENED.with(|opened| {
            let mut opened = opened
                .try_borrow_mut()
                .map_err(|_| CollationError::Memory)?;
            if !opened.contains_key(locale) {
                prepared(
                    opened
                        .try_reserve(1)
                        .map_err(|_| BudgetError::AllocationFailed),
                )?;
                opened.insert(locale.to_owned(), [None, None]);
            }
            let Some(slots) = opened.get_mut(locale) else {
                return Err(CollationError::Memory);
            };
            let (slot, native) = match strength {
                Strength::Secondary => (&mut slots[0], icu46_sys::Strength::Secondary),
                Strength::Tertiary => (&mut slots[1], icu46_sys::Strength::Tertiary),
            };
            let collator = match slot {
                Some(collator) => collator,
                None => {
                    let name = std::ffi::CString::new(locale)
                        .map_err(|_| CollationError::Restart { memory: false })?;
                    let collator = Collator::open_managed_cancellable(&name, native, &root, &NEVER)
                        .map_err(runtime_error)?;
                    slot.insert(collator)
                }
            };
            write(collator, &source, &root)
        })
    }

    fn write(
        collator: &Collator,
        source: &[u16],
        root: &MemoryBudget,
    ) -> Result<BudgetedVec<u8>, CollationError> {
        let mut probe = [0; SORT_KEY_SLACK + 1];
        let needed = match collator
            .sort_key_managed_cancellable(source, &mut probe, root, &NEVER)
            .map_err(runtime_error)?
        {
            SortKeyWrite::Complete(n) | SortKeyWrite::Required(n) => n,
        };
        let capacity = prepared(
            needed
                .checked_add(SORT_KEY_SLACK)
                .ok_or(BudgetError::Overflow),
        )?;
        let mut output = prepared(
            BudgetedVec::try_with_capacity(root, Category::Execution, capacity)
                .and_then(|mut output| output.try_resize(capacity, 0).map(|()| output)),
        )?;
        match collator
            .sort_key_managed_cancellable(source, &mut output, root, &NEVER)
            .map_err(runtime_error)?
        {
            SortKeyWrite::Complete(n) if n > 0 => {
                output.truncate(n);
                Ok(output)
            }
            _ => Err(quarantined().map_or(CollationError::Restart { memory: false }, restart)),
        }
    }
}

#[cfg(all(test, feature = "icu"))]
mod tests {
    use super::*;

    fn u(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn ru_rules_from_module_semantics_10() {
        let c = IcuCollation::new("ru_RU");
        assert!(c.less(&u("a"), &u("A")) && !c.less(&u("A"), &u("a")));
        assert!(c.less(&u("a"), &u("B")) && c.less(&u("b"), &u("C")));
        assert!(c.less(&u("е"), &u("ё")) && c.less(&u("ё"), &u("ж")));
        assert!(c.less(&u("a-b"), &u("ab")));
        assert!(c.less(&u("z"), &u("а")));
        assert!(c.less(&u("10"), &u("9")));
        assert_eq!(c.compare_secondary(&u("abc"), &u("ABC")), Ordering::Equal);
        assert_eq!(c.compare_secondary(&u("е"), &u("ё")), Ordering::Less);
        assert_eq!(
            c.sort_key_for_sorting(&u("a  ")),
            c.sort_key_for_sorting(&u("a"))
        );
        assert_ne!(
            c.sort_key(Strength::Tertiary, &u("a ")),
            c.sort_key(Strength::Tertiary, &u("a"))
        );
    }

    #[test]
    fn sort_key_for_sorting_trims_trailing_spaces_only() {
        let c = IcuCollation::new("ru_RU");
        let k = |s: &str| c.sort_key_for_sorting(&u(s));
        assert_ne!(k(" a"), k("a"));
        assert_ne!(k("a\t"), k("a"));
        assert_eq!(k(""), k("   "));
    }

    #[test]
    fn usable_from_threads() {
        let c = std::sync::Arc::new(IcuCollation::new("ru_RU"));
        let hs: Vec<_> = (0..4)
            .map(|_| {
                let c = c.clone();
                std::thread::spawn(move || c.less(&u("a"), &u("b")))
            })
            .collect();
        assert!(hs.into_iter().all(|h| h.join().unwrap()));
    }

    #[test]
    fn bad_locale_falls_back_and_never_panics() {
        let c = IcuCollation::new("zz_ZZ_nonexistent");
        assert!(!c.sort_key(Strength::Tertiary, &u("a")).is_empty());
        assert_eq!(fallback_key(&[0x0102]), vec![1, 2, 0]);
    }
}
