use std::collections::{BTreeMap, HashMap};
use std::ops::Bound;
use std::sync::Arc;

use bsl_prim::fold_name;
use memory_budget::{Evictor, Reservation};
use query_ir::host::TxnId;
use query_ir::{ExecError, RuntimeError, TempSchema, TempSchemas};

use crate::coll::ExecCollation;
use crate::key::{Key, try_key_of};
use crate::model::{keys_model, rows_model, try_key_sizes, vec_block};
use crate::ops::row_bytes;
use crate::source::{ExecEnv, Row};

type Entries = Vec<(Vec<Key>, u32)>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TempTableId(pub u32);

#[derive(Clone, Debug)]
pub struct TempIndex {
    pub cols: Vec<u16>,
    pub name: String,
    pub unique: bool,
    entries: Vec<(Vec<Key>, u32)>,
    bytes: u64,
}

impl TempIndex {
    fn new(cols: Vec<u16>, unique: bool, schema: &TempSchema) -> TempIndex {
        let name = cols
            .iter()
            .filter_map(|c| schema.columns.get(usize::from(*c)).map(|c| c.name.as_str()))
            .collect::<Vec<_>>()
            .join(",");
        TempIndex {
            cols,
            name,
            unique,
            entries: Vec::new(),
            bytes: 0,
        }
    }

    fn entries_bound(
        &self,
        rows: &[Row],
        first: usize,
        coll: ExecCollation<'_>,
    ) -> Result<u64, ExecError> {
        let null = query_ir::QValue::Null;
        let n = rows.len().saturating_sub(first);
        let mut kept = vec_block(n, std::mem::size_of::<(Vec<Key>, u32)>());
        let mut peak = 0u64;
        for row in rows.iter().skip(first) {
            kept += vec_block(self.cols.len(), std::mem::size_of::<Key>());
            for c in &self.cols {
                let (k, p) = try_key_sizes(row.get(usize::from(*c)).unwrap_or(&null), coll)?;
                kept += k;
                peak = peak.max(p);
            }
        }
        Ok(kept + peak)
    }

    fn entries_of(
        &self,
        rows: &[Row],
        first: usize,
        coll: ExecCollation<'_>,
    ) -> Result<(Entries, u64), ExecError> {
        let null = try_key_of(&query_ir::QValue::Null, coll)?;
        let n = rows.len().saturating_sub(first);
        let mut bytes = vec_block(n, std::mem::size_of::<(Vec<Key>, u32)>());
        let mut out = Vec::with_capacity(n);
        for (i, row) in rows.iter().enumerate().skip(first) {
            let keys: Vec<Key> = self
                .cols
                .iter()
                .map(|c| match row.get(usize::from(*c)) {
                    Some(v) => try_key_of(v, coll),
                    None => Ok(null.clone()),
                })
                .collect::<Result<_, ExecError>>()?;
            bytes += keys_model(&keys);
            out.push((keys, u32::try_from(i).unwrap_or(u32::MAX)));
        }
        Ok((out, bytes))
    }

    fn has_duplicate(&self, new: &mut [(Vec<Key>, u32)]) -> bool {
        if !self.unique {
            return false;
        }
        new.sort_unstable();
        new.windows(2).any(|w| matches!(w, [a, b] if a.0 == b.0))
            || new.iter().any(|(k, _)| {
                self.entries
                    .binary_search_by(|e| e.0.as_slice().cmp(k.as_slice()))
                    .is_ok()
            })
    }

    fn apply_growth(&self, new: usize) -> u64 {
        if self.entries.is_empty() {
            0
        } else {
            vec_block(
                self.entries.len() + new,
                std::mem::size_of::<(Vec<Key>, u32)>(),
            )
        }
    }

    fn apply(&mut self, mut new: Vec<(Vec<Key>, u32)>, bytes: u64) -> u64 {
        self.bytes += bytes;
        let elem = std::mem::size_of::<(Vec<Key>, u32)>();
        let freed = if self.entries.is_empty() {
            self.entries = new;
            0
        } else {
            let old = vec_block(self.entries.capacity(), elem) + vec_block(new.capacity(), elem);
            let mut merged = Vec::with_capacity(self.entries.len() + new.len());
            merged.append(&mut self.entries);
            merged.append(&mut new);
            self.entries = merged;
            self.bytes = self.bytes.saturating_sub(old);
            self.bytes += vec_block(self.entries.capacity(), elem);
            old
        };
        self.entries.sort_unstable();
        freed
    }

    pub fn span(&self, lo: Bound<&[Key]>, hi: Bound<&[Key]>) -> (usize, usize) {
        let prefix_cmp = |e: &(Vec<Key>, u32), b: &[Key]| {
            let n = b.len().min(e.0.len());
            e.0.get(..n).unwrap_or(&[]).cmp(b)
        };
        let start = match lo {
            Bound::Unbounded => 0,
            Bound::Included(b) => self.entries.partition_point(|e| prefix_cmp(e, b).is_lt()),
            Bound::Excluded(b) => self.entries.partition_point(|e| prefix_cmp(e, b).is_le()),
        };
        let end = match hi {
            Bound::Unbounded => self.entries.len(),
            Bound::Included(b) => self.entries.partition_point(|e| prefix_cmp(e, b).is_le()),
            Bound::Excluded(b) => self.entries.partition_point(|e| prefix_cmp(e, b).is_lt()),
        };
        (start, end.max(start))
    }

    pub fn rows_in(&self, start: usize, end: usize) -> impl Iterator<Item = usize> + '_ {
        self.entries
            .get(start..end)
            .unwrap_or(&[])
            .iter()
            .map(|e| e.1 as usize)
    }
}

#[derive(Clone, Debug)]
pub struct TempTable {
    pub schema: Arc<TempSchema>,
    pub rows: Vec<Row>,
    pub txn: Option<TxnId>,
    added: Vec<(TxnId, usize, usize)>,
    indexes: Vec<TempIndex>,
    bytes: u64,
}

impl TempTable {
    fn tracked(&self) -> bool {
        self.txn.is_some() || !self.added.is_empty()
    }

    pub fn indexes(&self) -> &[TempIndex] {
        &self.indexes
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}

#[derive(Default)]
pub struct TempTables {
    by_name: HashMap<String, TempTableId>,
    tables: BTreeMap<TempTableId, TempTable>,
    next: u32,
    bytes: u64,
    in_txn: usize,
    lease: Option<Arc<Reservation>>,
    evictor: Option<Arc<dyn Evictor + Send + Sync>>,
}

fn limit() -> ExecError {
    ExecError::Fatal("превышен лимит памяти временных таблиц".to_string())
}

impl TempTables {
    pub fn new() -> TempTables {
        TempTables::default()
    }

    pub fn with_lease(
        lease: Arc<Reservation>,
        evictor: Arc<dyn Evictor + Send + Sync>,
    ) -> TempTables {
        let mut t = TempTables::default();
        t.lease = Some(lease);
        t.evictor = Some(evictor);
        t
    }

    pub fn is_leased(&self) -> bool {
        self.lease.is_some()
    }

    fn take(&self, n: u64, env: &ExecEnv<'_>) -> Result<(), ExecError> {
        match &self.lease {
            Some(lease) => match &self.evictor {
                Some(e) => lease.try_grow_or_evict(n, e.as_ref()),
                None => lease.try_grow(n),
            }
            .map_err(ExecError::Memory),
            None => {
                if self.bytes.saturating_add(n) > env.mem_limit {
                    return Err(limit());
                }
                Ok(())
            }
        }
    }

    fn give(&self, n: u64) {
        if let Some(lease) = &self.lease
            && n > 0
        {
            let _ = lease.shrink(n);
        }
    }

    pub fn forget_rolled_back(&mut self, rolled_back: &dyn Fn(TxnId) -> bool) {
        if self.in_txn == 0 {
            return;
        }
        let gone: Vec<TempTableId> = self
            .tables
            .iter()
            .inspect(|_| crate::probe::hit())
            .filter(|(_, t)| t.txn.is_some_and(rolled_back))
            .map(|(id, _)| *id)
            .collect();
        for id in gone {
            self.remove(id);
        }
        let leased = self.lease.is_some();
        let mut untracked = 0;
        let mut freed = 0;
        for t in self.tables.values_mut() {
            if !t.added.iter().any(|(x, _, _)| rolled_back(*x)) {
                continue;
            }
            let was = t.tracked();
            let mut keep = vec![true; t.rows.len()];
            for (x, first, n) in &t.added {
                if rolled_back(*x) {
                    for k in keep.iter_mut().skip(*first).take(*n) {
                        *k = false;
                    }
                }
            }
            let mut renumber = Vec::with_capacity(t.rows.len());
            let mut next = 0usize;
            for k in &keep {
                renumber.push(next);
                next += usize::from(*k);
            }
            let rows_before = if leased {
                rows_model(&t.rows)
            } else {
                t.rows.iter().map(|r| row_bytes(r)).sum::<u64>()
            };
            let mut flags = keep.iter();
            t.rows.retain(|_| flags.next().copied().unwrap_or(true));
            let rows_after = if leased {
                rows_model(&t.rows)
            } else {
                t.rows.iter().map(|r| row_bytes(r)).sum::<u64>()
            };
            let mut gone_bytes = rows_before.saturating_sub(rows_after);
            t.added = t
                .added
                .iter()
                .filter(|(x, _, _)| !rolled_back(*x))
                .map(|(x, first, n)| {
                    let kept = keep.iter().skip(*first).take(*n).filter(|k| **k).count();
                    (*x, renumber.get(*first).copied().unwrap_or(0), kept)
                })
                .collect();
            for ix in &mut t.indexes {
                let mut dropped = 0u64;
                ix.entries.retain_mut(|e| {
                    let i = e.1 as usize;
                    if keep.get(i).copied().unwrap_or(false) {
                        e.1 = u32::try_from(renumber.get(i).copied().unwrap_or(0))
                            .unwrap_or(u32::MAX);
                        true
                    } else {
                        dropped += keys_model(&e.0);
                        false
                    }
                });
                ix.bytes = ix.bytes.saturating_sub(dropped);
                gone_bytes += dropped;
            }
            t.bytes = t.bytes.saturating_sub(gone_bytes);
            freed += gone_bytes;
            if was && !t.tracked() {
                untracked += 1;
            }
        }
        self.bytes = self.bytes.saturating_sub(freed);
        self.in_txn = self.in_txn.saturating_sub(untracked);
        self.give(freed);
    }

    pub fn schemas(&self) -> &dyn TempSchemas {
        self
    }

    pub fn names(&self) -> Vec<String> {
        self.tables
            .values()
            .map(|t| t.schema.name.clone())
            .collect()
    }

    pub fn table(&self, name: &str) -> Option<&TempTable> {
        self.by_name
            .get(&fold_name(name))
            .and_then(|id| self.tables.get(id))
    }

    pub fn table_by_id(&self, id: TempTableId) -> Option<&TempTable> {
        self.tables.get(&id)
    }

    pub fn id_of(&self, name: &str) -> Option<TempTableId> {
        self.by_name.get(&fold_name(name)).copied()
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn len(&self) -> usize {
        self.tables.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tables.is_empty()
    }

    pub fn close(&mut self) {
        let bytes = self.bytes;
        self.tables = BTreeMap::new();
        self.by_name = HashMap::new();
        self.bytes = 0;
        self.in_txn = 0;
        self.give(bytes);
    }

    pub fn drop_table(&mut self, name: &str) -> bool {
        match self.id_of(name) {
            Some(id) => {
                self.remove(id);
                true
            }
            None => false,
        }
    }

    fn remove(&mut self, id: TempTableId) {
        if let Some(t) = self.tables.remove(&id) {
            self.by_name.remove(&fold_name(&t.schema.name));
            self.bytes = self.bytes.saturating_sub(t.bytes);
            if t.tracked() {
                self.in_txn = self.in_txn.saturating_sub(1);
            }
            let bytes = t.bytes;
            drop(t);
            self.give(bytes);
        }
    }

    pub fn put(
        &mut self,
        schema: Arc<TempSchema>,
        rows: Vec<Row>,
        txn: Option<TxnId>,
        index_sets: &[(Vec<usize>, bool)],
        env: &ExecEnv<'_>,
    ) -> Result<u64, ExecError> {
        let key = fold_name(&schema.name);
        if self.by_name.contains_key(&key) {
            return Err(ExecError::Fatal(format!(
                "временная таблица {} уже существует",
                schema.name
            )));
        }
        let managed = if index_sets.is_empty() {
            None
        } else {
            crate::coll::open(env)?
        };
        let coll = crate::coll::route(env, managed.as_ref());
        let rows_bytes = if self.lease.is_some() {
            rows_model(&rows)
        } else {
            rows.iter().map(|r| row_bytes(r)).sum::<u64>()
        };
        let fixed = 64 + 2 * schema.name.len() as u64;
        self.take(
            fixed + if self.lease.is_some() { 0 } else { rows_bytes },
            env,
        )?;
        let mut bytes = rows_bytes + fixed;
        let mut taken = fixed;
        let mut indexes = Vec::with_capacity(index_sets.len());
        for (set, unique) in index_sets {
            let mut ix = TempIndex::new(column_numbers(set), *unique, &schema);
            let bound = match ix.entries_bound(&rows, 0, coll) {
                Ok(bound) => bound + 64,
                Err(e) => {
                    self.give(taken);
                    return Err(e);
                }
            };
            if let Err(e) = self.take(bound, env) {
                self.give(taken);
                return Err(e);
            }
            taken += bound;
            let (mut entries, b) = match ix.entries_of(&rows, 0, coll) {
                Ok(built) => built,
                Err(e) => {
                    self.give(taken);
                    return Err(e);
                }
            };
            if ix.has_duplicate(&mut entries) {
                self.give(taken);
                return Err(ExecError::Runtime(RuntimeError::unique_index()));
            }
            ix.apply(entries, b);
            let actual = b + 64;
            self.give(bound.saturating_sub(actual));
            taken -= bound.saturating_sub(actual);
            bytes += actual;
            indexes.push(ix);
        }
        let n = rows.len() as u64;
        let id = TempTableId(self.next);
        self.next = self.next.wrapping_add(1);
        self.bytes += bytes;
        if txn.is_some() {
            self.in_txn += 1;
        }
        self.by_name.insert(key, id);
        self.tables.insert(
            id,
            TempTable {
                schema,
                rows,
                txn,
                added: Vec::new(),
                indexes,
                bytes,
            },
        );
        Ok(n)
    }

    pub fn append(
        &mut self,
        name: &str,
        rows: Vec<Row>,
        txn: Option<TxnId>,
        index_sets: &[(Vec<usize>, bool)],
        env: &ExecEnv<'_>,
    ) -> Result<u64, ExecError> {
        let Some(id) = self.id_of(name) else {
            return Err(ExecError::Fatal(format!(
                "временная таблица {name} не существует"
            )));
        };
        let leased = self.lease.is_some();
        let lease = self.lease.clone();
        let evictor = self.evictor.clone();
        let used = self.bytes;
        let Some(t) = self.tables.get_mut(&id) else {
            return Err(ExecError::Fatal("временная таблица пропала".to_string()));
        };
        let managed = if index_sets.is_empty() && t.indexes.is_empty() {
            None
        } else {
            crate::coll::open(env)?
        };
        let coll = crate::coll::route(env, managed.as_ref());
        let first = t.rows.len();
        let n = rows.len() as u64;
        let rows_bytes = if leased {
            rows_model(&rows)
        } else {
            rows.iter().map(|r| row_bytes(r)).sum::<u64>()
        };
        let take = |bytes: u64, taken: &mut u64| -> Result<(), ExecError> {
            match (&lease, &evictor) {
                (Some(l), Some(e)) => l
                    .try_grow_or_evict(bytes, e.as_ref())
                    .map_err(ExecError::Memory)?,
                (Some(l), None) => l.try_grow(bytes).map_err(ExecError::Memory)?,
                (None, _) => {
                    if used.saturating_add(*taken).saturating_add(bytes) > env.mem_limit {
                        return Err(limit());
                    }
                }
            }
            *taken += bytes;
            Ok(())
        };
        let give = |bytes: u64| {
            if let Some(l) = &lease
                && bytes > 0
            {
                let _ = l.shrink(bytes);
            }
        };
        let mut taken = 0u64;
        let elem = std::mem::size_of::<Row>();
        let old_rows_vec = vec_block(t.rows.capacity(), elem);
        let new_rows_vec = vec_block((t.rows.capacity() * 2).max(first + rows.len()), elem);
        let mut add = rows_bytes;
        if let Err(e) = take(
            if leased { 0 } else { rows_bytes }
                + if first + rows.len() > t.rows.capacity() {
                    new_rows_vec
                } else {
                    0
                },
            &mut taken,
        ) {
            give(taken);
            return Err(e);
        }
        let mut all = std::mem::take(&mut t.rows);
        let grew = first + rows.len() > all.capacity();
        all.extend(rows);
        let mut kept_growth = 0;
        if grew {
            give(old_rows_vec);
            kept_growth = new_rows_vec;
            let delta = new_rows_vec.saturating_sub(old_rows_vec);
            t.bytes += delta;
            self.bytes += delta;
        }
        let mut new_entries = Vec::with_capacity(t.indexes.len());
        for ix in &t.indexes {
            let built = ix
                .entries_bound(&all, first, coll)
                .map(|bound| bound + ix.apply_growth(all.len() - first))
                .and_then(|bound| take(bound, &mut taken).map(|()| bound))
                .and_then(|bound| Ok((bound, ix.entries_of(&all, first, coll)?)));
            let (bound, (mut e, b)) = match built {
                Ok(built) => built,
                Err(e) => {
                    all.truncate(first);
                    t.rows = all;
                    give(taken - kept_growth);
                    return Err(e);
                }
            };
            if ix.has_duplicate(&mut e) {
                all.truncate(first);
                t.rows = all;
                give(taken - kept_growth);
                return Err(ExecError::Runtime(RuntimeError::unique_index()));
            }
            give(bound.saturating_sub(b + ix.apply_growth(e.len())));
            taken -= bound.saturating_sub(b + ix.apply_growth(e.len()));
            add += b;
            new_entries.push((e, b));
        }
        let mut fresh = Vec::with_capacity(index_sets.len());
        for (set, unique) in index_sets {
            let ix = TempIndex::new(column_numbers(set), *unique, &t.schema);
            let built = ix
                .entries_bound(&all, 0, coll)
                .map(|bound| bound + 64)
                .and_then(|bound| take(bound, &mut taken).map(|()| bound))
                .and_then(|bound| Ok((bound, ix.entries_of(&all, 0, coll)?)));
            let (bound, (mut e, b)) = match built {
                Ok(built) => built,
                Err(e) => {
                    all.truncate(first);
                    t.rows = all;
                    give(taken - kept_growth);
                    return Err(e);
                }
            };
            if ix.has_duplicate(&mut e) {
                all.truncate(first);
                t.rows = all;
                give(taken - kept_growth);
                return Err(ExecError::Runtime(RuntimeError::unique_index()));
            }
            give(bound.saturating_sub(b + 64));
            taken -= bound.saturating_sub(b + 64);
            add += b + 64;
            fresh.push((ix, e, b));
        }
        t.rows = all;
        for (ix, (e, b)) in t.indexes.iter_mut().zip(new_entries) {
            let merged_growth = ix.apply_growth(e.len());
            let freed = ix.apply(e, b);
            add += merged_growth;
            add = add.saturating_sub(freed);
            give(freed);
        }
        for (mut ix, e, b) in fresh {
            ix.apply(e, b);
            t.indexes.push(ix);
        }
        t.bytes += add;
        self.bytes += add;
        if let Some(x) = txn
            && n > 0
        {
            let was = t.tracked();
            t.added.push((x, first, n as usize));
            if !was {
                self.in_txn += 1;
            }
        }
        Ok(n)
    }
}

impl Drop for TempTables {
    fn drop(&mut self) {
        let bytes = std::mem::take(&mut self.bytes);
        self.tables = BTreeMap::new();
        self.give(bytes);
    }
}

fn column_numbers(set: &[usize]) -> Vec<u16> {
    set.iter().filter_map(|c| u16::try_from(*c).ok()).collect()
}

impl TempSchemas for TempTables {
    fn get(&self, name: &str) -> Option<Arc<TempSchema>> {
        self.table(name).map(|t| t.schema.clone())
    }
}
