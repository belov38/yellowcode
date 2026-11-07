use std::collections::{HashMap, HashSet};
use std::ops::Bound;
use std::sync::Arc;

use ibschema::storage::IndexId;
use query_ir::{ExecError, HierTable, PExpr, QValue};

use super::join::key_charged;
use super::sort::{SKey, desc_flags};
use super::{BoxOp, Cx, MemBudget, Op, fatal};
use crate::key::Key;
use crate::model::{
    LedgerWork, block, hash_buckets_for, hash_table_model, invalid_size, key_model, row_cap_model,
    size_add, size_block, size_vec, totals_payload_model, totals_row_model, vec_block,
    vec_push_growth,
};
use crate::source::Row;
use crate::totals::payload::{PaidValue, PaidVec};

const NODE_WALK: u64 = 256;

fn push_growth(
    len: usize,
    cap: usize,
    elem: usize,
    cx: &Cx<'_>,
    held: &mut u64,
) -> Result<u64, ExecError> {
    let (grow, release) = vec_push_growth(len, cap, elem);
    if grow > 0 {
        cx.mem.grow(grow)?;
        *held += grow;
    }
    Ok(release)
}

fn release(n: u64, cx: &Cx<'_>, held: &mut u64) {
    if n > 0 {
        cx.mem.shrink(n);
        *held = held.saturating_sub(n);
    }
}

pub struct HierarchyOrder<'a> {
    input: BoxOp<'a>,
    id: u16,
    parent: u16,
    keys: Vec<(PExpr, bool)>,
    out: Option<std::vec::IntoIter<Row>>,
    held: u64,
}

impl<'a> HierarchyOrder<'a> {
    pub fn new(
        input: BoxOp<'a>,
        id: u16,
        parent: u16,
        keys: Vec<(PExpr, bool)>,
    ) -> HierarchyOrder<'a> {
        HierarchyOrder {
            input,
            id,
            parent,
            keys,
            out: None,
            held: 0,
        }
    }

    fn run(&mut self, cx: &mut Cx<'a>) -> Result<Vec<Row>, ExecError> {
        let desc: Arc<[bool]> = desc_flags(&self.keys);
        let mut rows: Vec<Option<Row>> = Vec::new();
        let mut skeys: Vec<SKey> = Vec::new();
        let mut ids: Vec<Key> = Vec::new();
        let mut parents: Vec<Option<Key>> = Vec::new();
        while let Some(row) = self.input.next(cx)? {
            cx.tick()?;
            let idv = row
                .get(usize::from(self.id))
                .ok_or_else(|| fatal("hierarchy id column out of range"))?;
            let idk = key_charged(idv, cx)?;
            let pv = row
                .get(usize::from(self.parent))
                .ok_or_else(|| fatal("hierarchy parent column out of range"))?;
            let pk = if matches!(pv, query_ir::QValue::Null) {
                None
            } else {
                Some(key_charged(pv, cx)?)
            };
            let sk = SKey::of(&self.keys, &desc, &row, cx)?;
            let kb = key_model(&idk) + pk.as_ref().map_or(0, key_model) + sk.bytes();
            cx.mem.keep_scratch(kb)?;
            let rb = row_cap_model(&row);
            cx.mem.grow(rb)?;
            self.held += kb + rb;
            let mut old = 0;
            old += push_growth(
                ids.len(),
                ids.capacity(),
                std::mem::size_of::<Key>(),
                cx,
                &mut self.held,
            )?;
            ids.push(idk);
            old += push_growth(
                parents.len(),
                parents.capacity(),
                std::mem::size_of::<Option<Key>>(),
                cx,
                &mut self.held,
            )?;
            parents.push(pk);
            old += push_growth(
                skeys.len(),
                skeys.capacity(),
                std::mem::size_of::<SKey>(),
                cx,
                &mut self.held,
            )?;
            skeys.push(sk);
            old += push_growth(
                rows.len(),
                rows.capacity(),
                std::mem::size_of::<Option<Row>>(),
                cx,
                &mut self.held,
            )?;
            rows.push(Some(row));
            release(old, cx, &mut self.held);
        }
        let n = rows.len();
        let aux = hash_table_model(hash_buckets_for(n), 16)
            + vec_block(n, std::mem::size_of::<Vec<usize>>())
            + block(48 * n as u64)
            + 3 * vec_block(n, std::mem::size_of::<usize>())
            + vec_block(n, std::mem::size_of::<Row>());
        cx.mem.grow(aux)?;
        self.held += aux;
        let mut by_id: HashMap<&Key, usize> = HashMap::with_capacity(n);
        for (i, k) in ids.iter().enumerate() {
            by_id.entry(k).or_insert(i);
        }
        let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
        let mut roots: Vec<usize> = Vec::with_capacity(n);
        for (i, p) in parents.iter().enumerate() {
            match p.as_ref().and_then(|k| by_id.get(k)) {
                Some(&pi) if pi != i => {
                    if let Some(c) = children.get_mut(pi) {
                        c.push(i);
                    }
                }
                _ => roots.push(i),
            }
        }
        let by_key = |a: &usize, b: &usize| skeys.get(*a).cmp(&skeys.get(*b));
        roots.sort_by(by_key);
        for c in &mut children {
            c.sort_by(by_key);
        }
        let mut out = Vec::with_capacity(n);
        let mut stack: Vec<usize> = Vec::with_capacity(n);
        stack.extend(roots.into_iter().rev());
        while let Some(i) = stack.pop() {
            cx.tick()?;
            if let Some(r) = rows.get_mut(i).and_then(Option::take) {
                out.push(r);
                if let Some(c) = children.get(i) {
                    stack.extend(c.iter().rev());
                }
            }
        }
        out.extend(rows.into_iter().flatten());
        drop(by_id);
        drop((children, stack, ids, parents, skeys));
        let rows_bytes: u64 = out.iter().map(row_cap_model).sum::<u64>()
            + vec_block(out.capacity(), std::mem::size_of::<Row>());
        let gone = self.held.saturating_sub(rows_bytes);
        release(gone, cx, &mut self.held);
        Ok(out)
    }
}

impl<'a> Op<'a> for HierarchyOrder<'a> {
    fn close(&mut self, cx: &mut Cx<'a>) {
        self.out = None;
        cx.mem.shrink(std::mem::take(&mut self.held));
        self.input.close(cx);
    }

    fn next(&mut self, cx: &mut Cx<'a>) -> Result<Option<Row>, ExecError> {
        if self.out.is_none() {
            let v = self.run(cx)?;
            self.out = Some(v.into_iter());
        }
        match self.out.as_mut().and_then(Iterator::next) {
            Some(r) => {
                let n = row_cap_model(&r);
                cx.mem.shrink(n);
                self.held = self.held.saturating_sub(n);
                Ok(Some(r))
            }
            None => {
                self.out = None;
                cx.mem.shrink(std::mem::take(&mut self.held));
                Ok(None)
            }
        }
    }
}

pub enum Seeds<'a> {
    Values(Vec<PExpr>),
    Rel(Option<BoxOp<'a>>),
}

pub struct HierarchyClosure<'a> {
    tables: Vec<HierTable>,
    seeds: Seeds<'a>,
    use_index: bool,
    out: Option<std::vec::IntoIter<Row>>,
    held: u64,
}

struct ClosureBuild<'c, 'a> {
    cx: &'c mut Cx<'a>,
    input: Option<BoxOp<'a>>,
    temp: u64,
    output: u64,
    walk: u64,
    out: Vec<Row>,
}

fn hierarchy_snapshot(_mem: &MemBudget<'_>) {
    #[cfg(test)]
    payload_admission_tests::snapshot(_mem);
    #[cfg(test)]
    super::agg::payload_admission_tests::snapshot(_mem);
    #[cfg(test)]
    super::agg::product_witnesses::snapshot(_mem);
}

fn hierarchy_check(env: &crate::source::ExecEnv<'_>) -> Result<(), ExecError> {
    LedgerWork::new(env).check()
}

pub(crate) fn hierarchy_reserve<T>(
    values: &mut Vec<T>,
    additional: usize,
    mem: &MemBudget<'_>,
    env: &crate::source::ExecEnv<'_>,
    balance: &mut u64,
) -> Result<(), ExecError> {
    let required = values
        .len()
        .checked_add(additional)
        .ok_or_else(invalid_size)?;
    if required <= values.capacity() {
        return Ok(());
    }
    let capacity = values
        .capacity()
        .checked_mul(2)
        .ok_or_else(invalid_size)?
        .max(required)
        .max(4);
    let old_bytes = size_vec(values.capacity(), std::mem::size_of::<T>())?;
    let new_bytes = size_vec(capacity, std::mem::size_of::<T>())?;
    let updated = size_add(
        balance.checked_sub(old_bytes).ok_or_else(invalid_size)?,
        new_bytes,
    )?;
    let next = PaidVec::new(capacity, mem, Some(env))?;
    let mut replacement = Replacement {
        values: Some(next),
        mem,
    };
    let old_capacity = values.capacity();
    let mut old = std::mem::take(values).into_iter();
    #[cfg(test)]
    super::agg::product_witnesses::replacement(
        agg_role,
        mem,
        old_pointer,
        replacement
            .values
            .as_ref()
            .ok_or_else(invalid_size)?
            .as_ptr() as usize,
    );
    let mut work = LedgerWork::new(env);
    work.check()?;
    for (i, value) in old.by_ref().enumerate() {
        work.step()?;
        replacement
            .values
            .as_mut()
            .ok_or_else(invalid_size)?
            .push(value)?;
        #[cfg(test)]
        payload_admission_tests::move_checkpoint(
            mem,
            env.cancel,
            old_pointer,
            (old_capacity, capacity),
            std::mem::size_of::<T>(),
            i + 1,
            replacement
                .values
                .as_ref()
                .ok_or_else(invalid_size)?
                .as_ptr() as usize,
        );
        #[cfg(test)]
        super::agg::product_witnesses::moving(
            agg_role,
            mem,
            env.cancel,
            old_pointer,
            old_capacity,
            std::mem::size_of::<T>(),
            i + 1,
        )?;
        #[cfg(not(test))]
        let _ = (i, old_capacity);
    }
    let replacement_values = replacement.values.take().ok_or_else(invalid_size)?;
    #[cfg(test)]
    super::agg::product_witnesses::restore_role(agg_role, mem);
    #[cfg(test)]
    if agg_early {
        mem.shrink(old_bytes);
        super::agg::product_witnesses::snapshot(mem);
    }
    #[cfg(test)]
    super::agg::product_witnesses::event(
        super::agg::product_witnesses::Point::OldDrop,
        mem,
        old_pointer,
        old_bytes as usize,
        0,
    );
    hierarchy_snapshot(mem);
    drop(old);
    #[cfg(test)]
    if !agg_early {
        mem.shrink(old_bytes);
    }
    #[cfg(not(test))]
    mem.shrink(old_bytes);
    hierarchy_snapshot(mem);
    #[cfg(test)]
    super::agg::product_witnesses::event(
        super::agg::product_witnesses::Point::OldRefund,
        mem,
        old_pointer,
        old_bytes as usize,
        0,
    );
    let (next, _) = replacement_values.into_parts();
    *values = next;
    *balance = updated;
    Ok(())
}

struct Replacement<'m, 'a, T> {
    values: Option<PaidVec<'m, 'a, T>>,
    mem: &'m MemBudget<'a>,
}

impl<T> Drop for Replacement<'_, '_, T> {
    fn drop(&mut self) {
        if let Some(values) = self.values.take() {
            #[cfg(test)]
            {
                super::agg::product_witnesses::restore_key(values.as_ptr() as usize, self.mem);
                if super::agg::product_witnesses::early(
                    super::agg::product_witnesses::Point::ReplacementDrop,
                    self.mem,
                ) {
                    let (values, bytes) = values.into_parts();
                    self.mem.shrink(bytes);
                    super::agg::product_witnesses::snapshot(self.mem);
                    drop(values);
                    return;
                }
            }
            #[cfg(test)]
            if payload_admission_tests::refund_early() {
                let (values, bytes) = values.into_parts();
                self.mem.shrink(bytes);
                hierarchy_snapshot(self.mem);
                drop(values);
                return;
            }
            hierarchy_snapshot(self.mem);
            drop(values);
            hierarchy_snapshot(self.mem);
        }
    }
}

pub(crate) struct HierarchyKey<'m, 'a> {
    value: Key,
    mem: &'m MemBudget<'a>,
    scratch: u64,
    retained: u64,
}

#[derive(Clone, Copy)]
enum KeyBuildKind {
    OrdinaryScratch,
    TotalsOwned,
}

#[allow(clippy::too_many_arguments)]
fn build_key(
    value: &QValue,
    mem: &MemBudget<'_>,
    coll: crate::coll::ExecCollation<'_>,
    env: Option<&crate::source::ExecEnv<'_>>,
    key: &mut Key,
    temporary: &mut u64,
    kind: KeyBuildKind,
) -> Result<(), ExecError> {
    if let Some(env) = env {
        hierarchy_check(env)?;
    }
    let mut charge = |bytes| {
        let next = size_add(*temporary, bytes)?;
        #[cfg(test)]
        if matches!(kind, KeyBuildKind::TotalsOwned) {
            crate::totals::distinct_tests::snapshot(
                crate::totals::distinct_tests::Point::ChargeBefore,
                mem,
                bytes,
                *temporary,
            );
            crate::totals::distinct_tests::fault(
                crate::totals::distinct_tests::Point::ChargeBefore,
            )?;
        }
        match kind {
            KeyBuildKind::OrdinaryScratch => mem.grow_scratch(bytes)?,
            KeyBuildKind::TotalsOwned => mem.grow(bytes)?,
        }
        #[cfg(test)]
        if let Some(refusal) = refusal {
            refusal.accepted();
        }
        *temporary = next;
        #[cfg(test)]
        if matches!(kind, KeyBuildKind::TotalsOwned) {
            crate::totals::distinct_tests::snapshot(
                crate::totals::distinct_tests::Point::ChargeAfter,
                mem,
                bytes,
                *temporary,
            );
        }
        if matches!(kind, KeyBuildKind::OrdinaryScratch) {
            hierarchy_snapshot(mem);
            #[cfg(test)]
            payload_admission_tests::key_charge_checkpoint()?;
            #[cfg(test)]
            super::agg::product_witnesses::checkpoint(
                super::agg::product_witnesses::Point::KeyCharge,
                mem,
                env.map(|env| env.cancel),
                0,
                bytes as usize,
                0,
            )?;
        }
        Ok(())
    };
    match kind {
        KeyBuildKind::OrdinaryScratch => {
            crate::model::try_charge_key(value, coll, &mut charge)?;
        }
        KeyBuildKind::TotalsOwned => {
            crate::model::try_charge_totals_key(value, coll, &mut charge)?;
        }
    }
    #[cfg(test)]
    if matches!(kind, KeyBuildKind::OrdinaryScratch) {
        payload_admission_tests::key_build_checkpoint(mem)?;
        #[cfg(test)]
        super::agg::product_witnesses::checkpoint(
            super::agg::product_witnesses::Point::KeyBuild,
            mem,
            env.map(|env| env.cancel),
            0,
            0,
            0,
        )?;
    }
    *key = match kind {
        KeyBuildKind::OrdinaryScratch => crate::key::try_key_of(value, coll)?,
        KeyBuildKind::TotalsOwned => crate::key::try_key_of_totals(value, coll)?,
    };
    #[cfg(test)]
    if matches!(kind, KeyBuildKind::OrdinaryScratch) {
        super::agg::product_witnesses::built(mem, key.0.as_ptr() as usize, key.0.capacity());
    }
    #[cfg(test)]
    if matches!(kind, KeyBuildKind::TotalsOwned) {
        crate::totals::distinct_tests::key_snapshot(
            crate::totals::distinct_tests::Point::Built,
            mem,
            key.0.as_ptr() as usize,
            key.0.capacity(),
            *temporary,
        );
    }
    Ok(())
}

pub(crate) struct TotalsKey<'m, 'a> {
    value: Key,
    mem: &'m MemBudget<'a>,
    temporary: u64,
    retained: u64,
}

impl<'m, 'a> TotalsKey<'m, 'a> {
    pub(crate) fn new(
        value: &QValue,
        mem: &'m MemBudget<'a>,
        coll: crate::coll::ExecCollation<'_>,
        env: Option<&crate::source::ExecEnv<'_>>,
    ) -> Result<Self, ExecError> {
        let mut owner = Self {
            value: Key(Vec::new()),
            mem,
            temporary: 0,
            retained: 0,
        };
        build_key(
            value,
            mem,
            coll,
            env,
            &mut owner.value,
            &mut owner.temporary,
            KeyBuildKind::TotalsOwned,
        )?;
        Ok(owner)
    }

    pub(crate) fn as_key(&self) -> &Key {
        &self.value
    }

    pub(crate) fn retain(&mut self) -> Result<(), ExecError> {
        #[cfg(test)]
        {
            crate::totals::distinct_tests::snapshot(
                crate::totals::distinct_tests::Point::RetainBefore,
                self.mem,
                self.value.0.capacity() as u64,
                self.temporary,
            );
            crate::totals::distinct_tests::fault(
                crate::totals::distinct_tests::Point::RetainBefore,
            )?;
        }
        let bytes = size_block(self.value.0.capacity() as u64)?;
        let residual = self.temporary.checked_sub(bytes).ok_or_else(invalid_size)?;
        self.temporary = 0;
        self.retained = bytes;
        self.mem.shrink(residual);
        #[cfg(test)]
        {
            crate::totals::distinct_tests::snapshot(
                crate::totals::distinct_tests::Point::RetainAfter,
                self.mem,
                self.value.0.capacity() as u64,
                self.retained,
            );
            crate::totals::distinct_tests::fault(
                crate::totals::distinct_tests::Point::RetainAfter,
            )?;
        }
        Ok(())
    }

    pub(crate) fn bytes(&self) -> u64 {
        self.retained
    }

    pub(crate) fn transfer(mut self, destination: &mut u64, next: u64) -> Key {
        let value = Key(std::mem::take(&mut self.value.0));
        self.retained = 0;
        *destination = next;
        value
    }
}

impl Drop for TotalsKey<'_, '_> {
    fn drop(&mut self) {
        if self.temporary == 0 && self.retained == 0 {
            return;
        }
        #[cfg(test)]
        crate::totals::distinct_tests::snapshot(
            crate::totals::distinct_tests::Point::KeyDropBegin,
            self.mem,
            self.retained,
            self.temporary,
        );
        drop(std::mem::take(&mut self.value.0));
        #[cfg(test)]
        crate::totals::distinct_tests::key_snapshot(
            crate::totals::distinct_tests::Point::KeyDestroyed,
            self.mem,
            observed_backing.0,
            observed_backing.1,
            0,
        );
        #[cfg(test)]
        crate::totals::distinct_tests::snapshot(
            crate::totals::distinct_tests::Point::KeyRefundBefore,
            self.mem,
            self.retained,
            0,
        );
        self.mem.shrink(std::mem::take(&mut self.retained));
        #[cfg(test)]
        crate::totals::distinct_tests::snapshot(
            crate::totals::distinct_tests::Point::KeyRefundAfter,
            self.mem,
            self.temporary,
            0,
        );
        #[cfg(test)]
        crate::totals::distinct_tests::snapshot(
            crate::totals::distinct_tests::Point::KeyRefundBefore,
            self.mem,
            self.temporary,
            0,
        );
        self.mem.shrink(std::mem::take(&mut self.temporary));
        #[cfg(test)]
        crate::totals::distinct_tests::snapshot(
            crate::totals::distinct_tests::Point::KeyRefundAfter,
            self.mem,
            0,
            0,
        );
        #[cfg(test)]
        crate::totals::distinct_tests::snapshot(
            crate::totals::distinct_tests::Point::KeyDropEnd,
            self.mem,
            0,
            0,
        );
    }
}

impl<'m, 'a> HierarchyKey<'m, 'a> {
    pub(crate) fn new(value: &QValue, cx: &'m Cx<'a>) -> Result<Self, ExecError> {
        let mut owner = Self {
            value: Key(Vec::new()),
            mem: &cx.mem,
            scratch: 0,
            retained: 0,
        };
        build_key(
            value,
            &cx.mem,
            cx.coll,
            Some(cx.env),
            &mut owner.value,
            &mut owner.scratch,
            KeyBuildKind::OrdinaryScratch,
        )?;
        Ok(owner)
    }

    pub(crate) fn as_key(&self) -> &Key {
        &self.value
    }

    pub(crate) fn retain(&mut self) -> Result<(), ExecError> {
        #[cfg(test)]
        super::agg::product_witnesses::restore_key(self.value.0.as_ptr() as usize, self.mem);
        let bytes = size_block(self.value.0.capacity() as u64)?;
        let residual = self.scratch.checked_sub(bytes).ok_or_else(invalid_size)?;
        self.mem.keep_scratch(bytes)?;
        self.retained = bytes;
        self.scratch = residual;
        self.mem.shrink_scratch(std::mem::take(&mut self.scratch));
        hierarchy_snapshot(self.mem);
        #[cfg(test)]
        payload_admission_tests::key_retained_checkpoint(
            self.mem,
            self.value.0.as_ptr() as usize,
            bytes,
        )?;
        #[cfg(test)]
        super::agg::product_witnesses::checkpoint(
            super::agg::product_witnesses::Point::KeyRetain,
            self.mem,
            None,
            self.value.0.as_ptr() as usize,
            bytes as usize,
            0,
        )?;
        Ok(())
    }

    pub(crate) fn into_parts(mut self) -> (Key, u64) {
        let value = Key(std::mem::take(&mut self.value.0));
        let bytes = std::mem::take(&mut self.retained);
        (value, bytes)
    }
}

impl Drop for HierarchyKey<'_, '_> {
    fn drop(&mut self) {
        #[cfg(test)]
        {
            super::agg::product_witnesses::restore_key(self.value.0.as_ptr() as usize, self.mem);
            if super::agg::product_witnesses::early(
                super::agg::product_witnesses::Point::KeyDrop,
                self.mem,
            ) {
                self.mem.shrink(std::mem::take(&mut self.retained));
                super::agg::product_witnesses::snapshot(self.mem);
            }
        }
        #[cfg(test)]
        if payload_admission_tests::refund_early() {
            self.mem.shrink(std::mem::take(&mut self.retained));
        }
        hierarchy_snapshot(self.mem);
        drop(std::mem::take(&mut self.value.0));
        self.mem.shrink(std::mem::take(&mut self.retained));
        self.mem.shrink_scratch(std::mem::take(&mut self.scratch));
        hierarchy_snapshot(self.mem);
        #[cfg(test)]
        super::agg::product_witnesses::restore_role(
            super::agg::product_witnesses::Role::Idle,
            self.mem,
        );
    }
}

pub(crate) struct HierarchyHash<T> {
    len: usize,
    buckets: usize,
    marker: std::marker::PhantomData<T>,
}

impl<T> HierarchyHash<T> {
    pub(crate) fn new() -> Self {
        Self {
            len: 0,
            buckets: 0,
            marker: std::marker::PhantomData,
        }
    }

    pub(crate) fn bytes(buckets: usize) -> Result<u64, ExecError> {
        if buckets == 0 {
            return Ok(0);
        }
        let alignment = std::mem::align_of::<T>().max(16);
        let entries = buckets
            .checked_mul(std::mem::size_of::<T>())
            .ok_or_else(invalid_size)?;
        let offset = entries
            .checked_add(alignment - 1)
            .ok_or_else(invalid_size)?
            & !(alignment - 1);
        let bytes = offset
            .checked_add(buckets)
            .and_then(|n| n.checked_add(16))
            .ok_or_else(invalid_size)?;
        std::alloc::Layout::from_size_align(bytes, alignment).map_err(|_| invalid_size())?;
        if bytes > (isize::MAX as usize).saturating_sub(alignment - 1) {
            return Err(invalid_size());
        }
        size_block(u64::try_from(bytes).map_err(|_| invalid_size())?)
    }

    pub(crate) fn prepare(
        &self,
        mem: &MemBudget<'_>,
        balance: &mut u64,
    ) -> Result<(usize, u64), ExecError> {
        self.len.checked_add(1).ok_or_else(invalid_size)?;
        let capacity = if self.buckets < 8 {
            self.buckets.saturating_sub(1)
        } else {
            self.buckets / 8 * 7
        };
        if self.len < capacity {
            #[cfg(test)]
            super::agg::product_witnesses::hash_prepared(mem, 0);
            return Ok((self.buckets, 0));
        }
        let buckets = self.buckets.checked_mul(2).ok_or_else(invalid_size)?.max(4);
        let bytes = Self::bytes(buckets)?;
        let next = size_add(*balance, bytes)?;
        mem.grow(bytes)?;
        #[cfg(test)]
        refusal.accepted();
        *balance = next;
        #[cfg(test)]
        super::agg::product_witnesses::hash_prepared(mem, bytes);
        hierarchy_snapshot(mem);
        Ok((buckets, Self::bytes(self.buckets)?))
    }

    pub(crate) fn inserted(
        &mut self,
        prepared: (usize, u64),
        capacity: usize,
        mem: &MemBudget<'_>,
        balance: &mut u64,
    ) -> Result<(), ExecError> {
        self.len = self.len.checked_add(1).ok_or_else(invalid_size)?;
        self.buckets = prepared.0;
        let expected = if self.buckets < 8 {
            self.buckets.saturating_sub(1)
        } else {
            self.buckets / 8 * 7
        };
        if capacity != expected {
            return Err(invalid_size());
        }
        *balance = balance.checked_sub(prepared.1).ok_or_else(invalid_size)?;
        mem.shrink(prepared.1);
        hierarchy_snapshot(mem);
        Ok(())
    }
}

pub(crate) fn hierarchy_copy_model(value: &QValue, cx: &Cx<'_>) -> Result<u64, ExecError> {
    let retained = totals_payload_model(value, &cx.mem, Some(cx.env))?;
    match value {
        QValue::Number(number) => match number.try_clone_with(Err::<(), _>) {
            Ok(_) => Ok(0),
            Err(bsl_prim::number::NumberAllocError::Reserve(len)) => {
                size_vec(len, std::mem::size_of::<u32>())
            }
            Err(_) => Err(invalid_size()),
        },
        QValue::Type(ty) => size_add(
            size_vec(ty.refs.len(), std::mem::size_of::<query_ir::RefAtom>())?,
            size_vec(ty.others.len(), std::mem::size_of::<query_ir::Other>())?,
        ),
        QValue::Boundary(boundary) => size_add(
            size_block(std::mem::size_of::<query_ir::value::Boundary>() as u64)?,
            hierarchy_copy_model(&boundary.point, cx)?,
        ),
        _ => Ok(retained),
    }
}

impl<'c, 'a> ClosureBuild<'c, 'a> {
    fn new(cx: &'c mut Cx<'a>) -> Self {
        Self {
            cx,
            input: None,
            temp: 0,
            output: 0,
            walk: 0,
            out: Vec::new(),
        }
    }

    fn close_input(&mut self) {
        if let Some(mut input) = self.input.take() {
            input.close(self.cx);
            drop(input);
        }
    }

    fn collect_seeds(&mut self, seeds: &mut Seeds<'a>) -> Result<Vec<QValue>, ExecError> {
        let mut out = Vec::new();
        match seeds {
            Seeds::Values(expressions) => {
                for expression in expressions {
                    hierarchy_check(self.cx.env)?;
                    match expression {
                        PExpr::Set(set) => {
                            for value in &set.values {
                                self.seed(&mut out, value)?;
                            }
                        }
                        PExpr::Lit(value) => self.seed(&mut out, value)?,
                        expression => {
                            let value = super::eval(expression, &[], self.cx)?;
                            self.seed(&mut out, &value)?;
                        }
                    }
                }
            }
            Seeds::Rel(input) => {
                self.input = input.take();
                while let Some(row) = match self.input.as_mut() {
                    Some(input) => input.next(self.cx)?,
                    None => None,
                } {
                    self.cx.tick()?;
                    let value = row
                        .first()
                        .ok_or_else(|| fatal("a hierarchy seed without a column"))?;
                    #[cfg(test)]
                    payload_admission_tests::seed_checked_checkpoint(value);
                    self.seed(&mut out, value)?;
                }
                self.close_input();
            }
        }
        Ok(out)
    }

    fn seed(&mut self, out: &mut Vec<QValue>, value: &QValue) -> Result<(), ExecError> {
        let mem = &self.cx.mem;
        hierarchy_reserve(out, 1, mem, self.cx.env, &mut self.temp)?;
        let copy = PaidValue::copy(value, mem, Some(self.cx.env))?;
        let next = size_add(self.temp, copy.bytes())?;
        let (value, _) = copy.into_parts();
        out.push(value);
        self.temp = next;
        #[cfg(test)]
        payload_admission_tests::seed_transferred_checkpoint(out.last());
        Ok(())
    }

    fn add(
        &mut self,
        value: &QValue,
        seen: &mut HashSet<Key>,
        model: &mut HierarchyHash<(Key, ())>,
        pending: &mut bool,
    ) -> Result<Option<usize>, ExecError> {
        let mut key = HierarchyKey::new(value, self.cx)?;
        if seen.contains(&key.value) {
            return Ok(None);
        }
        key.retain()?;
        let row_bytes = size_add(
            size_vec(1, crate::model::VALUE_SIZE)?,
            hierarchy_copy_model(value, self.cx)?,
        )?;
        self.cx.mem.check_value(row_bytes)?;
        let prepared = model.prepare(&self.cx.mem, &mut self.temp)?;
        hierarchy_reserve(
            &mut self.out,
            1,
            &self.cx.mem,
            self.cx.env,
            &mut self.output,
        )?;
        if !*pending {
            let next = size_add(self.walk, NODE_WALK)?;
            self.cx.mem.grow(NODE_WALK)?;
            self.walk = next;
        }
        let mut row = PaidVec::new(1, &self.cx.mem, Some(self.cx.env))?;
        let copy = PaidValue::copy(value, &self.cx.mem, Some(self.cx.env))?;
        let actual_row = size_add(
            size_vec(row.capacity(), crate::model::VALUE_SIZE)?,
            copy.bytes(),
        )?;
        if actual_row != row_bytes {
            return Err(invalid_size());
        }
        let next_output = size_add(self.output, actual_row)?;
        let next_temp = size_add(self.temp, key.retained)?;
        let (value, _) = copy.into_parts();
        Vec::push(&mut row, value);
        let (row, _) = row.into_parts();
        let (key, _) = key.into_parts();
        self.temp = next_temp;
        self.output = next_output;
        let index = self.out.len();
        self.out.push(row);
        seen.insert(key);
        *pending = false;
        hierarchy_snapshot(&self.cx.mem);
        model.inserted(prepared, seen.capacity(), &self.cx.mem, &mut self.temp)?;
        Ok(Some(index))
    }

    fn children_map(&mut self, table: &HierTable) -> Result<HashMap<Key, Vec<QValue>>, ExecError> {
        let mut map: HashMap<Key, Vec<QValue>> = HashMap::new();
        let mut model = HierarchyHash::<(Key, Vec<QValue>)>::new();
        let src = self.cx.src;
        for row in src.scan_fields(table.table, &[table.id, table.parent])? {
            self.cx.tick()?;
            let row = row?;
            let id = row
                .get(usize::from(table.id.0))
                .ok_or_else(|| fatal("hierarchy column out of range"))?;
            let parent = row
                .get(usize::from(table.parent.0))
                .ok_or_else(|| fatal("hierarchy column out of range"))?;
            let mut key = HierarchyKey::new(parent, self.cx)?;
            if let Some(list) = map.get_mut(&key.value) {
                hierarchy_reserve(list, 1, &self.cx.mem, self.cx.env, &mut self.temp)?;
                let copy = PaidValue::copy(id, &self.cx.mem, Some(self.cx.env))?;
                let next = size_add(self.temp, copy.bytes())?;
                let (value, _) = copy.into_parts();
                list.push(value);
                self.temp = next;
            } else {
                key.retain()?;
                let prepared = model.prepare(&self.cx.mem, &mut self.temp)?;
                let mut list = Vec::new();
                hierarchy_reserve(&mut list, 1, &self.cx.mem, self.cx.env, &mut self.temp)?;
                let copy = PaidValue::copy(id, &self.cx.mem, Some(self.cx.env))?;
                let next = size_add(self.temp, size_add(copy.bytes(), key.retained)?)?;
                let (value, _) = copy.into_parts();
                list.push(value);
                let (key, _) = key.into_parts();
                self.temp = next;
                hierarchy_snapshot(&self.cx.mem);
                map.insert(key, list);
                model.inserted(prepared, map.capacity(), &self.cx.mem, &mut self.temp)?;
            }
        }
        Ok(map)
    }

    fn walk(
        &mut self,
        tables: &[HierTable],
        use_index: bool,
        seeds: Vec<QValue>,
    ) -> Result<(), ExecError> {
        let mut children: Vec<Option<Children>> = Vec::new();
        hierarchy_reserve(
            &mut children,
            tables.len(),
            &self.cx.mem,
            self.cx.env,
            &mut self.temp,
        )?;
        children.resize_with(tables.len(), || None);
        let mut seen = HashSet::new();
        let mut seen_model = HierarchyHash::<(Key, ())>::new();
        for seed in seeds {
            self.cx.tick()?;
            let slot = match &seed {
                QValue::Null => continue,
                QValue::Ref { table, id } => tables
                    .iter()
                    .position(|t| t.number == *table)
                    .map(|i| (i, id)),
                _ => None,
            };
            let mut pending = false;
            let Some((ti, id)) = slot else {
                self.add(&seed, &mut seen, &mut seen_model, &mut pending)?;
                continue;
            };
            let table = tables.get(ti).ok_or_else(invalid_size)?;
            let next_walk = size_add(self.walk, NODE_WALK)?;
            self.cx.mem.grow(NODE_WALK)?;
            self.walk = next_walk;
            pending = true;
            let src = self.cx.src;
            if id.iter().all(|byte| *byte == 0) {
                for row in src.scan_fields(table.table, &[table.id])? {
                    self.cx.tick()?;
                    let row = row?;
                    let value = row
                        .get(usize::from(table.id.0))
                        .ok_or_else(|| fatal("hierarchy id column out of range"))?;
                    self.add(value, &mut seen, &mut seen_model, &mut pending)?;
                }
            } else if src
                .get_by_id_fields(table.table, &seed, &[table.id])?
                .is_some()
                && let Some(index) = self.add(&seed, &mut seen, &mut seen_model, &mut pending)?
            {
                if children.get(ti).is_some_and(Option::is_none) {
                    let child = match hierarchy_index(table, use_index, self.cx) {
                        Some(index) => Children::Index(index),
                        None => Children::Map(self.children_map(table)?),
                    };
                    *children.get_mut(ti).ok_or_else(invalid_size)? = Some(child);
                }
                let mut queue = Vec::new();
                hierarchy_reserve(&mut queue, 1, &self.cx.mem, self.cx.env, &mut self.temp)?;
                queue.push(index);
                let mut head = 0usize;
                while let Some(&index) = queue.get(head) {
                    head = head.checked_add(1).ok_or_else(invalid_size)?;
                    self.cx.tick()?;
                    let value = self
                        .out
                        .get(index)
                        .and_then(|row| row.first())
                        .ok_or_else(invalid_size)?;
                    match children.get(ti).and_then(Option::as_ref) {
                        Some(Children::Index(index)) => {
                            let key = std::slice::from_ref(value);
                            let rows = src.range_fields(
                                table.table,
                                *index,
                                Bound::Included(key),
                                Bound::Included(key),
                                false,
                                &[table.id],
                            )?;
                            for row in rows {
                                self.cx.tick()?;
                                let row = row?;
                                if let Some(value) = row.get(usize::from(table.id.0))
                                    && let Some(index) =
                                        self.add(value, &mut seen, &mut seen_model, &mut pending)?
                                {
                                    hierarchy_reserve(
                                        &mut queue,
                                        1,
                                        &self.cx.mem,
                                        self.cx.env,
                                        &mut self.temp,
                                    )?;
                                    queue.push(index);
                                }
                            }
                        }
                        Some(Children::Map(map)) => {
                            let key = HierarchyKey::new(value, self.cx)?;
                            let list = map.get(&key.value);
                            drop(key);
                            if let Some(list) = list {
                                for value in list {
                                    hierarchy_check(self.cx.env)?;
                                    if let Some(index) =
                                        self.add(value, &mut seen, &mut seen_model, &mut pending)?
                                    {
                                        hierarchy_reserve(
                                            &mut queue,
                                            1,
                                            &self.cx.mem,
                                            self.cx.env,
                                            &mut self.temp,
                                        )?;
                                        queue.push(index);
                                    }
                                }
                            }
                        }
                        None => {}
                    }
                }
            }
            if pending {
                self.walk = self.walk.checked_sub(NODE_WALK).ok_or_else(invalid_size)?;
                self.cx.mem.shrink(NODE_WALK);
            }
        }
        Ok(())
    }

    fn finish(mut self) -> Result<(Vec<Row>, u64), ExecError> {
        self.close_input();
        let model = self.out.iter().try_fold(
            size_vec(self.out.capacity(), std::mem::size_of::<Row>())?,
            |sum, row| size_add(sum, totals_row_model(row, &self.cx.mem, Some(self.cx.env))?),
        )?;
        if model != self.output {
            return Err(invalid_size());
        }
        self.cx.mem.shrink(std::mem::take(&mut self.temp));
        self.cx.mem.shrink(std::mem::take(&mut self.walk));
        hierarchy_snapshot(&self.cx.mem);
        let rows = std::mem::take(&mut self.out);
        let bytes = std::mem::take(&mut self.output);
        Ok((rows, bytes))
    }
}

impl Drop for ClosureBuild<'_, '_> {
    fn drop(&mut self) {
        self.close_input();
        hierarchy_snapshot(&self.cx.mem);
        #[cfg(test)]
        {
            payload_admission_tests::build_drop_checkpoint(&self.cx.mem, &self.out);
            if payload_admission_tests::refund_early() {
                self.cx.mem.shrink(std::mem::take(&mut self.output));
                hierarchy_snapshot(&self.cx.mem);
            }
        }
        drop(std::mem::take(&mut self.out));
        self.cx.mem.shrink(std::mem::take(&mut self.output));
        self.cx.mem.shrink(std::mem::take(&mut self.temp));
        self.cx.mem.shrink(std::mem::take(&mut self.walk));
        hierarchy_snapshot(&self.cx.mem);
    }
}

enum Children {
    Index(IndexId),
    Map(HashMap<Key, Vec<QValue>>),
}

fn hierarchy_index(table: &HierTable, use_index: bool, cx: &Cx<'_>) -> Option<IndexId> {
    if !use_index {
        return None;
    }
    let index = cx
        .src
        .map()
        .try_table(table.table)?
        .indexes
        .iter()
        .position(|index| index.prefix == 0 && index.fields.first() == Some(&table.parent))?;
    Some(IndexId::Named(u16::try_from(index).ok()?))
}

impl<'a> HierarchyClosure<'a> {
    pub fn new(tables: Vec<HierTable>, use_index: bool, seeds: Seeds<'a>) -> Self {
        Self {
            tables,
            seeds,
            use_index,
            out: None,
            held: 0,
        }
    }

    fn seed_values(&mut self, cx: &mut Cx<'a>, held: &mut u64) -> Result<Vec<QValue>, ExecError> {
        let mut build = ClosureBuild::new(cx);
        let seeds = build.collect_seeds(&mut self.seeds)?;
        let next = size_add(*held, build.temp)?;
        *held = next;
        build.temp = 0;
        Ok(seeds)
    }

    fn run(&mut self, cx: &mut Cx<'a>) -> Result<Vec<Row>, ExecError> {
        let mut build = ClosureBuild::new(cx);
        let seeds = self.seed_values(build.cx, &mut build.temp)?;
        build.walk(&self.tables, self.use_index, seeds)?;
        let (rows, bytes) = build.finish()?;
        self.held = bytes;
        Ok(rows)
    }
}

impl<'a> Op<'a> for HierarchyClosure<'a> {
    fn close(&mut self, cx: &mut Cx<'a>) {
        self.out = None;
        cx.mem.shrink(std::mem::take(&mut self.held));
        if let Seeds::Rel(Some(q)) = &mut self.seeds {
            q.close(cx);
        }
    }

    fn next(&mut self, cx: &mut Cx<'a>) -> Result<Option<Row>, ExecError> {
        if self.out.is_none() {
            let v = self.run(cx)?;
            self.out = Some(v.into_iter());
        }
        match self.out.as_mut().and_then(Iterator::next) {
            Some(r) => Ok(Some(r)),
            None => {
                self.out = None;
                cx.mem.shrink(std::mem::take(&mut self.held));
                Ok(None)
            }
        }
    }
}
