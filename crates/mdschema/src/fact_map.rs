use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::borrow::Borrow;
use std::ops::Index;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactMap<K, V> {
    pub(crate) rows: Vec<(K, V)>,
}
impl<K, V> FactMap<K, V> {
    pub const fn new() -> Self {
        Self { rows: Vec::new() }
    }
    pub fn len(&self) -> usize {
        self.rows.len()
    }
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = (&K, &V)> + ExactSizeIterator {
        self.rows.iter().map(|(k, v)| (k, v))
    }
    pub fn keys(&self) -> impl DoubleEndedIterator<Item = &K> + ExactSizeIterator {
        self.rows.iter().map(|(k, _)| k)
    }
    pub fn values(&self) -> impl DoubleEndedIterator<Item = &V> + ExactSizeIterator {
        self.rows.iter().map(|(_, v)| v)
    }
}
impl<K: Ord, V> FactMap<K, V> {
    pub(crate) fn insert(&mut self, key: K, value: V) {
        match self.rows.binary_search_by(|(k, _)| k.cmp(&key)) {
            Ok(i) => {
                if let Some(slot) = self.rows.get_mut(i) {
                    slot.1 = value
                }
            }
            Err(i) => self.rows.insert(i, (key, value)),
        }
    }

    pub(crate) fn from_rows(mut rows: Vec<(K, V)>) -> Self {
        rows.sort_by(|(a, _), (b, _)| a.cmp(b));
        rows.dedup_by(|a, b| {
            if a.0 == b.0 {
                std::mem::swap(&mut a.1, &mut b.1);
                true
            } else {
                false
            }
        });
        Self { rows }
    }
    pub(crate) fn from_unique_rows(mut rows: Vec<(K, V)>) -> Self {
        rows.sort_unstable_by(|(a, _), (b, _)| a.cmp(b));
        Self { rows }
    }
    pub fn get<Q: Ord + ?Sized>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
    {
        let i = self
            .rows
            .binary_search_by(|(k, _)| k.borrow().cmp(key))
            .ok()?;
        self.rows.get(i).map(|(_, v)| v)
    }
    pub fn contains_key<Q: Ord + ?Sized>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
    {
        self.get(key).is_some()
    }
}
impl<K, V> Default for FactMap<K, V> {
    fn default() -> Self {
        Self::new()
    }
}
impl<K: Ord, V> FromIterator<(K, V)> for FactMap<K, V> {
    fn from_iter<T: IntoIterator<Item = (K, V)>>(v: T) -> Self {
        Self::from_rows(v.into_iter().collect())
    }
}
impl<K, V> IntoIterator for FactMap<K, V> {
    type Item = (K, V);
    type IntoIter = std::vec::IntoIter<(K, V)>;
    fn into_iter(self) -> Self::IntoIter {
        self.rows.into_iter()
    }
}
type PairRef<'a, K, V> = (&'a K, &'a V);
type PairIter<'a, K, V> =
    std::iter::Map<std::slice::Iter<'a, (K, V)>, fn(&'a (K, V)) -> PairRef<'a, K, V>>;
fn pair<K, V>(p: &(K, V)) -> (&K, &V) {
    (&p.0, &p.1)
}
impl<'a, K, V> IntoIterator for &'a FactMap<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = PairIter<'a, K, V>;
    fn into_iter(self) -> Self::IntoIter {
        self.rows.iter().map(pair::<K, V>)
    }
}
impl<K: Ord, V, Q: Ord + ?Sized> Index<&Q> for FactMap<K, V>
where
    K: Borrow<Q>,
{
    type Output = V;
    #[expect(
        clippy::panic,
        reason = "Index requires a missing-key panic; get is the fallible interface"
    )]
    fn index(&self, key: &Q) -> &V {
        match self.get(key) {
            Some(v) => v,
            None => panic!("missing fact key"),
        }
    }
}
impl<K: Serialize, V: Serialize> Serialize for FactMap<K, V> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_map(self.iter())
    }
}
impl<'de, K: Deserialize<'de> + Ord, V: Deserialize<'de>> Deserialize<'de> for FactMap<K, V> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Rows<K, V>(std::marker::PhantomData<(K, V)>);
        impl<'de, K: Deserialize<'de> + Ord, V: Deserialize<'de>> serde::de::Visitor<'de> for Rows<K, V> {
            type Value = FactMap<K, V>;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("fact map")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut input: A,
            ) -> Result<Self::Value, A::Error> {
                let mut rows = Vec::new();
                while let Some(p) = input.next_entry()? {
                    rows.push(p)
                }
                Ok(FactMap::from_rows(rows))
            }
        }
        d.deserialize_map(Rows(std::marker::PhantomData))
    }
}

impl<K: Ord, V: PartialEq> PartialEq<std::collections::BTreeMap<K, V>> for FactMap<K, V> {
    fn eq(&self, other: &std::collections::BTreeMap<K, V>) -> bool {
        self.len() == other.len() && self.iter().all(|(k, v)| other.get(k) == Some(v))
    }
}
