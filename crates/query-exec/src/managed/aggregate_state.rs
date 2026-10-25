use super::{
    arithmetic::ArithmeticError,
    cast::integer_fits,
    collation::ManagedCollation,
    compare,
    expr::ExprError,
    key::{ManagedKey, try_key_of},
    map::ManagedMap,
};
use bsl_prim::{Number, RoundMode};
use memory_budget::{BudgetError, Category, MemoryBudget, Reservation, Shared};
use query_ir::{
    Dec,
    managed_value::{
        ManagedRow, ManagedValue, NumberBinaryOp, ScalarValue, ValueRef,
        row::{ScopedInput, ScopedRow},
    },
};
use query_syntax::Func;
use std::{cmp::Ordering, collections::hash_map::RandomState, hash::BuildHasher};

pub struct ManagedAggState {
    value: Option<Shared<ScopedRow>>,
    seen: Option<ManagedMap<ManagedKey, ()>>,
    hash: RandomState,
    function: Func,
    decimal: Dec,
    unlimited: bool,
    count: u64,
    charge: Reservation,
}
fn number(value: &ManagedValue) -> Result<&Number, ExprError> {
    match value.view() {
        ValueRef::Number(n) => Ok(n),
        _ => {
            Err(ArithmeticError::Argument("СУММА/СРЕДНЕЕ over a value that is not a number").into())
        }
    }
}
fn guard(value: &Number, limit: u64) -> Result<(), ExprError> {
    if value.size_hint_limbs() > limit {
        Err(ArithmeticError::NumberLimit.into())
    } else {
        Ok(())
    }
}
fn one(value: ManagedValue, budget: &MemoryBudget) -> Result<ScopedRow, ExprError> {
    let mut row = ManagedRow::try_with_capacity(budget, Category::Execution, 1)?;
    row.try_push(value)?;
    Ok(ScopedRow::try_from_owned(row)?)
}
fn count_value(mut value: u64, budget: &MemoryBudget) -> Result<ManagedValue, ExprError> {
    let mut digits = [0; 20];
    let mut start = digits.len();
    loop {
        start -= 1;
        *digits.get_mut(start).ok_or(BudgetError::Overflow)? =
            u8::try_from(value % 10).map_err(|_| BudgetError::Overflow)?;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    ManagedValue::try_number_from_decimal_digits(
        false,
        digits.get(start..).ok_or(BudgetError::Overflow)?,
        0,
        budget,
    )
    .map_err(ArithmeticError::from)?
    .ok_or_else(|| BudgetError::Overflow.into())
}
impl ManagedAggState {
    pub fn try_new(
        function: Func,
        decimal: Dec,
        distinct: bool,
        unlimited: bool,
        budget: &MemoryBudget,
    ) -> Result<Self, ExprError> {
        if !matches!(
            function,
            Func::Count | Func::Sum | Func::Avg | Func::Min | Func::Max
        ) {
            return Err(ExprError::Unsupported("aggregate function"));
        }
        if budget.scope_snapshot().is_some() {
            return Err(BudgetError::Overflow.into());
        }
        let nested = if distinct {
            std::mem::size_of::<ManagedMap<ManagedKey, ()>>()
        } else {
            0
        };
        let bytes = std::mem::size_of::<Self>()
            .checked_sub(nested)
            .ok_or(BudgetError::Overflow)?;
        let charge = budget
            .try_reserve(
                Category::Execution,
                u64::try_from(bytes).map_err(|_| BudgetError::Overflow)?,
            )
            .map_err(BudgetError::from)?;
        let seen = if distinct {
            Some(ManagedMap::new(budget)?)
        } else {
            None
        };
        Ok(Self {
            value: None,
            seen,
            hash: RandomState::new(),
            function,
            decimal,
            unlimited,
            count: 0,
            charge,
        })
    }
    pub fn add(
        &mut self,
        source: &ScopedRow,
        index: Option<usize>,
        coll: &ManagedCollation,
        max_number_limbs: u64,
    ) -> Result<(), ExprError> {
        if !source.budget().same_instance(self.charge.budget()) {
            return Err(BudgetError::Overflow.into());
        }
        coll.check_budget(source.budget())?;
        source
            .budget()
            .check_admission(Category::Execution)
            .map_err(BudgetError::from)?;
        let Some(index) = index else {
            self.count = self.count.checked_add(1).ok_or(BudgetError::Overflow)?;
            return Ok(());
        };
        let incoming = source.values().get(index).ok_or(ArithmeticError::Column {
            index,
            columns: source.values().len(),
        })?;
        if matches!(incoming.view(), ValueRef::Null) {
            return Ok(());
        }
        if let Some(seen) = &mut self.seen {
            let key = try_key_of(incoming.view(), coll, source.budget())?;
            let hash = self.hash.hash_one(&key);
            if seen.get(hash, &key).is_some() {
                return Ok(());
            }
            seen.try_insert(hash, key, ()).map_err(|e| e.error)?;
        }
        let imported = self
            .value
            .as_ref()
            .map(|value| ScopedInput::try_new(value.clone(), source.budget()))
            .transpose()?;
        let current = imported.as_ref().and_then(|row| row.values().first());
        let value = match self.function {
            Func::Count => None,
            Func::Sum | Func::Avg => {
                let incoming = number(incoming)?;
                guard(incoming, max_number_limbs)?;
                let zero = Number::zero();
                let left = match current {
                    Some(value) => number(value)?,
                    None => &zero,
                };
                let sum = ManagedValue::try_number_binary(
                    NumberBinaryOp::Add,
                    left,
                    incoming,
                    source.budget(),
                )
                .map_err(ArithmeticError::from)?;
                guard(number(&sum)?, max_number_limbs)?;
                Some(sum)
            }
            Func::Min | Func::Max => {
                let want = if self.function == Func::Min {
                    Ordering::Less
                } else {
                    Ordering::Greater
                };
                if let Some(current) = current {
                    if compare::try_cmp(incoming.view(), current.view(), coll, source.budget())?
                        == want
                    {
                        Some(incoming.clone())
                    } else {
                        None
                    }
                } else {
                    Some(incoming.clone())
                }
            }
            _ => return Err(ExprError::Unsupported("aggregate function")),
        };
        let replacement = value
            .map(|value| {
                one(value, source.budget())?
                    .try_into_shared()
                    .map_err(ExprError::from)
            })
            .transpose()?;
        if let Some(value) = replacement {
            self.value = Some(value);
        }
        self.count = self.count.checked_add(1).ok_or(BudgetError::Overflow)?;
        Ok(())
    }
    pub fn finish(
        self,
        budget: &MemoryBudget,
        max_number_limbs: u64,
    ) -> Result<ScopedRow, ExprError> {
        if budget.scope_snapshot().is_none() || !budget.same_instance(self.charge.budget()) {
            return Err(BudgetError::Overflow.into());
        }
        budget
            .check_admission(Category::Execution)
            .map_err(BudgetError::from)?;
        let imported = self.import_value(budget)?;
        self.finish_imported(
            budget,
            imported.as_ref().and_then(|row| row.values().first()),
            max_number_limbs,
        )
    }
    fn import_value(&self, budget: &MemoryBudget) -> Result<Option<ScopedInput>, ExprError> {
        Ok(self
            .value
            .as_ref()
            .map(|value| ScopedInput::try_new(value.clone(), budget))
            .transpose()?)
    }
    fn finish_imported(
        self,
        budget: &MemoryBudget,
        current: Option<&ManagedValue>,
        max_number_limbs: u64,
    ) -> Result<ScopedRow, ExprError> {
        let null = || ManagedValue::scalar(ScalarValue::Null);
        let value = match self.function {
            Func::Count => count_value(self.count, budget)?,
            Func::Min | Func::Max => current.cloned().unwrap_or_else(null),
            _ if self.count == 0 => null(),
            Func::Sum => {
                let value = match current {
                    Some(value) => value.clone(),
                    None => count_value(0, budget)?,
                };
                if !self.unlimited
                    && self.decimal.is_known()
                    && !integer_fits(
                        number(&value)?,
                        usize::from(38u8.saturating_sub(self.decimal.s)),
                    )
                {
                    return Err(ArithmeticError::CantFit.into());
                }
                value
            }
            Func::Avg => {
                let zero = Number::zero();
                let sum = match current {
                    Some(value) => number(value)?,
                    None => &zero,
                };
                let count = count_value(self.count, budget)?;
                let quotient = ManagedValue::try_number_binary(
                    NumberBinaryOp::Div,
                    sum,
                    number(&count)?,
                    budget,
                )
                .map_err(ArithmeticError::from)?;
                guard(number(&quotient)?, max_number_limbs)?;
                if self.unlimited || !self.decimal.is_known() {
                    quotient
                } else {
                    let scale = self.decimal.s.max(6);
                    let rounded = ManagedValue::try_number_round(
                        number(&quotient)?,
                        i64::from(scale),
                        RoundMode::HalfAwayFromZero,
                        budget,
                    )
                    .map_err(ArithmeticError::from)?;
                    if !integer_fits(number(&rounded)?, usize::from(38u8.saturating_sub(scale))) {
                        return Err(ArithmeticError::CantFit.into());
                    }
                    rounded
                }
            }
            _ => return Err(ExprError::Unsupported("aggregate function")),
        };
        one(value, budget)
    }
}

use super::{
    expr,
    stream::{StreamControl, StreamError},
};
use memory_budget::{BudgetedVec, RequestMemory};
use query_ir::{AggL, PExpr, QValue};
use std::{
    hash::{Hash, Hasher},
    sync::atomic::Ordering as AtomicOrdering,
    time::Instant,
};

struct TupleKey(BudgetedVec<Shared<ManagedKey>>);
impl PartialEq for TupleKey {
    fn eq(&self, other: &Self) -> bool {
        self.0.len() == other.0.len() && self.0.iter().zip(other.0.iter()).all(|(a, b)| **a == **b)
    }
}
impl Eq for TupleKey {}
impl Hash for TupleKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.len().hash(state);
        for key in self.0.iter() {
            (**key).hash(state);
        }
    }
}
#[derive(Clone, Copy)]
enum Indices<'a> {
    All(usize),
    Explicit(&'a [u16]),
}
impl Indices<'_> {
    fn len(self) -> usize {
        match self {
            Self::All(n) => n,
            Self::Explicit(v) => v.len(),
        }
    }
    fn get(self, at: usize) -> Option<usize> {
        match self {
            Self::All(n) => (at < n).then_some(at),
            Self::Explicit(v) => v.get(at).map(|&i| usize::from(i)),
        }
    }
    fn contains(self, index: usize) -> bool {
        match self {
            Self::All(n) => index < n,
            Self::Explicit(v) => v.iter().any(|&i| usize::from(i) == index),
        }
    }
}
struct Group {
    rep: Option<Shared<ScopedRow>>,
    states: BudgetedVec<Option<ManagedAggState>>,
}
impl Group {
    fn new(
        rep: Option<Shared<ScopedRow>>,
        aggs: &[AggL],
        budget: &MemoryBudget,
    ) -> Result<Self, ExprError> {
        let mut states = BudgetedVec::try_with_capacity(budget, Category::Execution, aggs.len())?;
        for agg in aggs {
            let function = if agg.f == Func::GroupedBy {
                Func::Count
            } else {
                agg.f
            };
            states.try_push(Some(ManagedAggState::try_new(
                function,
                agg.arg_dec,
                agg.distinct,
                false,
                budget,
            )?))?;
        }
        Ok(Self { rep, states })
    }
}
struct Set<'a> {
    indices: Indices<'a>,
    map: Option<ManagedMap<TupleKey, usize>>,
    groups: BudgetedVec<Option<Group>>,
    next: usize,
}

pub struct AggregateRows<'a, I> {
    input: Option<I>,
    sets: Option<BudgetedVec<Option<Set<'a>>>>,
    keys: &'a [PExpr],
    aggs: &'a [AggL],
    coll: &'a ManagedCollation,
    control: StreamControl<'a>,
    hash: RandomState,
    row_limit: u64,
    ready: bool,
    next_set: usize,
    ordinal: u64,
    charge: Reservation,
}
fn checkpoint<E>(control: StreamControl<'_>, budget: &MemoryBudget) -> Result<(), StreamError<E>> {
    if control.cancel.load(AtomicOrdering::Relaxed) {
        return Err(StreamError::Cancelled);
    }
    if control.stop.is_some_and(|stop| Instant::now() >= stop) {
        return Err(StreamError::Timeout);
    }
    budget
        .check_admission(Category::Execution)
        .map_err(BudgetError::from)?;
    Ok(())
}
impl<'a, I> AggregateRows<'a, I> {
    pub fn try_new(
        input: I,
        keys: &'a [PExpr],
        sets: Option<&'a [Vec<u16>]>,
        aggs: &'a [AggL],
        request: &RequestMemory,
        coll: &'a ManagedCollation,
        control: StreamControl<'a>,
    ) -> Result<Self, ExprError> {
        let budget = &request.budget;
        if budget.scope_snapshot().is_some() {
            return Err(BudgetError::Overflow.into());
        }
        coll.check_budget(budget)?;
        let bytes = std::mem::size_of::<Self>()
            .checked_sub(std::mem::size_of::<BudgetedVec<Option<Set<'a>>>>())
            .ok_or(BudgetError::Overflow)?;
        let charge = budget
            .try_reserve(
                Category::Execution,
                u64::try_from(bytes).map_err(|_| BudgetError::Overflow)?,
            )
            .map_err(BudgetError::from)?;
        let mut built = BudgetedVec::try_with_capacity(
            budget,
            Category::Execution,
            sets.map_or(1, <[Vec<u16>]>::len),
        )?;
        for at in 0..sets.map_or(1, <[Vec<u16>]>::len) {
            let indices = match sets {
                None => Indices::All(keys.len()),
                Some(sets) => Indices::Explicit(sets.get(at).ok_or(BudgetError::Overflow)?),
            };
            built.try_push(Some(Set {
                indices,
                map: Some(ManagedMap::new(budget)?),
                groups: BudgetedVec::new(budget, Category::Execution)?,
                next: 0,
            }))?;
        }
        Ok(Self {
            input: Some(input),
            sets: Some(built),
            keys,
            aggs,
            coll,
            control,
            hash: RandomState::new(),
            row_limit: request.row_limit,
            ready: false,
            next_set: 0,
            ordinal: 0,
            charge,
        })
    }
    fn finish(&mut self) {
        drop(self.input.take());
        drop(self.sets.take());
    }
    fn add<E>(&mut self, input: ScopedRow) -> Result<(), StreamError<E>> {
        let budget = input.budget();
        if !budget.same_instance(self.charge.budget()) {
            return Err(BudgetError::Overflow.into());
        }
        checkpoint(self.control, self.charge.budget())?;
        let count = self
            .keys
            .len()
            .checked_add(self.aggs.len())
            .ok_or(BudgetError::Overflow)?;
        let mut eager = ManagedRow::try_with_capacity(budget, Category::Execution, count)?;
        let mut encoded =
            BudgetedVec::try_with_capacity(budget, Category::Execution, self.keys.len())?;
        for expression in self.keys {
            checkpoint(self.control, self.charge.budget())?;
            let value =
                expr::try_eval(expression, &input, self.coll, self.control.max_number_limbs)?;
            let value = value.values().first().ok_or(ExprError::Column(0))?;
            let key = try_key_of(value.view(), self.coll, budget).map_err(ExprError::from)?;
            let charge = budget
                .try_reserve(Category::Execution, 0)
                .map_err(BudgetError::from)?;
            encoded.try_push(Shared::try_new(key, charge)?)?;
            eager.try_push(value.clone())?;
        }
        for agg in self.aggs {
            checkpoint(self.control, self.charge.budget())?;
            let value = match &agg.arg {
                Some(expression) => {
                    let value = expr::try_eval(
                        expression,
                        &input,
                        self.coll,
                        self.control.max_number_limbs,
                    )?;
                    value.values().first().ok_or(ExprError::Column(0))?.clone()
                }
                None => ManagedValue::scalar(ScalarValue::Null),
            };
            eager.try_push(value)?;
        }
        let eager = ScopedRow::try_from_owned(eager)?;
        let sets = self.sets.as_mut().ok_or(BudgetError::Overflow)?;
        for set in sets.iter_mut().flatten() {
            checkpoint(self.control, self.charge.budget())?;
            let valid = (0..set.indices.len())
                .filter_map(|at| set.indices.get(at))
                .filter(|&i| i < self.keys.len())
                .count();
            let mut tuple = BudgetedVec::try_with_capacity(budget, Category::Execution, valid)?;
            for at in 0..set.indices.len() {
                if let Some(key) = set.indices.get(at).and_then(|i| encoded.get(i)) {
                    tuple.try_push(key.clone())?;
                }
            }
            let tuple = TupleKey(tuple);
            let hash = self.hash.hash_one(&tuple);
            let map = set.map.as_mut().ok_or(BudgetError::Overflow)?;
            let at = match map.get(hash, &tuple).copied() {
                Some(at) => at,
                None => {
                    let mut rep =
                        ManagedRow::try_with_capacity(budget, Category::Execution, valid)?;
                    for at in 0..set.indices.len() {
                        if let Some(value) = set
                            .indices
                            .get(at)
                            .filter(|&i| i < self.keys.len())
                            .and_then(|i| eager.values().get(i))
                        {
                            rep.try_push(value.clone())?;
                        }
                    }
                    let rep = ScopedRow::try_from_owned(rep)?.try_into_shared()?;
                    let group = Group::new(Some(rep), self.aggs, self.charge.budget())?;
                    let at = set.groups.len();
                    set.groups.try_push(Some(group))?;
                    map.try_insert(hash, tuple, at).map_err(|e| e.error)?;
                    at
                }
            };
            let group = set
                .groups
                .get_mut(at)
                .and_then(Option::as_mut)
                .ok_or(BudgetError::Overflow)?;
            for (at, (state, agg)) in group.states.iter_mut().zip(self.aggs).enumerate() {
                checkpoint(self.control, self.charge.budget())?;
                state.as_mut().ok_or(BudgetError::Overflow)?.add(
                    &eager,
                    if agg.arg.is_some() {
                        Some(
                            self.keys
                                .len()
                                .checked_add(at)
                                .ok_or(BudgetError::Overflow)?,
                        )
                    } else {
                        None
                    },
                    self.coll,
                    self.control.max_number_limbs,
                )?;
            }
        }
        checkpoint(self.control, self.charge.budget())?;
        Ok(())
    }
    fn output<E>(
        &self,
        indices: Indices<'_>,
        mut group: Group,
    ) -> Result<ScopedRow, StreamError<E>> {
        checkpoint(self.control, self.charge.budget())?;
        let scope = self
            .charge
            .budget()
            .try_scope(Category::Row, self.row_limit)?;
        let rep = group
            .rep
            .as_ref()
            .map(|rep| ScopedInput::try_new(rep.clone(), &scope))
            .transpose()?;
        let mut imports =
            BudgetedVec::try_with_capacity(&scope, Category::Execution, group.states.len())?;
        for state in group.states.iter().flatten() {
            imports.try_push(state.import_value(&scope)?)?;
        }
        let count = self
            .keys
            .len()
            .checked_add(self.aggs.len())
            .ok_or(BudgetError::Overflow)?;
        let mut row = ManagedRow::try_with_capacity(&scope, Category::Execution, count)?;
        for _ in self.keys {
            row.try_push(ManagedValue::scalar(ScalarValue::Null))?;
        }
        if let Some(rep) = &rep {
            for (at, value) in rep.values().iter().enumerate() {
                if let Some(slot) = indices.get(at).and_then(|index| row.get_mut(index)) {
                    *slot = value.clone();
                }
            }
        }
        for (at, agg) in self.aggs.iter().enumerate() {
            checkpoint(self.control, self.charge.budget())?;
            let state = group
                .states
                .get_mut(at)
                .and_then(Option::take)
                .ok_or(BudgetError::Overflow)?;
            let value = if agg.f == Func::GroupedBy {
                let included = match &agg.arg {
                    Some(PExpr::Lit(QValue::Number(n))) => n
                        .to_i64()
                        .and_then(|i| usize::try_from(i).ok())
                        .is_some_and(|i| indices.contains(i)),
                    _ => false,
                };
                ManagedValue::scalar(ScalarValue::Bool(included))
            } else {
                let current = imports
                    .get(at)
                    .and_then(Option::as_ref)
                    .and_then(|row| row.values().first());
                let finished =
                    state.finish_imported(&scope, current, self.control.max_number_limbs)?;
                finished
                    .values()
                    .first()
                    .ok_or(ExprError::Column(0))?
                    .clone()
            };
            row.try_push(value)?;
        }
        checkpoint(self.control, self.charge.budget())?;
        Ok(ScopedRow::try_from_owned(row)?)
    }
}
impl<I, E> AggregateRows<'_, I>
where
    I: Iterator<Item = Result<(u64, ScopedRow), StreamError<E>>>,
{
    fn load(&mut self) -> Result<(), StreamError<E>> {
        loop {
            checkpoint(self.control, self.charge.budget())?;
            let Some((_, row)) = self.input.as_mut().and_then(Iterator::next).transpose()? else {
                break;
            };
            self.add(row)?;
        }
        drop(self.input.take());
        checkpoint(self.control, self.charge.budget())?;
        for set in self
            .sets
            .as_mut()
            .ok_or(BudgetError::Overflow)?
            .iter_mut()
            .flatten()
        {
            if set.indices.len() == 0 && set.groups.is_empty() {
                set.groups
                    .try_push(Some(Group::new(None, self.aggs, self.charge.budget())?))?;
            }
            drop(set.map.take());
        }
        self.ready = true;
        Ok(())
    }
    fn next_row(&mut self) -> Result<Option<(u64, ScopedRow)>, StreamError<E>> {
        if !self.ready {
            self.load()?;
        }
        checkpoint(self.control, self.charge.budget())?;
        loop {
            let sets = self.sets.as_mut().ok_or(BudgetError::Overflow)?;
            let Some(slot) = sets.get_mut(self.next_set) else {
                return Ok(None);
            };
            let set = slot.as_mut().ok_or(BudgetError::Overflow)?;
            let Some(group) = set.groups.get_mut(set.next).and_then(Option::take) else {
                drop(slot.take());
                self.next_set = self.next_set.checked_add(1).ok_or(BudgetError::Overflow)?;
                continue;
            };
            set.next = set.next.checked_add(1).ok_or(BudgetError::Overflow)?;
            let indices = set.indices;
            let row = self.output(indices, group)?;
            let ordinal = self.ordinal;
            self.ordinal = self.ordinal.checked_add(1).ok_or(BudgetError::Overflow)?;
            let sets = self.sets.as_mut().ok_or(BudgetError::Overflow)?;
            while let Some(slot) = sets.get_mut(self.next_set) {
                if slot.as_ref().is_some_and(|set| set.next < set.groups.len()) {
                    break;
                }
                drop(slot.take());
                self.next_set = self.next_set.checked_add(1).ok_or(BudgetError::Overflow)?;
            }
            if self.next_set == sets.len() {
                self.finish();
            }
            return Ok(Some((ordinal, row)));
        }
    }
}
impl<I, E> Iterator for AggregateRows<'_, I>
where
    I: Iterator<Item = Result<(u64, ScopedRow), StreamError<E>>>,
{
    type Item = Result<(u64, ScopedRow), StreamError<E>>;
    fn next(&mut self) -> Option<Self::Item> {
        self.sets.as_ref()?;
        match self.next_row() {
            Ok(Some(row)) => Some(Ok(row)),
            Ok(None) => {
                self.finish();
                None
            }
            Err(error) => {
                self.finish();
                Some(Err(error))
            }
        }
    }
}
impl<I, E> std::iter::FusedIterator for AggregateRows<'_, I> where
    I: Iterator<Item = Result<(u64, ScopedRow), StreamError<E>>>
{
}
