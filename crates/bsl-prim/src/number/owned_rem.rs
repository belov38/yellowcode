use super::{
    LimbAllocator, LimbBuffer, NumberOpError, NumberOutput, add_decimals, multiply_decimals,
    normalize, with_native,
};
use crate::Number;

fn truncate_output<A: LimbAllocator>(
    value: NumberOutput<A::Buffer>,
    alloc: &A,
) -> Result<NumberOutput<A::Buffer>, NumberOpError<A::Error>> {
    match value {
        NumberOutput::Inline(value) => value.try_trunc_with(alloc),
        NumberOutput::Buffered { parts, mut limbs } => {
            let len = limbs.as_ref().len();
            let skip = (parts.scale as usize).min(len);
            limbs.as_mut().copy_within(skip.., 0);
            limbs.truncate(len - skip);
            Ok(normalize(parts.neg, 0, limbs))
        }
    }
}

impl Number {
    pub fn try_rem_with<A: LimbAllocator>(
        &self,
        other: &Number,
        alloc: &A,
    ) -> Result<NumberOutput<A::Buffer>, NumberOpError<A::Error>> {
        let quotient = truncate_output(self.try_div_with(other, alloc)?, alloc)?;
        let product =
            quotient.with_decimal(|q| with_native(other, |b| multiply_decimals(b, q, alloc)))?;
        drop(quotient);
        let remainder = product.with_decimal(|mut p| {
            p.neg = !p.neg;
            with_native(self, |a| add_decimals(a, p, alloc))
        })?;
        drop(product);
        Ok(remainder)
    }
}
