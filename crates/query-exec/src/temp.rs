pub(crate) mod provenance;
use provenance::{OriginLedger, TempWriteStage};
use query_ir::bind::temp_provenance::MaterializedOrigins;

use std::collections::{BTreeMap, HashMap};
use std::ops::Bound;
use std::sync::Arc;

use bsl_prim::fold_name;
use memory_budget::{Evictor, Reservation};
use query_ir::host::TxnId;
use query_ir::{ExecError, RuntimeError, TempSchema, TempSchemas};

use crate::coll::ExecCollation;
use crate::key::{Key, try_key_of};
use crate::model::{
    invalid_size, keys_model, size_add, size_block, size_vec, temp_name_table_model,
    temp_tree_model, try_key_sizes, vec_block,
};
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

struct PreparedIndex {
    incoming: Entries,
    merged: Option<Entries>,
    schedule: Vec<bool>,
    next_bytes: u64,
}
impl PreparedIndex {
    fn prepare(
        index: &TempIndex,
        mut incoming: Entries,
        bytes: u64,
        growth: u64,
        work: &mut crate::model::LedgerWork<'_, '_>,
    ) -> Result<Self, ExecError> {
        incoming.sort_unstable();
        let freed = index.apply_freed(&incoming)?;
        let next_bytes = size_add(index.bytes, size_add(bytes, growth)?)?
            .checked_sub(freed)
            .ok_or_else(invalid_size)?;
        let (merged, schedule) = if !index.entries.is_empty() && !incoming.is_empty() {
            let count = index
                .entries
                .len()
                .checked_add(incoming.len())
                .ok_or_else(invalid_size)?;
            let mut schedule = Vec::with_capacity(count);
            let (mut old, mut new) = (0usize, 0usize);
            while old < index.entries.len() || new < incoming.len() {
                work.step()?;
                let take_old = match (index.entries.get(old), incoming.get(new)) {
                    (Some(a), Some(b)) => a <= b,
                    (Some(_), None) => true,
                    _ => false,
                };
                schedule.push(take_old);
                if take_old { old += 1 } else { new += 1 }
            }
            (
                Some(Vec::with_capacity(count.max(index.entries.capacity()))),
                schedule,
            )
        } else {
            (None, Vec::new())
        };
        Ok(Self {
            incoming,
            merged,
            schedule,
            next_bytes,
        })
    }
    fn commit(self, index: &mut TempIndex) {
        let Self {
            mut incoming,
            merged,
            schedule,
            next_bytes,
        } = self;
        if let Some(mut merged) = merged {
            let mut old = std::mem::take(&mut index.entries).into_iter();
            let mut new = incoming.into_iter();
            for take_old in schedule {
                if let Some(entry) = if take_old { old.next() } else { new.next() } {
                    merged.push(entry);
                }
            }
            index.entries = merged;
        } else if !incoming.is_empty() {
            if index.entries.capacity() >= incoming.len() {
                index.entries.append(&mut incoming);
            } else {
                index.entries = incoming;
            }
        }
        index.bytes = next_bytes;
    }
}

impl TempIndex {
    fn descriptor_layout(set: &[usize], schema: &TempSchema) -> Result<(usize, usize), ExecError> {
        let mut count = 0usize;
        let mut name_bytes = 0usize;
        let mut named = false;
        for column in set.iter().filter_map(|c| u16::try_from(*c).ok()) {
            count = count.checked_add(1).ok_or_else(invalid_size)?;
            if let Some(column) = schema.columns.get(usize::from(column)) {
                name_bytes = name_bytes
                    .checked_add(usize::from(named))
                    .and_then(|n| n.checked_add(column.name.len()))
                    .ok_or_else(invalid_size)?;
                named = true;
            }
        }
        std::alloc::Layout::array::<u16>(count).map_err(|_| invalid_size())?;
        std::alloc::Layout::array::<u8>(name_bytes).map_err(|_| invalid_size())?;
        Ok((count, name_bytes))
    }

    fn descriptor_model(set: &[usize], schema: &TempSchema) -> Result<u64, ExecError> {
        let (count, name_bytes) = Self::descriptor_layout(set, schema)?;
        size_add(
            size_vec(count, std::mem::size_of::<u16>())?,
            size_block(name_bytes as u64)?,
        )
    }

    fn new(set: &[usize], unique: bool, schema: &TempSchema) -> Result<TempIndex, ExecError> {
        let (count, name_bytes) = Self::descriptor_layout(set, schema)?;
        let mut cols = Vec::with_capacity(count);
        let mut name = String::with_capacity(name_bytes);
        let mut named = false;
        for column in set.iter().filter_map(|c| u16::try_from(*c).ok()) {
            cols.push(column);
            if let Some(column) = schema.columns.get(usize::from(column)) {
                if named {
                    name.push(',');
                }
                name.push_str(&column.name);
                named = true;
            }
        }
        Ok(TempIndex {
            cols,
            name,
            unique,
            entries: Vec::new(),
            bytes: 0,
        })
    }

    fn entries_bound<'r>(
        &self,
        rows: impl Iterator<Item = &'r Row>,
        n: usize,
        coll: ExecCollation<'_>,
        work: &mut crate::model::LedgerWork<'_, '_>,
    ) -> Result<u64, ExecError> {
        let null = query_ir::QValue::Null;
        let mut kept = size_vec(n, std::mem::size_of::<(Vec<Key>, u32)>())?;
        let mut peak = 0u64;
        for row in rows {
            work.step()?;
            kept = size_add(kept, size_vec(self.cols.len(), std::mem::size_of::<Key>())?)?;
            for c in &self.cols {
                let (k, p) = try_key_sizes(row.get(usize::from(*c)).unwrap_or(&null), coll)?;
                kept = size_add(kept, k)?;
                peak = peak.max(p);
            }
        }
        size_add(kept, peak)
    }

    fn entries_of<'r>(
        &self,
        rows: impl Iterator<Item = &'r Row>,
        n: usize,
        first: usize,
        coll: ExecCollation<'_>,
        work: &mut crate::model::LedgerWork<'_, '_>,
    ) -> Result<(Entries, u64), ExecError> {
        let null = try_key_of(&query_ir::QValue::Null, coll)?;
        let mut bytes = size_vec(n, std::mem::size_of::<(Vec<Key>, u32)>())?;
        let mut out = Vec::with_capacity(n);
        for (i, row) in rows.enumerate() {
            work.step()?;
            let mut keys = Vec::with_capacity(self.cols.len());
            for column in &self.cols {
                keys.push(match row.get(usize::from(*column)) {
                    Some(value) => try_key_of(value, coll)?,
                    None => null.clone(),
                });
            }
            bytes = size_add(bytes, keys_model(&keys))?;
            out.push((keys, u32::try_from(first + i).unwrap_or(u32::MAX)));
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

    fn apply_growth(&self, new: usize) -> Result<u64, ExecError> {
        if self.entries.is_empty() || new == 0 {
            Ok(0)
        } else {
            let capacity = self
                .entries
                .len()
                .checked_add(new)
                .ok_or_else(invalid_size)?
                .max(self.entries.capacity());
            std::alloc::Layout::array::<(Vec<Key>, u32)>(capacity).map_err(|_| invalid_size())?;
            size_vec(capacity, std::mem::size_of::<(Vec<Key>, u32)>())
        }
    }

    fn apply_freed(&self, new: &Entries) -> Result<u64, ExecError> {
        let elem = std::mem::size_of::<(Vec<Key>, u32)>();
        if new.is_empty() {
            return Ok(0);
        }
        if self.entries.is_empty() {
            return size_vec(
                if self.entries.capacity() >= new.len() {
                    new.capacity()
                } else {
                    self.entries.capacity()
                },
                elem,
            );
        }
        size_add(
            size_vec(self.entries.capacity(), elem)?,
            size_vec(new.capacity(), elem)?,
        )
    }

    fn apply(&mut self, mut new: Vec<(Vec<Key>, u32)>, bytes: u64) -> u64 {
        if new.is_empty() {
            return 0;
        }
        self.bytes += bytes;
        let elem = std::mem::size_of::<(Vec<Key>, u32)>();
        let freed = if self.entries.is_empty() {
            let freed = if self.entries.capacity() >= new.len() {
                let freed = vec_block(new.capacity(), elem);
                self.entries.append(&mut new);
                freed
            } else {
                let freed = vec_block(self.entries.capacity(), elem);
                self.entries = new;
                freed
            };
            self.bytes -= freed;
            freed
        } else {
            let old = vec_block(self.entries.capacity(), elem) + vec_block(new.capacity(), elem);
            let mut merged =
                Vec::with_capacity((self.entries.len() + new.len()).max(self.entries.capacity()));
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

#[derive(Debug)]
pub struct TempTable {
    pub schema: Arc<TempSchema>,
    pub rows: Vec<Row>,
    pub txn: Option<TxnId>,
    added: Vec<(TxnId, usize, usize)>,
    indexes: Vec<TempIndex>,
    row_ledger: crate::model::RowLedger,
    bytes: u64,
    schema_bytes: u64,
    origins: OriginLedger,
}

impl Clone for TempTable {
    fn clone(&self) -> Self {
        Self {
            schema: self.schema.clone(),
            rows: self.rows.clone(),
            txn: self.txn,
            added: self.added.clone(),
            indexes: self.indexes.clone(),
            row_ledger: self.row_ledger.clone(),
            bytes: self.bytes - self.origins.bytes,
            schema_bytes: self.schema_bytes,
            origins: OriginLedger::default(),
        }
    }
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
    name_bytes: u64,
    name_capacity: usize,
    tree_bytes: u64,
    tree_high_water: usize,
    in_txn: usize,
    lease: Option<Arc<Reservation>>,
    evictor: Option<Arc<dyn Evictor + Send + Sync>>,
}

fn folded_name_char(c: char) -> char {
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(one), None) => one,
        _ => c,
    }
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

    fn reserve_insert(&mut self, table_bytes: u64, env: &ExecEnv<'_>) -> Result<(), ExecError> {
        let entries = self.tables.len().checked_add(1).ok_or_else(invalid_size)?;
        let high_water = self.tree_high_water.max(entries);
        let tree_bytes = temp_tree_model(high_water)?;
        let tree_delta = tree_bytes
            .checked_sub(self.tree_bytes)
            .ok_or_else(invalid_size)?;
        let grow_names = entries > self.by_name.capacity();
        let names_bound = if grow_names {
            let capacity = self
                .name_capacity
                .checked_mul(2)
                .ok_or_else(invalid_size)?
                .max(entries);
            temp_name_table_model(capacity)?
        } else {
            0
        };
        let names_delta = if grow_names {
            names_bound
                .checked_sub(self.name_bytes)
                .ok_or_else(invalid_size)?
        } else {
            0
        };
        let transient = size_add(names_bound, tree_delta)?;
        size_add(
            size_add(size_add(self.bytes, tree_delta)?, names_delta)?,
            table_bytes,
        )?;
        self.take(
            if self.lease.is_some() {
                transient
            } else {
                size_add(transient, table_bytes)?
            },
            env,
        )?;
        if grow_names {
            if self.by_name.try_reserve(1).is_err() {
                self.give(transient);
                return Err(invalid_size());
            }
            let capacity = self.by_name.capacity().max(self.name_capacity);
            let actual = if capacity > self.name_capacity {
                names_bound
            } else {
                self.name_bytes
            };
            let released = names_bound - (actual - self.name_bytes);
            self.bytes += actual - self.name_bytes;
            self.name_bytes = actual;
            self.name_capacity = capacity;
            self.give(released);
        }
        self.bytes += tree_delta;
        self.tree_bytes = tree_bytes;
        self.tree_high_water = high_water;
        Ok(())
    }

    fn free_empty_containers(&mut self) {
        if !self.tables.is_empty() {
            return;
        }
        drop(std::mem::take(&mut self.by_name));
        drop(std::mem::take(&mut self.tables));
        let freed = self.name_bytes + self.tree_bytes;
        self.name_bytes = 0;
        self.name_capacity = 0;
        self.tree_bytes = 0;
        self.tree_high_water = 0;
        self.bytes -= freed;
        self.give(freed);
    }

    pub fn forget_rolled_back(&mut self, rolled_back: &dyn Fn(TxnId) -> bool) {
        if self.in_txn == 0 {
            return;
        }
        while let Some(id) = self
            .tables
            .iter()
            .inspect(|_| crate::probe::hit())
            .find(|(_, table)| table.txn.is_some_and(rolled_back))
            .map(|(id, _)| *id)
        {
            self.remove(id);
        }
        let mut untracked = 0;
        let mut freed = 0;
        for t in self.tables.values_mut() {
            if !t.added.iter().any(|(txn, _, _)| rolled_back(*txn)) {
                continue;
            }
            let was = t.tracked();
            let old_len = t.rows.len();
            let mut ranges = t.added.iter().peekable();
            let mut end = 0;
            let mut removed = false;
            let ledger = &mut t.row_ledger;
            let mut gone_bytes = ledger.prepare_rollback(&t.rows, &mut |row_index| {
                while ranges
                    .peek()
                    .is_some_and(|(_, first, _)| *first <= row_index)
                {
                    if let Some(&(txn, first, count)) = ranges.next() {
                        crate::probe::hit();
                        let start = first.min(old_len);
                        end = start + count.min(old_len - start);
                        removed = rolled_back(txn);
                    }
                }
                removed && row_index < end
            });
            gone_bytes += t.origins.rollback(ledger);
            let mut charges = ledger.rows.iter();
            t.rows.retain(|_| {
                crate::probe::hit();
                charges
                    .next()
                    .is_some_and(|charge| charge.new_row != usize::MAX)
            });
            let kept = t.rows.len();
            for ix in &mut t.indexes {
                let mut dropped = 0;
                ix.entries.retain_mut(|entry| {
                    crate::probe::hit();
                    let new_row = ledger
                        .rows
                        .get(entry.1 as usize)
                        .map_or(usize::MAX, |c| c.new_row);
                    if new_row == usize::MAX {
                        dropped += keys_model(&entry.0);
                        false
                    } else {
                        entry.1 = new_row as u32;
                        true
                    }
                });
                ix.bytes -= dropped;
                gone_bytes += dropped;
            }
            t.added.retain_mut(|(txn, first, _)| {
                crate::probe::hit();
                if rolled_back(*txn) {
                    return false;
                }
                *first = ledger
                    .rows
                    .get(*first)
                    .map_or(kept, |charge| charge.new_row);
                true
            });
            ledger.rows.retain(|charge| {
                crate::probe::hit();
                charge.new_row != usize::MAX
            });
            t.bytes -= gone_bytes;
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

    #[doc(hidden)]
    pub unsafe fn origin_provider(
        &self,
    ) -> impl query_ir::bind::temp_provenance::TempOriginProvider + '_ {
        provenance::OriginAdapter::new(self)
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
        let tables = std::mem::take(&mut self.tables);
        drop(std::mem::take(&mut self.by_name));
        self.in_txn = 0;
        let mut freed = 0;
        for table in tables.into_values() {
            freed += destroy_table(table);
        }
        freed += self.name_bytes + self.tree_bytes;
        self.name_bytes = 0;
        self.name_capacity = 0;
        self.tree_bytes = 0;
        self.tree_high_water = 0;
        self.bytes = 0;
        self.give(freed);
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
            self.by_name.retain(|key, value| {
                *value != id || !key.chars().eq(t.schema.name.chars().map(folded_name_char))
            });
            self.bytes = self.bytes.saturating_sub(t.bytes);
            if t.tracked() {
                self.in_txn = self.in_txn.saturating_sub(1);
            }
            self.give(destroy_table(t));
            self.free_empty_containers();
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
        self.put_with_origins(schema, rows, txn, index_sets, env, None)
    }

    pub(crate) fn put_with_origins(
        &mut self,
        schema: Arc<TempSchema>,
        rows: Vec<Row>,
        txn: Option<TxnId>,
        index_sets: &[(Vec<usize>, bool)],
        env: &ExecEnv<'_>,
        proof: Option<MaterializedOrigins>,
    ) -> Result<u64, ExecError> {
        let stage = TempWriteStage::new(self, env);
        let key_len = schema.name.chars().try_fold(0usize, |bytes, c| {
            bytes
                .checked_add(folded_name_char(c).len_utf8())
                .ok_or_else(invalid_size)
        })?;
        std::alloc::Layout::array::<u8>(key_len).map_err(|_| invalid_size())?;
        let key_bytes = size_block(key_len as u64)?;
        let leased = self.lease.is_some();
        let built = (|| -> Result<_, ExecError> {
            let origins = OriginLedger::for_put(proof, rows.len(), txn, &stage)?;
            stage.grow(key_bytes)?;
            let mut taken = stage.bytes();
            let mut key = String::with_capacity(key_len);
            key.extend(schema.name.chars().map(folded_name_char));
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
            let mut work = crate::model::LedgerWork::new(env);
            let row_ledger = crate::model::RowLedger::build_checked(
                &rows,
                leased,
                &mut |bytes, take| {
                    if take {
                        let next = size_add(taken, bytes)?;
                        stage.grow(bytes)?;
                        taken = next;
                    } else {
                        stage.release(bytes);
                        taken -= bytes;
                    }
                    Ok(())
                },
                &mut work,
            )?;
            let rows_bytes = if leased {
                row_ledger.rows_model(rows.capacity())?
            } else {
                row_ledger.legacy_rows_model()
            };
            let schema_bytes = if leased {
                crate::model::temp_schema_model(&schema)?
            } else {
                0
            };
            let fixed = size_add(
                size_add(64, schema_bytes)?,
                size_add(
                    index_container_model(index_sets.len())?,
                    index_descriptors_model(index_sets, &schema)?,
                )?,
            )?;
            let extra = if leased {
                fixed
            } else {
                size_add(rows_bytes, fixed)?
            };
            let next = size_add(taken, extra)?;
            stage.grow(extra)?;
            taken = next;
            let mut indexes = Vec::with_capacity(index_sets.len());
            for (set, unique) in index_sets {
                let mut ix = TempIndex::new(set, *unique, &schema)?;
                let bound = ix.entries_bound(rows.iter(), rows.len(), coll, &mut work)?;
                let next = size_add(taken, bound)?;
                stage.grow(bound)?;
                taken = next;
                let (mut entries, actual) =
                    ix.entries_of(rows.iter(), rows.len(), 0, coll, &mut work)?;
                if ix.has_duplicate(&mut entries) {
                    return Err(ExecError::Runtime(RuntimeError::unique_index()));
                }
                ix.apply(entries, actual);
                let excess = bound.saturating_sub(actual);
                stage.release(excess);
                taken -= excess;
                indexes.push(ix);
            }
            let bytes = if leased {
                size_add(rows_bytes, taken)?
            } else {
                taken
            };
            let n = rows.len() as u64;
            let table = TempTable {
                schema,
                rows,
                txn,
                added: Vec::new(),
                indexes,
                row_ledger,
                bytes,
                schema_bytes,
                origins,
            };
            self.reserve_insert(bytes, env)?;
            Ok((key, table, n))
        })();
        let (key, table, n) = built?;
        let id = TempTableId(self.next);
        self.next = self.next.wrapping_add(1);
        self.bytes += table.bytes;
        if txn.is_some() {
            self.in_txn += 1;
        }
        self.by_name.insert(key, id);
        self.tables.insert(id, table);
        stage.disarm();
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
        self.append_with_origins(name, rows, txn, index_sets, env, None)
    }

    pub(crate) fn append_with_origins(
        &mut self,
        name: &str,
        rows: Vec<Row>,
        txn: Option<TxnId>,
        index_sets: &[(Vec<usize>, bool)],
        env: &ExecEnv<'_>,
        proof: Option<MaterializedOrigins>,
    ) -> Result<u64, ExecError> {
        let stage = TempWriteStage::new(self, env);
        let Some(id) = self.id_of(name) else {
            return Err(ExecError::Fatal(format!(
                "временная таблица {name} не существует"
            )));
        };
        let leased = self.lease.is_some();
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
        let take = |bytes: u64, taken: &mut u64| -> Result<(), ExecError> {
            let next = size_add(*taken, bytes)?;
            stage.grow(bytes)?;
            *taken = next;
            Ok(())
        };
        let give = |bytes: u64| stage.release(bytes);
        let mut work = crate::model::LedgerWork::new(env);
        work.check()?;
        if rows.is_empty() && index_sets.is_empty() {
            let outer = if leased {
                size_vec(rows.capacity(), std::mem::size_of::<Row>())?
            } else {
                0
            };
            drop(rows);
            stage.refund(outer);
            return Ok(0);
        }
        let built = (|| -> Result<_, ExecError> {
            let origins = t
                .origins
                .prepare_append(proof, first, rows.len(), txn, &stage)?;
            let mut taken = stage.bytes();
            let origin_growth = origins.next_bytes - t.origins.bytes;
            let ledger = if rows.is_empty() {
                None
            } else {
                Some(t.row_ledger.prepare_append(
                    &rows,
                    leased,
                    &mut |bytes, take_it| {
                        if take_it {
                            take(bytes, &mut taken)?;
                        } else {
                            give(bytes);
                            taken -= bytes;
                        }
                        Ok(())
                    },
                    &mut work,
                )?)
            };
            let rows_bytes = if let Some(ledger) = &ledger {
                ledger.input_model(rows.capacity(), leased)?
            } else if leased {
                size_vec(rows.capacity(), std::mem::size_of::<Row>())?
            } else {
                0
            };
            if !leased {
                take(rows_bytes, &mut taken)?;
            }
            let incoming_outer = if leased {
                size_vec(rows.capacity(), std::mem::size_of::<Row>())?
            } else {
                0
            };
            let row_count = first.checked_add(rows.len()).ok_or_else(invalid_size)?;
            let index_count = t
                .indexes
                .len()
                .checked_add(index_sets.len())
                .ok_or_else(invalid_size)?;
            let old_index_container = index_container_model(t.indexes.capacity())?;
            let replace_indexes = index_count > t.indexes.capacity();
            let new_index_container = if replace_indexes {
                index_container_model(index_count)?
            } else {
                0
            };
            let staging = size_add(
                size_vec(t.indexes.len(), std::mem::size_of::<PreparedIndex>())?,
                size_add(
                    index_container_model(index_sets.len())?,
                    new_index_container,
                )?,
            )?;
            let descriptors = index_descriptors_model(index_sets, &t.schema)?;
            take(size_add(staging, descriptors)?, &mut taken)?;
            let mut persistent = size_add(descriptors, origin_growth)?;
            let mut new_entries = Vec::with_capacity(t.indexes.len());
            for ix in &t.indexes {
                let growth = ix.apply_growth(rows.len())?;
                let schedule = if !ix.entries.is_empty() && !rows.is_empty() {
                    crate::model::index_merge_schedule_model(
                        ix.entries
                            .len()
                            .checked_add(rows.len())
                            .ok_or_else(invalid_size)?,
                    )?
                } else {
                    0
                };
                let bound = size_add(
                    schedule,
                    size_add(
                        ix.entries_bound(rows.iter(), rows.len(), coll, &mut work)?,
                        growth,
                    )?,
                )?;
                take(bound, &mut taken)?;
                let (mut entries, bytes) =
                    ix.entries_of(rows.iter(), rows.len(), first, coll, &mut work)?;
                if ix.has_duplicate(&mut entries) {
                    return Err(ExecError::Runtime(RuntimeError::unique_index()));
                }
                let excess = bound
                    .checked_sub(size_add(schedule, size_add(bytes, growth)?)?)
                    .ok_or_else(invalid_size)?;
                give(excess);
                taken -= excess;
                let freed = ix.apply_freed(&entries)?;
                persistent = size_add(
                    persistent,
                    size_add(bytes, growth)?
                        .checked_sub(freed)
                        .ok_or_else(invalid_size)?,
                )?;
                new_entries.push(PreparedIndex::prepare(
                    ix, entries, bytes, growth, &mut work,
                )?);
            }
            let mut fresh = Vec::with_capacity(index_sets.len());
            for (set, unique) in index_sets {
                let mut ix = TempIndex::new(set, *unique, &t.schema)?;
                let bound =
                    ix.entries_bound(t.rows.iter().chain(rows.iter()), row_count, coll, &mut work)?;
                take(bound, &mut taken)?;
                let (mut entries, bytes) = ix.entries_of(
                    t.rows.iter().chain(rows.iter()),
                    row_count,
                    0,
                    coll,
                    &mut work,
                )?;
                if ix.has_duplicate(&mut entries) {
                    return Err(ExecError::Runtime(RuntimeError::unique_index()));
                }
                ix.apply(entries, bytes);
                let excess = bound.checked_sub(bytes).ok_or_else(invalid_size)?;
                give(excess);
                taken -= excess;
                persistent = size_add(persistent, bytes)?;
                fresh.push(ix);
            }
            let replacement_indexes = if replace_indexes {
                persistent = size_add(persistent, new_index_container - old_index_container)?;
                Some(Vec::with_capacity(index_count))
            } else {
                None
            };
            let journal_grows = txn.is_some() && n > 0 && t.added.len() == t.added.capacity();
            let replacement_added = if journal_grows {
                let cap = t
                    .added
                    .capacity()
                    .checked_mul(2)
                    .ok_or_else(invalid_size)?
                    .max(t.added.len().checked_add(1).ok_or_else(invalid_size)?)
                    .max(4);
                std::alloc::Layout::array::<(TxnId, usize, usize)>(cap)
                    .map_err(|_| invalid_size())?;
                let bytes = size_vec(cap, std::mem::size_of::<(TxnId, usize, usize)>())?;
                take(bytes, &mut taken)?;
                persistent = size_add(
                    persistent,
                    bytes
                        - size_vec(
                            t.added.capacity(),
                            std::mem::size_of::<(TxnId, usize, usize)>(),
                        )?,
                )?;
                let mut replacement = Vec::with_capacity(cap);
                for entry in &t.added {
                    work.step()?;
                    replacement.push(*entry);
                }
                Some(replacement)
            } else {
                None
            };
            let replacement_rows = if row_count > t.rows.capacity() {
                let cap = t
                    .rows
                    .capacity()
                    .checked_mul(2)
                    .ok_or_else(invalid_size)?
                    .max(row_count)
                    .max(4);
                std::alloc::Layout::array::<Row>(cap).map_err(|_| invalid_size())?;
                let bytes = size_vec(cap, std::mem::size_of::<Row>())?;
                take(bytes, &mut taken)?;
                persistent = size_add(
                    persistent,
                    bytes - size_vec(t.rows.capacity(), std::mem::size_of::<Row>())?,
                )?;
                Some(Vec::with_capacity(cap))
            } else {
                None
            };
            if let Some(ledger) = &ledger {
                persistent = size_add(persistent, ledger.next_bytes - t.row_ledger.bytes)?;
            }
            let retained_rows = ledger.as_ref().map_or(0, |plan| plan.retained_rows_bytes);
            let shared_refund = (rows_bytes - incoming_outer)
                .checked_sub(retained_rows)
                .ok_or_else(invalid_size)?;
            let add = size_add(retained_rows, persistent)?;
            let table_bytes = size_add(t.bytes, add)?;
            let manager_bytes = size_add(self.bytes, add)?;
            let paid_persistent = size_add(persistent, if leased { 0 } else { rows_bytes })?;
            let refund = size_add(
                taken
                    .checked_sub(paid_persistent)
                    .ok_or_else(invalid_size)?,
                size_add(incoming_outer, shared_refund)?,
            )?;
            work.check()?;
            Ok((
                origins,
                ledger,
                new_entries,
                fresh,
                replacement_indexes,
                replacement_added,
                replacement_rows,
                table_bytes,
                manager_bytes,
                refund,
            ))
        })();
        let (
            origins,
            ledger,
            new_entries,
            mut fresh,
            replacement_indexes,
            replacement_added,
            replacement_rows,
            table_bytes,
            manager_bytes,
            refund,
        ) = built?;
        let mut rows = rows;
        if let Some(mut replacement) = replacement_rows {
            replacement.append(&mut t.rows);
            replacement.append(&mut rows);
            drop(std::mem::replace(&mut t.rows, replacement));
        } else {
            t.rows.append(&mut rows);
        }
        drop(rows);
        for (ix, prepared) in t.indexes.iter_mut().zip(new_entries) {
            prepared.commit(ix);
        }
        if let Some(mut replacement) = replacement_indexes {
            replacement.append(&mut t.indexes);
            replacement.append(&mut fresh);
            drop(std::mem::replace(&mut t.indexes, replacement));
        } else {
            t.indexes.append(&mut fresh);
        }
        drop(fresh);
        if let Some(replacement) = replacement_added {
            drop(std::mem::replace(&mut t.added, replacement));
        }
        if let Some(ledger) = ledger {
            ledger.commit(&mut t.row_ledger);
        }
        origins.commit(&mut t.origins);
        t.bytes = table_bytes;
        self.bytes = manager_bytes;
        if let Some(x) = txn
            && n > 0
        {
            let was = t.tracked();
            t.added.push((x, first, n as usize));
            if !was {
                self.in_txn += 1;
            }
        }
        stage.disarm();
        stage.refund(refund);
        Ok(n)
    }
}

impl Drop for TempTables {
    fn drop(&mut self) {
        self.close();
    }
}

fn destroy_table(table: TempTable) -> u64 {
    let TempTable {
        mut schema,
        rows,
        txn: _,
        added,
        indexes,
        row_ledger,
        bytes,
        schema_bytes,
        origins,
    } = table;
    drop(rows);
    drop(added);
    drop(indexes);
    drop(row_ledger);
    drop(origins);
    if schema_bytes == 0 {
        drop(schema);
        return bytes;
    }
    let node_unique = Arc::get_mut(&mut schema).is_some();
    let released = match Arc::try_unwrap(schema) {
        Ok(schema) => {
            drop(schema);
            schema_bytes
                - if node_unique {
                    0
                } else {
                    crate::model::temp_schema_node_model()
                }
        }
        Err(schema) => {
            drop(schema);
            0
        }
    };
    bytes - schema_bytes + released
}

fn index_container_model(capacity: usize) -> Result<u64, ExecError> {
    std::alloc::Layout::array::<TempIndex>(capacity).map_err(|_| invalid_size())?;
    size_vec(capacity, std::mem::size_of::<TempIndex>())
}

fn index_descriptors_model(
    index_sets: &[(Vec<usize>, bool)],
    schema: &TempSchema,
) -> Result<u64, ExecError> {
    let mut bytes = 0;
    for (set, _) in index_sets {
        bytes = size_add(
            bytes,
            size_add(64, TempIndex::descriptor_model(set, schema)?)?,
        )?;
    }
    Ok(bytes)
}

impl TempSchemas for TempTables {
    fn get(&self, name: &str) -> Option<Arc<TempSchema>> {
        self.table(name).map(|t| t.schema.clone())
    }
}
