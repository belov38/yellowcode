use std::collections::{HashMap, HashSet};
use std::mem::{size_of, take};

use query_ir::{AggL, ExecError, PExpr, QValue};
use query_syntax::Func;

use super::hier::{HierarchyHash, HierarchyKey, hierarchy_copy_model, hierarchy_reserve};
use super::{BoxOp, Cx, MemBudget, Op, eval, fatal};
use crate::key::Key;
use crate::model::{
    LedgerWork, invalid_size, size_add, size_block, size_vec, totals_payload_model, value_model,
};
use crate::num::AggState;
use crate::source::Row;
use crate::totals::payload::{PaidValue, PaidVec};

struct Representative {
    value: QValue,
    bytes: u64,
}

struct Distinct {
    keys: HashSet<Key>,
    model: HierarchyHash<(Key, ())>,
}

struct ExecState {
    native: AggState,
    distinct: Option<Distinct>,
}

#[derive(Default)]
struct Group {
    rep: Vec<Representative>,
    states: Vec<ExecState>,
}

struct SetState {
    idx: Vec<usize>,
    last_position: Vec<usize>,
    map: HashMap<Vec<Key>, usize>,
    map_model: HierarchyHash<(Vec<Key>, usize)>,
    groups: Vec<Group>,
}

struct AggregateOwner<'m, 'a, T: Default> {
    value: T,
    mem: &'m MemBudget<'a>,
    bytes: u64,
}

impl<T: Default> AggregateOwner<'_, '_, T> {
    fn into_parts(mut self) -> (T, u64) {
        (take(&mut self.value), take(&mut self.bytes))
    }
}

impl<'m, 'a, T> AggregateOwner<'m, 'a, Vec<T>> {
    fn vector(capacity: usize, cx: &'m Cx<'a>) -> Result<Self, ExecError> {
        let paid = PaidVec::new(capacity, &cx.mem, Some(cx.env))?;
        let (value, bytes) = paid.into_parts();
        Ok(Self {
            value,
            mem: &cx.mem,
            bytes,
        })
    }
}

impl<T: Default> Drop for AggregateOwner<'_, '_, T> {
    fn drop(&mut self) {
        #[cfg(test)]
        payload_admission_tests::snapshot(self.mem);
        drop(take(&mut self.value));
        self.mem.shrink(take(&mut self.bytes));
        #[cfg(test)]
        payload_admission_tests::snapshot(self.mem);
    }
}

struct AggregateScratch<'m, 'a> {
    mem: &'m MemBudget<'a>,
    baseline: u64,
}

impl Drop for AggregateScratch<'_, '_> {
    fn drop(&mut self) {
        self.mem
            .shrink_scratch(self.mem.scratch().saturating_sub(self.baseline));
        #[cfg(test)]
        product_witnesses::restore_role(product_witnesses::Role::Idle, self.mem);
    }
}

enum AggregateValue<'v> {
    Borrowed(&'v QValue),
    Produced(QValue),
}

impl AggregateValue<'_> {
    fn get(&self) -> &QValue {
        match self {
            Self::Borrowed(value) => value,
            Self::Produced(value) => value,
        }
    }
}

fn aggregate_value<'v>(
    expression: &'v PExpr,
    row: &'v [QValue],
    cx: &'v Cx<'_>,
) -> Result<AggregateValue<'v>, ExecError> {
    let value = match expression {
        PExpr::Col(index) => row
            .get(usize::from(*index))
            .ok_or_else(|| fatal("column out of range"))?,
        PExpr::Outer { depth, col } => cx
            .outer
            .len()
            .checked_sub(usize::from(*depth))
            .and_then(|index| cx.outer.get(index))
            .and_then(|row| row.get(usize::from(*col)))
            .ok_or_else(|| fatal("outer column out of range"))?,
        PExpr::Lit(value) => value,
        _ => return eval(expression, row, cx).map(AggregateValue::Produced),
    };
    Ok(AggregateValue::Borrowed(value))
}

fn aggregate_tuple<'m, 'a>(
    indexes: &[usize],
    keys: &[HierarchyKey<'_, '_>],
    cx: &'m Cx<'a>,
) -> Result<AggregateOwner<'m, 'a, Vec<Key>>, ExecError> {
    #[cfg(test)]
    payload_admission_tests::phase(payload_admission_tests::Phase::Tuple, &cx.mem);
    #[cfg(test)]
    product_witnesses::phase(product_witnesses::Role::Tuple, &cx.mem);
    let mut tuple = AggregateOwner::vector(indexes.len(), cx)?;
    let mut work = LedgerWork::new(cx.env);
    for &index in indexes {
        work.step()?;
        let Some(key) = keys.get(index) else { continue };
        let key = key.as_key();
        #[cfg(test)]
        product_witnesses::phase(product_witnesses::Role::KeyCopy, &cx.mem);
        let mut copy = PaidVec::new(key.0.len(), &cx.mem, Some(cx.env))?;
        let bytes = size_vec(copy.capacity(), size_of::<u8>())?;
        let next = size_add(tuple.bytes, bytes)?;
        copy.extend_from_slice(&key.0);
        let (copy, _) = copy.into_parts();
        tuple.value.push(Key(copy));
        tuple.bytes = next;
    }
    Ok(tuple)
}

fn aggregate_representatives<'m, 'a>(
    indexes: &[usize],
    values: &[AggregateValue<'_>],
    width: usize,
    cx: &'m Cx<'a>,
) -> Result<AggregateOwner<'m, 'a, Vec<Representative>>, ExecError> {
    let mut projected = size_vec(width, size_of::<QValue>())?;
    let mut work = LedgerWork::new(cx.env);
    for &index in indexes {
        work.step()?;
        if let Some(value) = values.get(index) {
            projected = size_add(projected, hierarchy_copy_model(value.get(), cx)?)?;
        }
    }
    #[cfg(test)]
    product_witnesses::phase(product_witnesses::Role::Row, &cx.mem);
    cx.mem.check_value(projected)?;
    #[cfg(test)]
    refusal.accepted();
    #[cfg(test)]
    payload_admission_tests::phase(payload_admission_tests::Phase::RepBacking, &cx.mem);
    #[cfg(test)]
    product_witnesses::phase(product_witnesses::Role::RepBacking, &cx.mem);
    let mut rep = AggregateOwner::vector(indexes.len(), cx)?;
    for &index in indexes {
        work.step()?;
        let Some(value) = values.get(index) else {
            continue;
        };
        #[cfg(test)]
        payload_admission_tests::phase(payload_admission_tests::Phase::RepPayload, &cx.mem);
        #[cfg(test)]
        product_witnesses::phase(product_witnesses::Role::RepPayload, &cx.mem);
        let copy = PaidValue::copy(value.get(), &cx.mem, Some(cx.env))?;
        let bytes = copy.bytes();
        let next = size_add(rep.bytes, bytes)?;
        let (value, _) = copy.into_parts();
        rep.value.push(Representative { value, bytes });
        rep.bytes = next;
    }
    Ok(rep)
}

fn aggregate_group<'m, 'a>(
    rep: AggregateOwner<'m, 'a, Vec<Representative>>,
    aggregates: &[AggL],
    cx: &'m Cx<'a>,
) -> Result<AggregateOwner<'m, 'a, Group>, ExecError> {
    #[cfg(test)]
    payload_admission_tests::state_request::<ExecState>(aggregates.len());
    #[cfg(test)]
    product_witnesses::phase(product_witnesses::Role::State, &cx.mem);
    let mut states = PaidVec::new(aggregates.len(), &cx.mem, Some(cx.env))?;
    #[cfg(test)]
    payload_admission_tests::constructed(states.as_ptr() as usize);
    let mut work = LedgerWork::new(cx.env);
    for aggregate in aggregates {
        work.step()?;
        let function = if aggregate.f == Func::GroupedBy {
            Func::Count
        } else {
            aggregate.f
        };
        states.push(ExecState {
            native: AggState::new(function, aggregate.arg_dec, false, false)?,
            distinct: aggregate.distinct.then(|| Distinct {
                keys: HashSet::new(),
                model: HierarchyHash::new(),
            }),
        })?;
    }
    let bytes = size_add(
        rep.bytes,
        size_vec(states.capacity(), size_of::<ExecState>())?,
    )?;
    let (rep, _) = rep.into_parts();
    let (states, _) = states.into_parts();
    Ok(AggregateOwner {
        value: Group { rep, states },
        mem: &cx.mem,
        bytes,
    })
}

fn aggregate_is_last(
    last_position: &[usize],
    index: usize,
    position: usize,
) -> Result<bool, ExecError> {
    #[cfg(test)]
    if let Some(last) = product_witnesses::lookup(last_position, index, position)? {
        return Ok(last);
    }
    let last = last_position
        .get(index)
        .ok_or_else(|| fatal("aggregate key index out of range"))?;
    Ok(*last == position)
}

impl ExecState {
    fn add(
        &mut self,
        value: Option<&QValue>,
        cx: &Cx<'_>,
        temp: &mut u64,
    ) -> Result<(), ExecError> {
        if let Some(value) = value
            && !matches!(value, QValue::Null)
            && let Some(distinct) = &mut self.distinct
        {
            #[cfg(test)]
            payload_admission_tests::phase(payload_admission_tests::Phase::Distinct, &cx.mem);
            #[cfg(test)]
            product_witnesses::phase(product_witnesses::Role::DistinctKey, &cx.mem);
            let mut key = HierarchyKey::new(value, cx)?;
            if distinct.keys.contains(key.as_key()) {
                return Ok(());
            }
            key.retain()?;
            let bytes = size_block(key.as_key().0.capacity() as u64)?;
            #[cfg(test)]
            product_witnesses::phase(product_witnesses::Role::DistinctTable, &cx.mem);
            let prepared = distinct.model.prepare(&cx.mem, temp)?;
            let next = size_add(*temp, bytes)?;
            let (key, _) = key.into_parts();
            *temp = next;
            #[cfg(test)]
            product_witnesses::hash_insert(&cx.mem);
            distinct.keys.insert(key);
            distinct
                .model
                .inserted(prepared, distinct.keys.capacity(), &cx.mem, temp)?;
            #[cfg(test)]
            product_witnesses::event(
                product_witnesses::Point::DistinctCommit,
                &cx.mem,
                0,
                prepared.1 as usize,
                0,
            );
        }
        #[cfg(test)]
        payload_admission_tests::phase(payload_admission_tests::Phase::NativeUpdate, &cx.mem);
        #[cfg(test)]
        product_witnesses::phase(product_witnesses::Role::Native, &cx.mem);
        #[cfg(test)]
        product_witnesses::event(product_witnesses::Point::BeforeNative, &cx.mem, 0, 0, 0);
        self.native.add_with(value, cx.env, cx.coll)
    }
}

struct AggregateBuild<'c, 'a> {
    cx: &'c mut Cx<'a>,
    sets: Vec<SetState>,
    out: Vec<Row>,
    temp: u64,
    output: u64,
    legacy: u64,
}

impl<'c, 'a> AggregateBuild<'c, 'a> {
    fn new(cx: &'c mut Cx<'a>) -> Self {
        Self {
            cx,
            sets: Vec::new(),
            out: Vec::new(),
            temp: 0,
            output: 0,
            legacy: 0,
        }
    }

    fn setup(&mut self, keys: usize, sets: Option<&[Vec<u16>]>) -> Result<(), ExecError> {
        #[cfg(test)]
        payload_admission_tests::phase(payload_admission_tests::Phase::Setup, &self.cx.mem);
        #[cfg(test)]
        product_witnesses::phase(product_witnesses::Role::Setup, &self.cx.mem);
        let count = sets.map_or(1, |sets| sets.len());
        let mut states = PaidVec::new(count, &self.cx.mem, Some(self.cx.env))?;
        let mut work = LedgerWork::new(self.cx.env);
        for si in 0..count {
            work.step()?;
            let set = sets
                .map(|sets| {
                    sets.get(si)
                        .ok_or_else(|| fatal("aggregate set index out of range"))
                })
                .transpose()?;
            let count = set.map_or(keys, Vec::len);
            #[cfg(test)]
            product_witnesses::phase(product_witnesses::Role::Idx, &self.cx.mem);
            let mut idx = PaidVec::new(count, &self.cx.mem, Some(self.cx.env))?;
            for index in 0..count {
                work.step()?;
                let index = match set {
                    Some(set) => usize::from(
                        *set.get(index)
                            .ok_or_else(|| fatal("aggregate key index out of range"))?,
                    ),
                    None => index,
                };
                idx.push(index)?;
            }
            #[cfg(test)]
            product_witnesses::phase(product_witnesses::Role::Last, &self.cx.mem);
            let mut last_position = PaidVec::new(keys, &self.cx.mem, Some(self.cx.env))?;
            for _ in 0..keys {
                work.step()?;
                #[cfg(test)]
                product_witnesses::work_init();
                last_position.push(usize::MAX)?;
            }
            let rep_len = idx.iter().filter(|&&index| index < keys).count();
            for (position, &index) in idx.iter().take(rep_len).enumerate() {
                work.step()?;
                #[cfg(test)]
                product_witnesses::work_mark();
                if let Some(slot) = last_position.get_mut(index) {
                    *slot = position;
                }
            }
            let metadata = size_add(
                size_vec(idx.capacity(), size_of::<usize>())?,
                size_vec(last_position.capacity(), size_of::<usize>())?,
            )?;
            let next = size_add(self.temp, metadata)?;
            let (idx, _) = idx.into_parts();
            let (last_position, _) = last_position.into_parts();
            self.temp = next;
            Vec::push(
                &mut states,
                SetState {
                    idx,
                    last_position,
                    map: HashMap::new(),
                    map_model: HierarchyHash::new(),
                    groups: Vec::new(),
                },
            );
        }
        let next = size_add(
            self.temp,
            size_vec(states.capacity(), size_of::<SetState>())?,
        )?;
        let (sets, _) = states.into_parts();
        self.sets = sets;
        self.temp = next;
        #[cfg(test)]
        payload_admission_tests::setup_complete(&self.cx.mem);
        Ok(())
    }

    fn add_row(&mut self, row: &Row, keys: &[PExpr], aggregates: &[AggL]) -> Result<(), ExecError> {
        let cx = &*self.cx;
        let _scratch = AggregateScratch {
            mem: &cx.mem,
            baseline: cx.mem.scratch(),
        };
        #[cfg(test)]
        product_witnesses::phase(product_witnesses::Role::Values, &cx.mem);
        let mut values = PaidVec::new(keys.len(), &cx.mem, Some(cx.env))?;
        #[cfg(test)]
        product_witnesses::phase(product_witnesses::Role::Comparisons, &cx.mem);
        let mut comparisons = PaidVec::new(keys.len(), &cx.mem, Some(cx.env))?;
        let mut work = LedgerWork::new(cx.env);
        for expression in keys {
            work.step()?;
            let value = aggregate_value(expression, row, cx)?;
            #[cfg(test)]
            product_witnesses::phase(product_witnesses::Role::Key, &cx.mem);
            let key = HierarchyKey::new(value.get(), cx)?;
            values.push(value)?;
            comparisons.push(key)?;
        }
        #[cfg(test)]
        product_witnesses::phase(product_witnesses::Role::Args, &cx.mem);
        let mut args = PaidVec::new(aggregates.len(), &cx.mem, Some(cx.env))?;
        for aggregate in aggregates {
            work.step()?;
            args.push(match &aggregate.arg {
                Some(expression) => Some(aggregate_value(expression, row, cx)?),
                None => None,
            })?;
        }
        let width = keys
            .len()
            .checked_add(aggregates.len())
            .ok_or_else(invalid_size)?;
        for set in &mut self.sets {
            work.step()?;
            let tuple = aggregate_tuple(&set.idx, &comparisons, cx)?;
            let gi = match set.map.get(&tuple.value) {
                Some(&gi) => {
                    drop(tuple);
                    gi
                }
                None => {
                    let rep = aggregate_representatives(&set.idx, &values, width, cx)?;
                    let group = aggregate_group(rep, aggregates, cx)?;
                    #[cfg(test)]
                    product_witnesses::phase(product_witnesses::Role::Map, &cx.mem);
                    let prepared = set.map_model.prepare(&cx.mem, &mut self.temp)?;
                    #[cfg(test)]
                    payload_admission_tests::groups_request(&cx.mem);
                    #[cfg(test)]
                    product_witnesses::phase(product_witnesses::Role::Groups, &cx.mem);
                    hierarchy_reserve(&mut set.groups, 1, &cx.mem, cx.env, &mut self.temp)?;
                    let next = size_add(self.temp, size_add(group.bytes, tuple.bytes)?)?;
                    let gi = set.groups.len();
                    let (group, _) = group.into_parts();
                    let (tuple, _) = tuple.into_parts();
                    self.temp = next;
                    set.groups.push(group);
                    #[cfg(test)]
                    product_witnesses::hash_insert(&cx.mem);
                    set.map.insert(tuple, gi);
                    #[cfg(test)]
                    payload_admission_tests::groups_constructed(
                        set.groups.as_ptr() as usize,
                        set.groups.capacity(),
                    );
                    set.map_model.inserted(
                        prepared,
                        set.map.capacity(),
                        &cx.mem,
                        &mut self.temp,
                    )?;
                    #[cfg(test)]
                    product_witnesses::event(
                        product_witnesses::Point::GroupCommit,
                        &cx.mem,
                        0,
                        0,
                        0,
                    );
                    gi
                }
            };
            let group = set.groups.get_mut(gi).ok_or_else(invalid_size)?;
            for ((state, aggregate), value) in
                group.states.iter_mut().zip(aggregates).zip(args.iter())
            {
                work.step()?;
                if aggregate.f != Func::GroupedBy {
                    state.add(value.as_ref().map(AggregateValue::get), cx, &mut self.temp)?;
                }
            }
        }
        Ok(())
    }

    fn empty_total(&mut self, si: usize, aggregates: &[AggL]) -> Result<(), ExecError> {
        #[cfg(test)]
        payload_admission_tests::empty_total(&self.cx.mem);
        let rep = AggregateOwner::vector(0, self.cx)?;
        let group = aggregate_group(rep, aggregates, self.cx)?;
        let set = self
            .sets
            .get_mut(si)
            .ok_or_else(|| fatal("aggregate set index out of range"))?;
        #[cfg(test)]
        payload_admission_tests::groups_request(&self.cx.mem);
        #[cfg(test)]
        product_witnesses::phase(product_witnesses::Role::Groups, &self.cx.mem);
        hierarchy_reserve(
            &mut set.groups,
            1,
            &self.cx.mem,
            self.cx.env,
            &mut self.temp,
        )?;
        let next = size_add(self.temp, group.bytes)?;
        let (group, _) = group.into_parts();
        self.temp = next;
        set.groups.push(group);
        #[cfg(test)]
        payload_admission_tests::groups_constructed(
            set.groups.as_ptr() as usize,
            set.groups.capacity(),
        );
        Ok(())
    }

    fn output_group(
        &mut self,
        group: Group,
        indexes: &[usize],
        last_position: &[usize],
        keys: usize,
        aggregates: &[AggL],
    ) -> Result<(), ExecError> {
        let width = keys
            .checked_add(aggregates.len())
            .ok_or_else(invalid_size)?;
        let mut representatives = 0;
        let mut work = LedgerWork::new(self.cx.env);
        for (position, (&index, rep)) in indexes.iter().zip(&group.rep).enumerate() {
            work.step()?;
            if index < keys && aggregate_is_last(last_position, index, position)? {
                representatives = size_add(representatives, rep.bytes)?;
            }
        }
        let bytes = size_add(size_vec(width, size_of::<QValue>())?, representatives)?;
        self.cx.mem.check_value(bytes)?;
        #[cfg(test)]
        payload_admission_tests::phase(payload_admission_tests::Phase::Output, &self.cx.mem);
        #[cfg(test)]
        product_witnesses::phase(product_witnesses::Role::Output, &self.cx.mem);
        hierarchy_reserve(
            &mut self.out,
            1,
            &self.cx.mem,
            self.cx.env,
            &mut self.output,
        )?;
        #[cfg(test)]
        payload_admission_tests::phase(payload_admission_tests::Phase::Row, &self.cx.mem);
        #[cfg(test)]
        product_witnesses::phase(product_witnesses::Role::Row, &self.cx.mem);
        let mut row = PaidVec::new(width, &self.cx.mem, Some(self.cx.env))?;
        row.resize(keys, QValue::Null);
        for (position, (&index, rep)) in indexes.iter().zip(group.rep).enumerate() {
            work.step()?;
            if index < keys && aggregate_is_last(last_position, index, position)? {
                *row.get_mut(index)
                    .ok_or_else(|| fatal("aggregate output index out of range"))? = rep.value;
            }
        }
        #[cfg(test)]
        product_witnesses::phase(product_witnesses::Role::Row, &self.cx.mem);
        #[cfg(test)]
        product_witnesses::checkpoint(
            product_witnesses::Point::BeforeNative,
            &self.cx.mem,
            Some(self.cx.env.cancel),
            row.as_ptr() as usize,
            row.capacity(),
            0,
        )?;
        let mut legacy = 0;
        for (state, aggregate) in group.states.into_iter().zip(aggregates) {
            work.step()?;
            let value = if aggregate.f == Func::GroupedBy {
                QValue::Bool(grouped_by(aggregate, indexes))
            } else {
                state.native.finish(self.cx.env)?
            };
            legacy = size_add(legacy, value_model(&value))?;
            row.push(value)?;
        }
        let next_legacy = size_add(self.legacy, legacy)?;
        self.cx.mem.check_value(size_add(bytes, legacy)?)?;
        self.cx.mem.grow(legacy)?;
        self.legacy = next_legacy;
        let next_temp = self
            .temp
            .checked_sub(representatives)
            .ok_or_else(invalid_size)?;
        let next_output = size_add(self.output, bytes)?;
        let (row, _) = row.into_parts();
        self.temp = next_temp;
        self.output = next_output;
        self.out.push(row);
        #[cfg(test)]
        product_witnesses::event(product_witnesses::Point::RowCommit, &self.cx.mem, 0, 0, 0);
        Ok(())
    }

    fn execute(
        &mut self,
        input: &mut BoxOp<'a>,
        keys: &[PExpr],
        sets: Option<&[Vec<u16>]>,
        aggregates: &[AggL],
    ) -> Result<(), ExecError> {
        self.setup(keys.len(), sets)?;
        while let Some(row) = input.next(self.cx)? {
            #[cfg(test)]
            product_witnesses::event(product_witnesses::Point::Input, &self.cx.mem, 0, 0, 0);
            self.cx.tick()?;
            self.add_row(&row, keys, aggregates)?;
        }
        for si in 0..self.sets.len() {
            let set = self
                .sets
                .get(si)
                .ok_or_else(|| fatal("aggregate set index out of range"))?;
            if set.idx.is_empty() && set.groups.is_empty() {
                self.empty_total(si, aggregates)?;
            }
            let set = self
                .sets
                .get_mut(si)
                .ok_or_else(|| fatal("aggregate set index out of range"))?;
            let indexes = take(&mut set.idx);
            let last_position = take(&mut set.last_position);
            let mut groups = take(&mut set.groups).into_iter();
            for group in groups.by_ref() {
                self.output_group(group, &indexes, &last_position, keys.len(), aggregates)?;
            }
            drop(groups);
            drop(last_position);
            drop(indexes);
        }
        Ok(())
    }

    fn finish(mut self, keys: usize) -> Result<(Vec<Row>, u64, u64), ExecError> {
        let mut model = size_vec(self.out.capacity(), size_of::<Row>())?;
        let mut work = LedgerWork::new(self.cx.env);
        for row in &self.out {
            work.step()?;
            let mut bytes = size_vec(row.capacity(), size_of::<QValue>())?;
            for value in row.iter().take(keys) {
                bytes = size_add(
                    bytes,
                    totals_payload_model(value, &self.cx.mem, Some(self.cx.env))?,
                )?;
            }
            model = size_add(model, bytes)?;
        }
        if model != self.output {
            return Err(invalid_size());
        }
        size_add(self.output, self.legacy)?;
        #[cfg(test)]
        product_witnesses::phase(product_witnesses::Role::Build, &self.cx.mem);
        #[cfg(test)]
        if product_witnesses::early(product_witnesses::Point::BuildDrop, &self.cx.mem) {
            self.cx.mem.shrink(take(&mut self.temp));
        }
        #[cfg(test)]
        product_witnesses::snapshot(&self.cx.mem);
        drop(take(&mut self.sets));
        self.cx.mem.shrink(take(&mut self.temp));
        #[cfg(test)]
        payload_admission_tests::phase(payload_admission_tests::Phase::Output, &self.cx.mem);
        #[cfg(test)]
        product_witnesses::event(
            product_witnesses::Point::OutputReady,
            &self.cx.mem,
            self.out.as_ptr() as usize,
            self.out.capacity(),
            0,
        );
        let rows = take(&mut self.out);
        Ok((rows, take(&mut self.output), take(&mut self.legacy)))
    }
}

impl Drop for AggregateBuild<'_, '_> {
    fn drop(&mut self) {
        #[cfg(test)]
        payload_admission_tests::snapshot(&self.cx.mem);
        #[cfg(test)]
        product_witnesses::phase(product_witnesses::Role::Build, &self.cx.mem);
        #[cfg(test)]
        if product_witnesses::early(product_witnesses::Point::BuildDrop, &self.cx.mem) {
            self.cx.mem.shrink(take(&mut self.temp));
            self.cx.mem.shrink(take(&mut self.output));
        }
        #[cfg(test)]
        product_witnesses::snapshot(&self.cx.mem);
        drop(take(&mut self.sets));
        drop(take(&mut self.out));
        self.cx.mem.shrink(take(&mut self.temp));
        self.cx.mem.shrink(take(&mut self.output));
        self.cx.mem.shrink(take(&mut self.legacy));
        #[cfg(test)]
        product_witnesses::event(product_witnesses::Point::BuildEnd, &self.cx.mem, 0, 0, 0);
        #[cfg(test)]
        payload_admission_tests::snapshot(&self.cx.mem);
    }
}

pub struct HashAggregate<'a> {
    input: BoxOp<'a>,
    keys: Vec<PExpr>,
    sets: Option<Vec<Vec<u16>>>,
    aggs: Vec<AggL>,
    out: Option<std::vec::IntoIter<Row>>,
    held: u64,
    legacy: u64,
}

impl<'a> HashAggregate<'a> {
    pub fn new(
        input: BoxOp<'a>,
        keys: Vec<PExpr>,
        sets: Option<Vec<Vec<u16>>>,
        aggs: Vec<AggL>,
    ) -> Self {
        Self {
            input,
            keys,
            sets,
            aggs,
            out: None,
            held: 0,
            legacy: 0,
        }
    }

    fn run(&mut self, cx: &mut Cx<'a>) -> Result<Vec<Row>, ExecError> {
        let mut build = AggregateBuild::new(cx);
        build.execute(
            &mut self.input,
            &self.keys,
            self.sets.as_deref(),
            &self.aggs,
        )?;
        let (rows, held, legacy) = build.finish(self.keys.len())?;
        self.held = held;
        self.legacy = legacy;
        Ok(rows)
    }
}

fn grouped_by(aggregate: &AggL, set: &[usize]) -> bool {
    match &aggregate.arg {
        Some(PExpr::Lit(QValue::Number(number))) => number
            .to_i64()
            .and_then(|key| usize::try_from(key).ok())
            .is_some_and(|key| set.contains(&key)),
        _ => false,
    }
}

impl<'a> Op<'a> for HashAggregate<'a> {
    fn close(&mut self, cx: &mut Cx<'a>) {
        #[cfg(test)]
        product_witnesses::phase(product_witnesses::Role::Close, &cx.mem);
        #[cfg(test)]
        product_witnesses::event(product_witnesses::Point::Close, &cx.mem, 0, 0, 0);
        #[cfg(test)]
        payload_admission_tests::snapshot(&cx.mem);
        self.out = None;
        #[cfg(test)]
        product_witnesses::event(product_witnesses::Point::HTransfer, &cx.mem, 0, 0, 0);
        cx.mem.shrink(take(&mut self.held));
        cx.mem.shrink(take(&mut self.legacy));
        #[cfg(test)]
        payload_admission_tests::snapshot(&cx.mem);
        self.input.close(cx);
    }

    fn next(&mut self, cx: &mut Cx<'a>) -> Result<Option<Row>, ExecError> {
        if self.out.is_none() {
            let rows = self.run(cx)?;
            self.out = Some(rows.into_iter());
        }
        match self.out.as_mut().and_then(Iterator::next) {
            #[cfg(not(test))]
            Some(row) => Ok(Some(row)),
            #[cfg(test)]
            Some(row) => {
                product_witnesses::event(
                    product_witnesses::Point::Handoff,
                    &cx.mem,
                    row.as_ptr() as usize,
                    row.capacity(),
                    0,
                );
                Ok(Some(row))
            }
            None => {
                #[cfg(test)]
                payload_admission_tests::snapshot(&cx.mem);
                self.out = None;
                #[cfg(test)]
                product_witnesses::event(product_witnesses::Point::HTransfer, &cx.mem, 0, 0, 0);
                cx.mem.shrink(take(&mut self.held));
                cx.mem.shrink(take(&mut self.legacy));
                #[cfg(test)]
                payload_admission_tests::snapshot(&cx.mem);
                Ok(None)
            }
        }
    }
}
