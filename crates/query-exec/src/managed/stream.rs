use super::{
    collation::ManagedCollation,
    expr::{self, ExprError},
};
use memory_budget::{BudgetError, Category, MemoryBudget, Reservation};
use query_ir::{
    PExpr,
    managed_value::{ManagedRow, ValueRef, row::ScopedRow},
};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

#[derive(Clone, Copy, Debug)]
pub enum Stage<'a> {
    Filter(&'a PExpr),
    Project(&'a [PExpr]),
}
#[derive(Clone, Copy, Debug)]
pub struct StreamControl<'a> {
    pub cancel: &'a AtomicBool,
    pub stop: Option<Instant>,
    pub max_number_limbs: u64,
}
#[derive(Debug, PartialEq, Eq)]
pub enum StreamError<E> {
    Upstream(E),
    Memory(BudgetError),
    Expr(ExprError),
    Cancelled,
    Timeout,
}
impl<E> From<BudgetError> for StreamError<E> {
    fn from(error: BudgetError) -> Self {
        Self::Memory(error)
    }
}
impl<E> From<ExprError> for StreamError<E> {
    fn from(error: ExprError) -> Self {
        Self::Expr(error)
    }
}
impl<E: std::fmt::Display> std::fmt::Display for StreamError<E> {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Upstream(e) => e.fmt(output),
            Self::Memory(e) => e.fmt(output),
            Self::Expr(e) => e.fmt(output),
            Self::Cancelled => output.write_str("query cancelled"),
            Self::Timeout => output.write_str("query timed out"),
        }
    }
}
impl<E: std::error::Error + 'static> std::error::Error for StreamError<E> {}

pub struct TransformRows<'a, I> {
    input: Option<I>,
    stage: Stage<'a>,
    coll: &'a ManagedCollation,
    control: StreamControl<'a>,
    charge: Reservation,
}
impl<'a, I> TransformRows<'a, I> {
    pub fn try_new(
        input: I,
        stage: Stage<'a>,
        budget: &MemoryBudget,
        coll: &'a ManagedCollation,
        control: StreamControl<'a>,
    ) -> Result<Self, ExprError> {
        if budget.scope_snapshot().is_some() {
            return Err(BudgetError::Overflow.into());
        }
        coll.check_budget(budget)?;
        let charge = budget
            .try_reserve(
                Category::Execution,
                u64::try_from(std::mem::size_of::<Self>()).map_err(|_| BudgetError::Overflow)?,
            )
            .map_err(BudgetError::from)?;
        Ok(Self {
            input: Some(input),
            stage,
            coll,
            control,
            charge,
        })
    }
    fn checkpoint<E>(&self) -> Result<(), StreamError<E>> {
        if self.control.cancel.load(Ordering::Relaxed) {
            return Err(StreamError::Cancelled);
        }
        if self.control.stop.is_some_and(|stop| Instant::now() >= stop) {
            return Err(StreamError::Timeout);
        }
        self.charge
            .budget()
            .check_admission(Category::Execution)
            .map_err(BudgetError::from)?;
        Ok(())
    }
    fn transform<E>(&self, input: ScopedRow) -> Result<Option<ScopedRow>, StreamError<E>> {
        if !input.budget().same_instance(self.charge.budget()) {
            return Err(BudgetError::Overflow.into());
        }
        self.checkpoint()?;
        let result = match self.stage {
            Stage::Filter(predicate) => {
                let test =
                    expr::try_eval(predicate, &input, self.coll, self.control.max_number_limbs)?;
                let keep = matches!(
                    test.values().first().map(|v| v.view()),
                    Some(ValueRef::Bool(true))
                );
                if keep { Some(input) } else { None }
            }
            Stage::Project(expressions) => {
                let mut output = ManagedRow::try_with_capacity(
                    input.budget(),
                    Category::Execution,
                    expressions.len(),
                )?;
                for expression in expressions {
                    self.checkpoint()?;
                    let value = expr::try_eval(
                        expression,
                        &input,
                        self.coll,
                        self.control.max_number_limbs,
                    )?;
                    output.try_push(value.values().first().ok_or(ExprError::Column(0))?.clone())?;
                }
                Some(ScopedRow::try_from_owned(output)?)
            }
        };
        self.checkpoint()?;
        Ok(result)
    }
}
impl<I, E> Iterator for TransformRows<'_, I>
where
    I: Iterator<Item = Result<(u64, ScopedRow), StreamError<E>>>,
{
    type Item = Result<(u64, ScopedRow), StreamError<E>>;
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            self.input.as_ref()?;
            let next = self
                .checkpoint()
                .and_then(|()| self.input.as_mut().and_then(Iterator::next).transpose());
            match next {
                Ok(None) => {
                    drop(self.input.take());
                    return None;
                }
                Err(error) => {
                    drop(self.input.take());
                    return Some(Err(error));
                }
                Ok(Some((id, row))) => match self.transform(row) {
                    Ok(Some(row)) => return Some(Ok((id, row))),
                    Ok(None) => continue,
                    Err(error) => {
                        drop(self.input.take());
                        return Some(Err(error));
                    }
                },
            }
        }
    }
}
impl<I, E> std::iter::FusedIterator for TransformRows<'_, I> where
    I: Iterator<Item = Result<(u64, ScopedRow), StreamError<E>>>
{
}
