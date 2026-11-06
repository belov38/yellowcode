use super::{ArithOp, ExecEnv, LedgerWork, MemBudget};
use bsl_prim::Number;
use bsl_prim::number::{LimbAllocator, LimbBuffer, NumberOpError, NumberOutput};
use query_ir::{ExecError, QValue};
use std::alloc::Layout;
use std::cell::Cell;
use std::mem::{size_of, take};
use std::ops::{Deref, DerefMut};

use crate::model::{invalid_size, size_add, size_block, size_vec, totals_payload_model};

pub(super) fn checkpoint(env: Option<&ExecEnv<'_>>) -> Result<(), ExecError> {
    if let Some(env) = env {
        LedgerWork::new(env).check()?;
    }
    Ok(())
}

struct Admission<'m, 'a> {
    mem: &'m MemBudget<'a>,
    bytes: u64,
}

impl<'m, 'a> Admission<'m, 'a> {
    fn new(mem: &'m MemBudget<'a>, bytes: u64) -> Result<Self, ExecError> {
        mem.grow(bytes)?;
        #[cfg(test)]
        refusal.accepted();
        #[cfg(test)]
        crate::ops::agg::product_witnesses::admitted(mem, bytes);
        Ok(Self { mem, bytes })
    }

    fn transfer(mut self) -> u64 {
        std::mem::replace(&mut self.bytes, 0)
    }
}

impl Drop for Admission<'_, '_> {
    fn drop(&mut self) {
        self.mem.shrink(self.bytes);
        #[cfg(test)]
        crate::ops::agg::product_witnesses::snapshot(self.mem);
    }
}

pub(crate) struct PaidVec<'m, 'a, T> {
    values: Vec<T>,
    mem: &'m MemBudget<'a>,
    bytes: u64,
}

impl<'m, 'a, T> PaidVec<'m, 'a, T> {
    pub(crate) fn new(
        capacity: usize,
        mem: &'m MemBudget<'a>,
        env: Option<&ExecEnv<'_>>,
    ) -> Result<Self, ExecError> {
        Layout::array::<T>(capacity).map_err(|_| invalid_size())?;
        let bytes = size_vec(capacity, size_of::<T>())?;
        checkpoint(env)?;
        let admission = Admission::new(mem, bytes)?;
        #[cfg(test)]
        super::accounting_invariants::payload_checkpoint("before_buffer");
        #[cfg(test)]
        crate::ops::agg::product_witnesses::checkpoint(
            crate::ops::agg::product_witnesses::Point::BeforeAlloc,
            mem,
            env.map(|env| env.cancel),
            0,
            capacity,
            0,
        )?;
        checkpoint(env)?;
        #[cfg(test)]
        crate::ops::hier::payload_snapshot(mem);
        #[cfg(test)]
        crate::ops::agg::payload_admission_tests::snapshot(mem);
        #[cfg(test)]
        crate::ops::agg::product_witnesses::allocation_target(
            Layout::array::<T>(capacity).map_err(|_| invalid_size())?,
        );
        let values = Vec::with_capacity(capacity);
        #[cfg(test)]
        crate::ops::agg::product_witnesses::checkpoint(
            crate::ops::agg::product_witnesses::Point::Built,
            mem,
            env.map(|env| env.cancel),
            values.as_ptr() as usize,
            values.capacity(),
            0,
        )?;
        #[cfg(test)]
        crate::ops::agg::product_witnesses::built(mem, values.as_ptr() as usize, values.capacity());
        Ok(Self {
            values,
            mem,
            bytes: admission.transfer(),
        })
    }

    pub(crate) fn push(&mut self, value: T) -> Result<(), ExecError> {
        if self.values.len() == self.values.capacity() {
            return Err(invalid_size());
        }
        self.values.push(value);
        Ok(())
    }

    pub(crate) fn into_parts(mut self) -> (Vec<T>, u64) {
        let bytes = std::mem::replace(&mut self.bytes, 0);
        (take(&mut self.values), bytes)
    }
}

impl<T> Deref for PaidVec<'_, '_, T> {
    type Target = Vec<T>;
    fn deref(&self) -> &Self::Target {
        &self.values
    }
}

impl<T> DerefMut for PaidVec<'_, '_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.values
    }
}

impl<T> Drop for PaidVec<'_, '_, T> {
    fn drop(&mut self) {
        #[cfg(test)]
        crate::ops::hier::payload_snapshot(self.mem);
        #[cfg(test)]
        crate::ops::agg::payload_admission_tests::snapshot(self.mem);
        #[cfg(test)]
        crate::ops::agg::product_witnesses::snapshot(self.mem);
        drop(take(&mut self.values));
        self.mem.shrink(self.bytes);
        #[cfg(test)]
        crate::ops::agg::product_witnesses::snapshot(self.mem);
        #[cfg(test)]
        crate::ops::hier::payload_snapshot(self.mem);
        #[cfg(test)]
        crate::ops::agg::payload_admission_tests::snapshot(self.mem);
    }
}

pub(crate) struct PaidValue<'m, 'a> {
    value: QValue,
    mem: &'m MemBudget<'a>,
    bytes: u64,
}

impl<'m, 'a> PaidValue<'m, 'a> {
    pub(super) fn null(mem: &'m MemBudget<'a>) -> Self {
        Self {
            value: QValue::Null,
            mem,
            bytes: 0,
        }
    }

    pub(crate) fn copy(
        value: &QValue,
        mem: &'m MemBudget<'a>,
        env: Option<&ExecEnv<'_>>,
    ) -> Result<Self, ExecError> {
        let retained = totals_payload_model(value, mem, env)?;
        Self::copy_validated(value, retained, mem, env)
    }

    fn copy_validated(
        value: &QValue,
        retained: u64,
        mem: &'m MemBudget<'a>,
        env: Option<&ExecEnv<'_>>,
    ) -> Result<Self, ExecError> {
        checkpoint(env)?;
        match value {
            QValue::Number(number) => {
                let mut admission = Admission { mem, bytes: 0 };
                let number = number
                    .try_clone_with(|len| {
                        let bytes = size_vec(len, size_of::<u32>())?;
                        mem.check_value(bytes)?;
                        checkpoint(env)?;
                        mem.grow(bytes)?;
                        admission.bytes = bytes;
                        #[cfg(test)]
                        crate::ops::agg::product_witnesses::admitted(mem, bytes);
                        checkpoint(env)?;
                        #[cfg(test)]
                        crate::ops::hier::payload_snapshot(mem);
                        #[cfg(test)]
                        crate::ops::agg::payload_admission_tests::snapshot(mem);
                        Ok(())
                    })
                    .map_err(|error| match error {
                        bsl_prim::number::NumberAllocError::Reserve(error) => error,
                        bsl_prim::number::NumberAllocError::Overflow => invalid_size(),
                        bsl_prim::number::NumberAllocError::AllocationFailed => {
                            super::fatal("number allocation failed")
                        }
                    })?;
                let result = Self {
                    value: QValue::Number(number),
                    mem,
                    bytes: admission.transfer(),
                };
                checkpoint(env)?;
                Ok(result)
            }
            QValue::Type(ty) => {
                let mut refs = PaidVec::new(ty.refs.len(), mem, env)?;
                for atom in &ty.refs {
                    refs.push(*atom)?;
                }
                #[cfg(test)]
                crate::ops::agg::product_witnesses::phase(agg_role, mem);
                let mut others = PaidVec::new(ty.others.len(), mem, env)?;
                for atom in &ty.others {
                    others.push(*atom)?;
                }
                let bytes = size_add(refs.bytes, others.bytes)?;
                mem.check_value(bytes)?;
                checkpoint(env)?;
                let (refs, _) = refs.into_parts();
                let (others, _) = others.into_parts();
                Ok(Self {
                    value: QValue::Type(query_ir::QType {
                        null: ty.null,
                        boolean: ty.boolean,
                        number: ty.number,
                        string: ty.string,
                        date: ty.date,
                        refs,
                        others,
                    }),
                    mem,
                    bytes,
                })
            }
            QValue::PointInTime(moment) => {
                let bytes = size_block(size_of::<query_ir::value::Moment>() as u64)?;
                mem.check_value(bytes)?;
                let admission = Admission::new(mem, bytes)?;
                checkpoint(env)?;
                let value = QValue::PointInTime(Box::new((**moment).clone()));
                Ok(Self {
                    value,
                    mem,
                    bytes: admission.transfer(),
                })
            }
            QValue::Boundary(boundary) => {
                let bytes = size_block(size_of::<query_ir::value::Boundary>() as u64)?;
                let admission = Admission::new(mem, bytes)?;
                let point = Self::copy(&boundary.point, mem, env)?;
                let bytes = size_add(bytes, point.bytes)?;
                mem.check_value(bytes)?;
                checkpoint(env)?;
                let (point, _) = point.into_parts();
                #[cfg(test)]
                crate::ops::agg::product_witnesses::restore_role(agg_role, mem);
                let value = QValue::Boundary(Box::new(query_ir::value::Boundary {
                    point,
                    include: boundary.include,
                }));
                let _ = admission.transfer();
                Ok(Self { value, mem, bytes })
            }
            QValue::String(_) | QValue::Binary(_) | QValue::Storage(_) | QValue::Nested(_) => {
                mem.check_value(retained)?;
                let admission = Admission::new(mem, retained)?;
                checkpoint(env)?;
                #[cfg(test)]
                crate::ops::agg::product_witnesses::event(
                    crate::ops::agg::product_witnesses::Point::BeforeAlloc,
                    mem,
                    0,
                    0,
                    0,
                );
                let value = value.clone();
                Ok(Self {
                    value,
                    mem,
                    bytes: admission.transfer(),
                })
            }
            _ => Ok(Self {
                value: value.clone(),
                mem,
                bytes: 0,
            }),
        }
    }

    pub(crate) fn bytes(&self) -> u64 {
        self.bytes
    }

    pub(super) fn is_owned_by(&self, mem: &MemBudget<'_>) -> bool {
        std::ptr::from_ref(self.mem).cast::<()>() == std::ptr::from_ref(mem).cast::<()>()
    }

    pub(crate) fn into_parts(mut self) -> (QValue, u64) {
        let bytes = std::mem::replace(&mut self.bytes, 0);
        (std::mem::replace(&mut self.value, QValue::Null), bytes)
    }

    pub(super) fn into_unbudgeted(self) -> QValue {
        let mem = self.mem;
        let (value, bytes) = self.into_parts();
        mem.shrink(bytes);
        value
    }
}

impl Deref for PaidValue<'_, '_> {
    type Target = QValue;
    fn deref(&self) -> &QValue {
        &self.value
    }
}

impl Drop for PaidValue<'_, '_> {
    fn drop(&mut self) {
        #[cfg(test)]
        crate::ops::hier::payload_snapshot(self.mem);
        #[cfg(test)]
        crate::ops::agg::payload_admission_tests::snapshot(self.mem);
        drop(std::mem::replace(&mut self.value, QValue::Null));
        self.mem.shrink(self.bytes);
        #[cfg(test)]
        crate::ops::hier::payload_snapshot(self.mem);
        #[cfg(test)]
        crate::ops::agg::payload_admission_tests::snapshot(self.mem);
    }
}

struct PaidLimbs<'m, 'a>(PaidVec<'m, 'a, u32>);
impl AsRef<[u32]> for PaidLimbs<'_, '_> {
    fn as_ref(&self) -> &[u32] {
        &self.0
    }
}
impl AsMut<[u32]> for PaidLimbs<'_, '_> {
    fn as_mut(&mut self) -> &mut [u32] {
        &mut self.0
    }
}
impl LimbBuffer for PaidLimbs<'_, '_> {
    fn truncate(&mut self, len: usize) {
        self.0.values.truncate(len);
    }
}

struct Numbers<'m, 'a, 'e> {
    mem: &'m MemBudget<'a>,
    env: Option<&'e ExecEnv<'a>>,
}
impl<'m, 'a> LimbAllocator for Numbers<'m, 'a, '_> {
    type Error = ExecError;
    type Buffer = PaidLimbs<'m, 'a>;
    fn allocate_zeroed(&self, len: usize) -> Result<<Self as LimbAllocator>::Buffer, ExecError> {
        let mut values = PaidVec::new(len, self.mem, self.env)?;
        values.values.resize(len, 0);
        Ok(PaidLimbs(values))
    }
}

fn number_result<'m, 'a>(
    output: NumberOutput<PaidLimbs<'m, 'a>>,
    mem: &'m MemBudget<'a>,
) -> PaidValue<'m, 'a> {
    match output {
        NumberOutput::Inline(number) => PaidValue {
            value: QValue::Number(number),
            mem,
            bytes: 0,
        },
        NumberOutput::Buffered { parts, limbs } => {
            let (buffer, bytes) = limbs.0.into_parts();
            PaidValue {
                value: QValue::Number(parts.into_number(buffer)),
                mem,
                bytes,
            }
        }
    }
}

fn number_error(error: NumberOpError<ExecError>) -> ExecError {
    match error {
        NumberOpError::Buffer(error) => error,
        NumberOpError::Overflow => invalid_size(),
        NumberOpError::DivByZero => super::fatal("unexpected zero divisor"),
    }
}

pub(super) fn count<'m, 'a>(
    mut count: u64,
    mem: &'m MemBudget<'a>,
    env: Option<&ExecEnv<'a>>,
) -> Result<PaidValue<'m, 'a>, ExecError> {
    let mut digits = [0u8; 20];
    let mut start = digits.len();
    loop {
        start -= 1;
        *digits.get_mut(start).ok_or_else(invalid_size)? = (count % 10) as u8;
        count /= 10;
        if count == 0 {
            break;
        }
    }
    let output = Number::try_from_decimal_digits_with(
        false,
        digits.get(start..).ok_or_else(invalid_size)?,
        0,
        &Numbers { mem, env },
    )
    .map_err(number_error)?
    .ok_or_else(invalid_size)?;
    Ok(number_result(output, mem))
}

pub(super) fn neg<'m, 'a>(
    value: &QValue,
    mem: &'m MemBudget<'a>,
    env: Option<&ExecEnv<'a>>,
) -> Result<PaidValue<'m, 'a>, ExecError> {
    match value {
        QValue::Null => Ok(PaidValue::null(mem)),
        QValue::Number(number) => {
            let output = number
                .try_neg_with(&Numbers { mem, env })
                .map_err(number_error)?;
            let result = number_result(output, mem);
            checkpoint(env)?;
            Ok(result)
        }
        _ => Err(super::later("ИТОГИ: унарный минус не числа")),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "The arithmetic inputs, old language guards and allocation context remain explicit."
)]
pub(super) fn binary<'m, 'a>(
    op: ArithOp,
    left: &QValue,
    right: &QValue,
    limbs: u64,
    div_zero: &Cell<bool>,
    mem: &'m MemBudget<'a>,
    env: Option<&ExecEnv<'a>>,
) -> Result<PaidValue<'m, 'a>, ExecError> {
    match (left, right) {
        (QValue::Null, _) | (_, QValue::Null) => Ok(PaidValue::null(mem)),
        (QValue::Number(left), QValue::Number(right)) => {
            let alloc = Numbers { mem, env };
            let output = match op {
                ArithOp::Add => left.try_add_with(right, &alloc),
                ArithOp::Sub => left.try_sub_with(right, &alloc),
                ArithOp::Mul => left.try_mul_with(right, &alloc),
                ArithOp::Div => left.try_div_with(right, &alloc),
            };
            let output = match output {
                Ok(output) => output,
                Err(NumberOpError::DivByZero) => {
                    div_zero.set(true);
                    return Ok(PaidValue::null(mem));
                }
                Err(error) => return Err(number_error(error)),
            };
            let result = number_result(output, mem);
            checkpoint(env)?;
            if let QValue::Number(number) = &*result
                && number.size_hint_limbs() > limbs
            {
                return Err(ExecError::Fatal("превышен лимит размера числа".to_string()));
            }
            Ok(result)
        }
        (QValue::String(left), QValue::String(right)) if op == ArithOp::Add => {
            let len = left
                .0
                .len()
                .checked_add(right.0.len())
                .ok_or_else(invalid_size)?;
            let header =
                Layout::array::<std::sync::atomic::AtomicUsize>(2).map_err(|_| invalid_size())?;
            let tail = Layout::array::<u16>(len).map_err(|_| invalid_size())?;
            let layout = header
                .extend(tail)
                .map_err(|_| invalid_size())?
                .0
                .pad_to_align();
            let arc_bytes = size_block(layout.size() as u64)?;
            mem.check_value(arc_bytes)?;
            let arc_admission = Admission::new(mem, arc_bytes)?;
            let mut units = PaidVec::new(len, mem, env)?;
            units.values.extend_from_slice(&left.0);
            units.values.extend_from_slice(&right.0);
            #[cfg(test)]
            super::accounting_invariants::payload_checkpoint("before_arc");
            checkpoint(env)?;
            let (units, bytes) = units.into_parts();
            let value = QValue::String(query_ir::QStr(units.into()));
            mem.shrink(bytes);
            let result = PaidValue {
                value,
                mem,
                bytes: arc_admission.transfer(),
            };
            checkpoint(env)?;
            Ok(result)
        }
        _ => Err(super::later("ИТОГИ: операция над нечисловыми значениями")),
    }
}

pub(super) struct PaidContext<'m, 'a> {
    values: PaidVec<'m, 'a, (usize, QValue)>,
    payload: u64,
}

impl Deref for PaidContext<'_, '_> {
    type Target = [(usize, QValue)];
    fn deref(&self) -> &Self::Target {
        &self.values
    }
}

impl Drop for PaidContext<'_, '_> {
    fn drop(&mut self) {
        self.values.values.clear();
        self.values.mem.shrink(self.payload);
    }
}

pub(super) fn context<'m, 'a>(
    ctx: &[(usize, QValue)],
    col: usize,
    value: &QValue,
    mem: &'m MemBudget<'a>,
    env: Option<&ExecEnv<'a>>,
) -> Result<PaidContext<'m, 'a>, ExecError> {
    let capacity = ctx.len().checked_add(1).ok_or_else(invalid_size)?;
    let mut result = PaidContext {
        values: PaidVec::new(capacity, mem, env)?,
        payload: 0,
    };
    for (column, value) in ctx
        .iter()
        .map(|(col, value)| (*col, value))
        .chain(std::iter::once((col, value)))
    {
        let value = PaidValue::copy(value, mem, env)?;
        result.payload = size_add(result.payload, value.bytes)?;
        let (value, _) = value.into_parts();
        result.values.push((column, value))?;
    }
    Ok(result)
}
