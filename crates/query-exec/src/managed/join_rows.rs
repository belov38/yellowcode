use super::super::{
    collation::ManagedCollation,
    expr::{self, ExprError},
    key::{ManagedKey, try_key_of},
    map::ManagedMap,
    stream::{StreamControl, StreamError},
};
use memory_budget::{BudgetError, BudgetedVec, Category, RequestMemory, Reservation, Shared};
use query_ir::{
    PExpr,
    managed_value::{
        ManagedRow, ManagedValue, ScalarValue, ValueRef,
        row::{ScopedInput, ScopedRow},
    },
};
use query_syntax::ast::JoinKind;
use std::{
    collections::hash_map::RandomState,
    hash::{BuildHasher, Hash, Hasher},
    sync::atomic::Ordering,
    time::Instant,
};

#[derive(Clone, Copy)]
pub enum JoinMode<'a> {
    Nested,
    Hash {
        left: &'a [PExpr],
        right: &'a [PExpr],
    },
}
#[derive(Clone, Copy)]
pub struct JoinSpec<'a> {
    pub kind: JoinKind,
    pub mode: JoinMode<'a>,
    pub on: Option<&'a PExpr>,
    pub left_width: usize,
    pub right_width: usize,
}
#[derive(Debug)]
struct TupleKey(BudgetedVec<ManagedKey>);
impl PartialEq for TupleKey {
    fn eq(&self, other: &Self) -> bool {
        self.0.as_slice() == other.0.as_slice()
    }
}
impl Eq for TupleKey {}
impl Hash for TupleKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.as_slice().hash(state);
    }
}
type Table = ManagedMap<TupleKey, BudgetedVec<usize>>;
struct State {
    rows: BudgetedVec<Shared<ScopedRow>>,
    matched: BudgetedVec<bool>,
    table: Option<Table>,
    current: Option<Shared<ScopedRow>>,
    key: Option<TupleKey>,
    hash: u64,
    pos: usize,
    current_matched: bool,
    built: bool,
    tail: Option<usize>,
    ordinal: u64,
}
pub struct JoinRows<'a, I, J> {
    left: Option<I>,
    right: Option<J>,
    state: Option<State>,
    spec: JoinSpec<'a>,
    request: &'a RequestMemory,
    coll: &'a ManagedCollation,
    control: StreamControl<'a>,
    hasher: RandomState,
    _charge: Reservation,
}
fn checkpoint<E>(
    request: &RequestMemory,
    control: StreamControl<'_>,
) -> Result<(), StreamError<E>> {
    if control.cancel.load(Ordering::Relaxed) {
        return Err(StreamError::Cancelled);
    }
    if control.stop.is_some_and(|stop| Instant::now() >= stop) {
        return Err(StreamError::Timeout);
    }
    request
        .budget
        .check_admission(Category::Execution)
        .map_err(BudgetError::from)?;
    Ok(())
}
impl<'a, I, J> JoinRows<'a, I, J> {
    pub fn try_new(
        left: I,
        right: J,
        spec: JoinSpec<'a>,
        request: &'a RequestMemory,
        coll: &'a ManagedCollation,
        control: StreamControl<'a>,
    ) -> Result<Self, ExprError> {
        if spec.kind == JoinKind::Right {
            return Err(ExprError::Unsupported("RIGHT join must be lowered to LEFT"));
        }
        if request.budget.scope_snapshot().is_some() {
            return Err(BudgetError::Overflow.into());
        }
        coll.check_budget(&request.budget)?;
        let nested = std::mem::size_of::<BudgetedVec<Shared<ScopedRow>>>()
            .checked_add(std::mem::size_of::<BudgetedVec<bool>>())
            .ok_or(BudgetError::Overflow)?
            .checked_add(if matches!(spec.mode, JoinMode::Hash { .. }) {
                std::mem::size_of::<Table>()
            } else {
                0
            })
            .ok_or(BudgetError::Overflow)?;
        let bytes = std::mem::size_of::<Self>()
            .checked_sub(nested)
            .ok_or(BudgetError::Overflow)?;
        let charge = request
            .budget
            .try_reserve(
                Category::Execution,
                u64::try_from(bytes).map_err(|_| BudgetError::Overflow)?,
            )
            .map_err(BudgetError::from)?;
        let rows = BudgetedVec::new(&request.budget, Category::Execution)?;
        let matched = BudgetedVec::new(&request.budget, Category::Execution)?;
        let table = match spec.mode {
            JoinMode::Nested => None,
            JoinMode::Hash { .. } => Some(ManagedMap::new(&request.budget)?),
        };
        Ok(Self {
            left: Some(left),
            right: Some(right),
            state: Some(State {
                rows,
                matched,
                table,
                current: None,
                key: None,
                hash: 0,
                pos: 0,
                current_matched: false,
                built: false,
                tail: None,
                ordinal: 0,
            }),
            spec,
            request,
            coll,
            control,
            hasher: RandomState::new(),
            _charge: charge,
        })
    }
    fn finish(&mut self) {
        drop(self.left.take());
        drop(self.right.take());
        drop(self.state.take());
    }
    fn share(&self, row: ScopedRow) -> Result<Shared<ScopedRow>, BudgetError> {
        if !row.budget().same_instance(&self.request.budget) {
            return Err(BudgetError::Overflow);
        }
        row.try_into_shared()
    }
    fn keys<E>(
        &self,
        source: Shared<ScopedRow>,
        expressions: &[PExpr],
    ) -> Result<Option<TupleKey>, StreamError<E>> {
        let scope = self
            .request
            .budget
            .try_scope(Category::Row, self.request.row_limit)?;
        let imported = ScopedInput::try_new(source, &scope)?;
        let mut row =
            ManagedRow::try_with_capacity(&scope, Category::Execution, imported.values().len())?;
        for value in imported.values() {
            row.try_push(value.clone())?;
        }
        let row = ScopedRow::try_from_owned(row)?;
        let mut keys =
            BudgetedVec::try_with_capacity(&scope, Category::Execution, expressions.len())?;
        for expression in expressions {
            checkpoint(self.request, self.control)?;
            let value = expr::try_eval(expression, &row, self.coll, self.control.max_number_limbs)?;
            checkpoint(self.request, self.control)?;
            let value = value.values().first().ok_or(ExprError::Column(0))?;
            if matches!(value.view(), ValueRef::Null) {
                return Ok(None);
            }
            keys.try_push(try_key_of(value.view(), self.coll, &scope).map_err(ExprError::from)?)?;
            checkpoint(self.request, self.control)?;
        }
        Ok(Some(TupleKey(keys)))
    }
    fn joined<E>(
        &self,
        left: Option<Shared<ScopedRow>>,
        right: Option<Shared<ScopedRow>>,
        predicate: bool,
    ) -> Result<Option<ScopedRow>, StreamError<E>> {
        checkpoint(self.request, self.control)?;
        let scope = self
            .request
            .budget
            .try_scope(Category::Row, self.request.row_limit)?;
        let left = left
            .map(|row| ScopedInput::try_new(row, &scope))
            .transpose()?;
        let right = right
            .map(|row| ScopedInput::try_new(row, &scope))
            .transpose()?;
        let l = left
            .as_ref()
            .map_or(self.spec.left_width, |row| row.values().len());
        let r = right
            .as_ref()
            .map_or(self.spec.right_width, |row| row.values().len());
        let count = l.checked_add(r).ok_or(BudgetError::Overflow)?;
        let mut out = ManagedRow::try_with_capacity(&scope, Category::Execution, count)?;
        for (input, width) in [(&left, l), (&right, r)] {
            for at in 0..width {
                checkpoint(self.request, self.control)?;
                out.try_push(match input {
                    Some(row) => row.values().get(at).ok_or(BudgetError::Overflow)?.clone(),
                    None => ManagedValue::scalar(ScalarValue::Null),
                })?;
            }
        }
        let out = ScopedRow::try_from_owned(out)?;
        checkpoint(self.request, self.control)?;
        if predicate && let Some(on) = self.spec.on {
            let value = expr::try_eval(on, &out, self.coll, self.control.max_number_limbs)?;
            checkpoint(self.request, self.control)?;
            if !matches!(
                value.values().first().map(|v| v.view()),
                Some(ValueRef::Bool(true))
            ) {
                return Ok(None);
            }
        }
        checkpoint(self.request, self.control)?;
        Ok(Some(out))
    }
    fn emit<E>(&mut self, row: ScopedRow) -> Result<Option<(u64, ScopedRow)>, StreamError<E>> {
        let state = self.state.as_mut().ok_or(BudgetError::Overflow)?;
        let ordinal = state.ordinal;
        state.ordinal = ordinal.checked_add(1).ok_or(BudgetError::Overflow)?;
        Ok(Some((ordinal, row)))
    }
}
impl<I, J, E> JoinRows<'_, I, J>
where
    I: Iterator<Item = Result<(u64, ScopedRow), StreamError<E>>>,
    J: Iterator<Item = Result<(u64, ScopedRow), StreamError<E>>>,
{
    fn build(&mut self) -> Result<(), StreamError<E>> {
        loop {
            checkpoint(self.request, self.control)?;
            let next = self.right.as_mut().and_then(Iterator::next).transpose()?;
            checkpoint(self.request, self.control)?;
            let Some((_, row)) = next else {
                break;
            };
            let row = self.share(row)?;
            let key = match self.spec.mode {
                JoinMode::Nested => None,
                JoinMode::Hash { right, .. } => self.keys(row.clone(), right)?,
            };
            let state = self.state.as_mut().ok_or(BudgetError::Overflow)?;
            let index = state.rows.len();
            if let Some(key) = key {
                let hash = self.hasher.hash_one(&key);
                let table = state.table.as_mut().ok_or(BudgetError::Overflow)?;
                if let Some(indices) = table.get_mut(hash, &key) {
                    indices.try_push(index)?;
                } else {
                    let mut indices = BudgetedVec::new(&self.request.budget, Category::Execution)?;
                    indices.try_push(index)?;
                    table
                        .try_insert(hash, key, indices)
                        .map_err(|error| error.error)?;
                }
            }
            state.rows.try_push(row)?;
            state.matched.try_push(false)?;
            checkpoint(self.request, self.control)?;
        }
        drop(self.right.take());
        self.state.as_mut().ok_or(BudgetError::Overflow)?.built = true;
        Ok(())
    }
    fn step(&mut self) -> Result<Option<(u64, ScopedRow)>, StreamError<E>> {
        checkpoint(self.request, self.control)?;
        if !self.state.as_ref().ok_or(BudgetError::Overflow)?.built {
            self.build()?;
        }
        loop {
            checkpoint(self.request, self.control)?;
            let state = self.state.as_ref().ok_or(BudgetError::Overflow)?;
            if let Some(at) = state.tail {
                let right = state.rows.get(at).cloned();
                let Some(right) = right else {
                    return Ok(None);
                };
                let matched = *state.matched.get(at).ok_or(BudgetError::Overflow)?;
                self.state.as_mut().ok_or(BudgetError::Overflow)?.tail =
                    Some(at.checked_add(1).ok_or(BudgetError::Overflow)?);
                if !matched {
                    let out = self
                        .joined(None, Some(right), false)?
                        .ok_or(BudgetError::Overflow)?;
                    return self.emit(out);
                }
                continue;
            }
            if state.current.is_none() {
                let next = self.left.as_mut().and_then(Iterator::next).transpose()?;
                checkpoint(self.request, self.control)?;
                let Some((_, row)) = next else {
                    drop(self.left.take());
                    if self.spec.kind == JoinKind::Full {
                        self.state.as_mut().ok_or(BudgetError::Overflow)?.tail = Some(0);
                        continue;
                    }
                    return Ok(None);
                };
                let row = self.share(row)?;
                let key = match self.spec.mode {
                    JoinMode::Nested => None,
                    JoinMode::Hash { left, .. } => self.keys(row.clone(), left)?,
                };
                let hash = key.as_ref().map_or(0, |key| self.hasher.hash_one(key));
                let state = self.state.as_mut().ok_or(BudgetError::Overflow)?;
                state.current = Some(row);
                state.key = key;
                state.hash = hash;
                state.pos = 0;
                state.current_matched = false;
            }
            let state = self.state.as_ref().ok_or(BudgetError::Overflow)?;
            let candidate = match self.spec.mode {
                JoinMode::Nested => (state.pos < state.rows.len()).then_some(state.pos),
                JoinMode::Hash { .. } => state
                    .key
                    .as_ref()
                    .and_then(|key| state.table.as_ref()?.get(state.hash, key)?.get(state.pos))
                    .copied(),
            };
            if let Some(at) = candidate {
                let left = state.current.clone().ok_or(BudgetError::Overflow)?;
                let right = state.rows.get(at).cloned().ok_or(BudgetError::Overflow)?;
                self.state.as_mut().ok_or(BudgetError::Overflow)?.pos =
                    state.pos.checked_add(1).ok_or(BudgetError::Overflow)?;
                if let Some(row) = self.joined(Some(left), Some(right), true)? {
                    let state = self.state.as_mut().ok_or(BudgetError::Overflow)?;
                    state.current_matched = true;
                    *state.matched.get_mut(at).ok_or(BudgetError::Overflow)? = true;
                    return self.emit(row);
                }
                continue;
            }
            let state = self.state.as_mut().ok_or(BudgetError::Overflow)?;
            let current = state.current.take().ok_or(BudgetError::Overflow)?;
            drop(state.key.take());
            if !state.current_matched && self.spec.kind != JoinKind::Inner {
                let row = self
                    .joined(Some(current), None, false)?
                    .ok_or(BudgetError::Overflow)?;
                return self.emit(row);
            }
        }
    }
}
impl<I, J, E> Iterator for JoinRows<'_, I, J>
where
    I: Iterator<Item = Result<(u64, ScopedRow), StreamError<E>>>,
    J: Iterator<Item = Result<(u64, ScopedRow), StreamError<E>>>,
{
    type Item = Result<(u64, ScopedRow), StreamError<E>>;
    fn next(&mut self) -> Option<Self::Item> {
        self.state.as_ref()?;
        match self.step() {
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
impl<I, J, E> std::iter::FusedIterator for JoinRows<'_, I, J>
where
    I: Iterator<Item = Result<(u64, ScopedRow), StreamError<E>>>,
    J: Iterator<Item = Result<(u64, ScopedRow), StreamError<E>>>,
{
}
