use std::cmp::Ordering;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Strength {
    Secondary,
    Tertiary,
}

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
    use super::{Collation, Strength, fallback_key};
    use std::cell::RefCell;
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
        fn sort_key(&self, strength: Strength, text: &[u16]) -> Vec<u8> {
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

        fn sort_key_len(&self, strength: Strength, text: &[u16]) -> Option<usize> {
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
