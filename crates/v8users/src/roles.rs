use std::collections::BTreeMap;

use crate::{Error, Uuid16};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoleSets {
    by_set: BTreeMap<Vec<Uuid16>, u32>,
    max: u32,
}

fn key(roles: &[Uuid16]) -> Vec<Uuid16> {
    let mut k = roles.to_vec();
    k.sort_unstable();
    k.dedup();
    k
}

impl RoleSets {
    pub fn from_rows(rows: &[(Vec<Uuid16>, u32)], counter: u32) -> RoleSets {
        let mut s = RoleSets {
            by_set: BTreeMap::new(),
            max: counter,
        };
        for (roles, n) in rows {
            s.by_set.entry(key(roles)).or_insert(*n);
            s.max = s.max.max(*n);
        }
        s
    }

    pub fn number_for(&mut self, roles: &[Uuid16]) -> Result<u32, Error> {
        let k = key(roles);
        if let Some(&n) = self.by_set.get(&k) {
            return Ok(n);
        }
        let n = self
            .max
            .checked_add(1)
            .ok_or(Error::RoleSetNumbersExhausted)?;
        self.max = n;
        self.by_set.insert(k, n);
        Ok(n)
    }

    pub fn max(&self) -> u32 {
        self.max
    }
}
