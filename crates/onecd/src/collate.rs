#[cfg(feature = "icu")]
use std::collections::HashMap;
#[cfg(feature = "icu")]
use std::collections::hash_map::Entry;
#[cfg(feature = "icu")]
use std::sync::{Mutex, OnceLock, PoisonError};

use crate::error::Result;
#[cfg(feature = "icu")]
use crate::error::{Error, Location, Unsupported};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Strength {
    Secondary,
    Tertiary,
}

pub trait Collate: Send + Sync {
    fn sort_key(&self, locale: &str, strength: Strength, text: &[u16]) -> Result<Vec<u8>>;
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
