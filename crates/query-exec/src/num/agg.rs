use std::cmp::Ordering;
use std::collections::HashSet;

use bsl_prim::Number;
use query_ir::{Dec, ExecError, QValue};
use query_syntax::Func;

use super::scale::{cant_fit, div_by_zero, div_round, fits, guard};
use crate::coll::ExecCollation;
use crate::eval::try_cmp;
use crate::key::{Key, try_key_of};
use crate::model::HashModel;
use crate::source::ExecEnv;

pub struct AggState {
    f: Func,
    dec: Dec,
    unlimited: bool,
    sum: Number,
    count: u64,
    rep: Option<QValue>,
    seen: Option<HashSet<Key>>,
    seen_model: HashModel,
}

impl AggState {
    pub fn new(f: Func, dec: Dec, distinct: bool, unlimited: bool) -> Result<AggState, ExecError> {
        if !matches!(
            f,
            Func::Sum | Func::Avg | Func::Count | Func::Min | Func::Max
        ) {
            return Err(ExecError::Fatal(format!("num: {f:?} is not an aggregate")));
        }
        Ok(AggState {
            f,
            dec,
            unlimited,
            sum: Number::zero(),
            count: 0,
            rep: None,
            seen: distinct.then(HashSet::new),
            seen_model: HashModel::new(std::mem::size_of::<Key>() as u64),
        })
    }

    pub fn is_distinct(&self) -> bool {
        self.seen.is_some()
    }

    pub fn distinct_growth(&self, k: &Key) -> Option<u64> {
        let seen = self.seen.as_ref()?;
        if seen.contains(k) {
            return None;
        }
        Some(self.seen_model.growth())
    }

    pub fn add_distinct(
        &mut self,
        k: Key,
        v: &QValue,
        env: &ExecEnv<'_>,
        coll: ExecCollation<'_>,
    ) -> Result<u64, ExecError> {
        let Some(seen) = &mut self.seen else {
            return self.add_with(Some(v), env, coll).map(|()| 0);
        };
        if !seen.insert(k) {
            return Ok(0);
        }
        let freed = self.seen_model.inserted();
        self.add_value(v, env, coll)?;
        Ok(freed)
    }

    pub fn add(&mut self, v: Option<&QValue>, env: &ExecEnv<'_>) -> Result<(), ExecError> {
        self.add_with(v, env, ExecCollation::Legacy(env.collation))
    }

    pub fn add_with(
        &mut self,
        v: Option<&QValue>,
        env: &ExecEnv<'_>,
        coll: ExecCollation<'_>,
    ) -> Result<(), ExecError> {
        let Some(v) = v else {
            self.count += 1;
            return Ok(());
        };
        if matches!(v, QValue::Null) {
            return Ok(());
        }
        if let Some(seen) = &mut self.seen
            && !seen.insert(try_key_of(v, coll)?)
        {
            return Ok(());
        }
        self.add_value(v, env, coll)
    }

    fn add_value(
        &mut self,
        v: &QValue,
        env: &ExecEnv<'_>,
        coll: ExecCollation<'_>,
    ) -> Result<(), ExecError> {
        match self.f {
            Func::Count => self.count += 1,
            Func::Sum | Func::Avg => {
                let QValue::Number(n) = v else {
                    return Err(ExecError::Fatal(
                        "num: СУММА/СРЕДНЕЕ over a value that is not a number".to_string(),
                    ));
                };
                guard(env, n)?;
                self.sum = self.sum.add(n);
                guard(env, &self.sum)?;
                self.count += 1;
            }
            Func::Min | Func::Max => {
                let want = if self.f == Func::Min {
                    Ordering::Less
                } else {
                    Ordering::Greater
                };
                match &self.rep {
                    Some(cur) if try_cmp(v, cur, coll)? != want => {}
                    _ => self.rep = Some(v.clone()),
                }
                self.count += 1;
            }
            _ => {}
        }
        Ok(())
    }

    pub fn finish(self, env: &ExecEnv<'_>) -> Result<QValue, ExecError> {
        Ok(match self.f {
            Func::Count => QValue::Number(Number::from_u64(self.count)),
            Func::Min | Func::Max => self.rep.unwrap_or(QValue::Null),
            _ if self.count == 0 => QValue::Null,
            Func::Sum => {
                if !self.unlimited && self.dec.is_known() {
                    let total = Dec {
                        p: 38,
                        s: self.dec.s,
                    };
                    if !fits(&self.sum, total) {
                        return Err(cant_fit());
                    }
                }
                QValue::Number(self.sum)
            }
            _ => {
                let n = Number::from_u64(self.count);
                if self.unlimited || !self.dec.is_known() {
                    let v = self.sum.div(&n).map_err(|_| div_by_zero())?;
                    guard(env, &v)?;
                    QValue::Number(v)
                } else {
                    let s = self.dec.s.max(6);
                    let v = div_round(&self.sum, &n, s, env)?;
                    if !fits(&v, Dec { p: 38, s }) {
                        return Err(cant_fit());
                    }
                    QValue::Number(v)
                }
            }
        })
    }
}

fn accumulator_guard(env: &ExecEnv<'_>, number: &Number) -> Result<(), ExecError> {
    guard(env, number)
}
fn accumulator_cant_fit() -> ExecError {
    cant_fit()
}

pub(crate) struct BudgetedAggState {
    native: AggState,
    payload: u64,
}
impl BudgetedAggState {
    pub(crate) fn new(f: Func, dec: Dec) -> Result<Self, ExecError> {
        Ok(Self {
            native: AggState::new(f, dec, false, false)?,
            payload: 0,
        })
    }
    pub(crate) fn add<'a>(
        &mut self,
        value: Option<&QValue>,
        env: &ExecEnv<'a>,
        coll: ExecCollation<'_>,
        mem: &crate::ops::MemBudget<'a>,
        temp: &mut u64,
    ) -> Result<(), ExecError> {
        self.native
            .add_paid(value, env, coll, mem, &mut self.payload, temp)
    }
    pub(crate) fn finish<'m, 'a>(
        mut self,
        env: &ExecEnv<'a>,
        mem: &'m crate::ops::MemBudget<'a>,
        temp: &mut u64,
    ) -> Result<crate::totals::payload::PaidValue<'m, 'a>, ExecError> {
        use crate::model::invalid_size;
        use crate::totals::payload::{self, PaidValue};
        let next = temp.checked_sub(self.payload).ok_or_else(invalid_size)?;
        let value = if matches!(self.native.f, Func::Min | Func::Max) {
            self.native.rep.take().unwrap_or(QValue::Null)
        } else {
            QValue::Number(std::mem::replace(&mut self.native.sum, Number::zero()))
        };
        let owned = PaidValue::from_paid_parts(value, self.payload, mem);
        *temp = next;
        let state = self.native;
        if state.f == Func::Count {
            return payload::count(state.count, mem, Some(env));
        }
        if matches!(state.f, Func::Min | Func::Max) {
            return Ok(owned);
        }
        if state.count == 0 {
            return Ok(PaidValue::from_paid_parts(QValue::Null, 0, mem));
        }
        let QValue::Number(sum) = &*owned else {
            return Err(invalid_size());
        };
        if state.f == Func::Sum {
            if !state.unlimited
                && state.dec.is_known()
                && !fits(
                    sum,
                    Dec {
                        p: 38,
                        s: state.dec.s,
                    },
                )
            {
                return Err(accumulator_cant_fit());
            }
            return Ok(owned);
        }
        let denominator = payload::count(state.count, mem, Some(env))?;
        let QValue::Number(denominator_number) = &*denominator else {
            return Err(invalid_size());
        };
        let quotient = payload::divide_numbers(sum, denominator_number, mem, env)?;
        let QValue::Number(number) = &*quotient else {
            return Err(invalid_size());
        };
        accumulator_guard(env, number)?;
        if state.unlimited || !state.dec.is_known() {
            return Ok(quotient);
        }
        let scale = state.dec.s.max(6);
        let rounded = payload::round_number(number, scale, mem, env)?;
        let QValue::Number(number) = &*rounded else {
            return Err(invalid_size());
        };
        if !fits(number, Dec { p: 38, s: scale }) {
            return Err(accumulator_cant_fit());
        }
        Ok(rounded)
    }
}
impl AggState {
    fn add_paid<'a>(
        &mut self,
        value: Option<&QValue>,
        env: &ExecEnv<'a>,
        coll: ExecCollation<'_>,
        mem: &crate::ops::MemBudget<'a>,
        payload: &mut u64,
        temp: &mut u64,
    ) -> Result<(), ExecError> {
        use crate::model::{invalid_size, size_add};
        use crate::totals::payload::{self, PaidValue};
        let Some(value) = value else {
            self.count += 1;
            return Ok(());
        };
        if matches!(value, QValue::Null) {
            return Ok(());
        }
        match self.f {
            Func::Count => {
                self.count += 1;
                return Ok(());
            }
            Func::Sum | Func::Avg => {
                let QValue::Number(number) = value else {
                    return Err(ExecError::Fatal(
                        "num: СУММА/СРЕДНЕЕ over a value that is not a number".to_string(),
                    ));
                };
                accumulator_guard(env, number)?;
                let result = payload::add_numbers(&self.sum, number, mem, env)?;
                let QValue::Number(number) = &*result else {
                    return Err(invalid_size());
                };
                accumulator_guard(env, number)?;
                let bytes = result.bytes();
                let next = size_add(temp.checked_sub(*payload).ok_or_else(invalid_size)?, bytes)?;
                let (value, _) = result.into_parts();
                let QValue::Number(number) = value else {
                    drop(PaidValue::from_paid_parts(value, bytes, mem));
                    return Err(invalid_size());
                };
                let old = std::mem::replace(&mut self.sum, number);
                let old_bytes = std::mem::replace(payload, bytes);
                *temp = next;
                #[cfg(test)]
                admission_tests::snapshot(mem);
                drop(old);
                mem.shrink(old_bytes);
            }
            Func::Min | Func::Max => {
                let want = if self.f == Func::Min {
                    Ordering::Less
                } else {
                    Ordering::Greater
                };
                if let Some(current) = &self.rep
                    && try_cmp(value, current, coll)? != want
                {
                    self.count += 1;
                    return Ok(());
                }
                let result = PaidValue::copy_aggregate(value, mem, env)?;
                let bytes = result.bytes();
                let next = size_add(temp.checked_sub(*payload).ok_or_else(invalid_size)?, bytes)?;
                let (value, _) = result.into_parts();
                let old = self.rep.replace(value);
                let old_bytes = std::mem::replace(payload, bytes);
                *temp = next;
                #[cfg(test)]
                admission_tests::snapshot(mem);
                drop(old);
                mem.shrink(old_bytes);
            }
            _ => {}
        }
        self.count += 1;
        Ok(())
    }
}
