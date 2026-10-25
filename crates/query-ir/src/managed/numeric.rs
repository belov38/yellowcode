use super::{AResult, CompileArena, ScratchType};
use bsl_prim::{
    Number,
    number::{LimbAllocator, LimbBuffer, NumberOpError, NumberOutput},
};
use memory_budget::{BudgetError, BudgetedText, Category};
use std::mem::ManuallyDrop;

struct Factory<'a>(&'a CompileArena);
struct Buffer<'a> {
    values: ManuallyDrop<Vec<u32>>,
    arena: &'a CompileArena,
}
impl AsRef<[u32]> for Buffer<'_> {
    fn as_ref(&self) -> &[u32] {
        &self.values
    }
}
impl AsMut<[u32]> for Buffer<'_> {
    fn as_mut(&mut self) -> &mut [u32] {
        &mut self.values
    }
}
impl LimbBuffer for Buffer<'_> {
    fn truncate(&mut self, len: usize) {
        self.values.truncate(len);
    }
}
impl<'a> LimbAllocator for Factory<'a> {
    type Error = BudgetError;
    type Buffer = Buffer<'a>;
    fn allocate_zeroed(&self, len: usize) -> AResult<Self::Buffer> {
        let mut values = self.0.vector(len)?;
        values.resize(len, 0);
        Ok(Buffer {
            values: ManuallyDrop::new(values),
            arena: self.0,
        })
    }
}
impl Drop for Buffer<'_> {
    fn drop(&mut self) {
        let bytes = self.values.capacity() as u64 * std::mem::size_of::<u32>() as u64;
        unsafe { ManuallyDrop::drop(&mut self.values) };
        let _ = self
            .arena
            .compile(self.arena.charge.shrink(bytes).map_err(BudgetError::from));
    }
}
impl Buffer<'_> {
    fn transfer(self, parts: bsl_prim::number::NumberParts) -> Number {
        let mut owner = ManuallyDrop::new(self);
        parts.into_number(unsafe { ManuallyDrop::take(&mut owner.values) })
    }
}

pub(crate) struct ScratchNumber<'a> {
    value: ManuallyDrop<Number>,
    arena: Option<&'a CompileArena>,
}
impl<'a> ScratchNumber<'a> {
    fn new(arena: Option<&'a CompileArena>, value: Number) -> Self {
        Self {
            value: ManuallyDrop::new(value),
            arena,
        }
    }
    pub(super) fn parsed(
        arena: Option<&'a CompileArena>,
        text: &str,
    ) -> crate::sema::R<Option<Self>> {
        Ok(super::number(arena, text)?.map(|number| Self::new(arena, number)))
    }
    pub(super) fn scope_matches(&self, scope: Option<&CompileArena>) -> bool {
        match (self.arena, scope) {
            (Some(a), Some(b)) => std::ptr::eq(a, b),
            (None, None) => true,
            _ => false,
        }
    }
    pub(crate) fn into_number(self) -> Number {
        let mut owner = ManuallyDrop::new(self);
        unsafe { ManuallyDrop::take(&mut owner.value) }
    }
}
impl std::ops::Deref for ScratchNumber<'_> {
    type Target = Number;
    fn deref(&self) -> &Number {
        &self.value
    }
}
impl Drop for ScratchNumber<'_> {
    fn drop(&mut self) {
        let bytes = self.value.backing_capacity_limbs() as u64 * std::mem::size_of::<u32>() as u64;
        unsafe { ManuallyDrop::drop(&mut self.value) };
        if let Some(arena) = self.arena {
            let _ = arena.compile(arena.charge.shrink(bytes).map_err(BudgetError::from));
        }
    }
}

pub(crate) fn number_neg<'a>(
    arena: Option<&'a CompileArena>,
    number: &Number,
) -> crate::sema::R<ScratchNumber<'a>> {
    let Some(arena) = arena else {
        return Ok(ScratchNumber::new(None, number.neg()));
    };
    let output = number
        .try_neg_with(&Factory(arena))
        .map_err(|error| match error {
            NumberOpError::Buffer(error) => error,
            NumberOpError::Overflow | NumberOpError::DivByZero => BudgetError::Overflow,
        });
    let number = arena.compile(output).map(|output| match output {
        NumberOutput::Inline(number) => number,
        NumberOutput::Buffered { parts, limbs } => limbs.transfer(parts),
    })?;
    Ok(ScratchNumber::new(Some(arena), number))
}

fn allocation_error(error: bsl_prim::number::NumberAllocError<BudgetError>) -> BudgetError {
    match error {
        bsl_prim::number::NumberAllocError::Reserve(error) => error,
        bsl_prim::number::NumberAllocError::AllocationFailed => BudgetError::AllocationFailed,
        bsl_prim::number::NumberAllocError::Overflow => BudgetError::Overflow,
    }
}
fn reserve_number(arena: &CompileArena, reserved: &mut u64, limbs: usize) -> AResult<()> {
    let layout = std::alloc::Layout::array::<u32>(limbs).map_err(|_| BudgetError::Overflow)?;
    arena.reserve(layout)?;
    *reserved = layout.size() as u64;
    Ok(())
}
fn reconcile<T>(
    arena: &CompileArena,
    reserved: u64,
    result: AResult<T>,
    capacity: impl FnOnce(&T) -> usize,
) -> AResult<T> {
    let retained = match &result {
        Ok(value) => std::alloc::Layout::array::<u32>(capacity(value))
            .map_err(|_| BudgetError::Overflow)?
            .size() as u64,
        Err(_) => 0,
    };
    let released = reserved
        .checked_sub(retained)
        .ok_or(BudgetError::Overflow)?;
    arena.charge.shrink(released)?;
    result
}
pub(super) fn parse_number(arena: &CompileArena, text: &str) -> AResult<Option<Number>> {
    let mut reserved = 0;
    let result = Number::try_parse_with(text, |limbs| reserve_number(arena, &mut reserved, limbs))
        .map_err(allocation_error);
    reconcile(arena, reserved, result, |value| {
        value.as_ref().map_or(0, Number::backing_capacity_limbs)
    })
}
pub(super) fn clone_number(arena: &CompileArena, value: &Number) -> AResult<Number> {
    let mut reserved = 0;
    let result = value
        .try_clone_with(|limbs| reserve_number(arena, &mut reserved, limbs))
        .map_err(allocation_error);
    reconcile(arena, reserved, result, Number::backing_capacity_limbs)
}

#[allow(clippy::large_enum_variant)]
pub(crate) enum NumericPrefix {
    Value(crate::tir::TExpr),
    Runtime(crate::tir::TExpr, crate::tir::TExpr),
    TooLong(BudgetedText),
}

pub(crate) fn numeric_prefix(
    arena: &CompileArena,
    left: crate::tir::TExpr,
    right: crate::tir::TExpr,
    op: query_syntax::ast::BinOp,
    ty: crate::QType,
) -> crate::sema::R<NumericPrefix> {
    use crate::{
        QValue,
        managed_value::NumberBinaryOp,
        tir::{TExpr, TKind},
    };
    use query_syntax::ast::BinOp;
    let TExpr {
        kind: TKind::Const(QValue::Number(a)),
        ty: left_ty,
        span: left_span,
    } = left
    else {
        unreachable!("numeric prefix requires constant number operands")
    };
    let TExpr {
        kind: TKind::Const(QValue::Number(b)),
        ty: right_ty,
        span: right_span,
    } = right
    else {
        unreachable!("numeric prefix requires constant number operands")
    };
    let a = ScratchNumber::new(Some(arena), a);
    let b = ScratchNumber::new(Some(arena), b);
    let left_ty = ScratchType::new(Some(arena), left_ty);
    let right_ty = ScratchType::new(Some(arena), right_ty);
    let ty = ScratchType::new(Some(arena), ty);
    let operation = match op {
        BinOp::Add => NumberBinaryOp::Add,
        BinOp::Sub => NumberBinaryOp::Sub,
        BinOp::Mul => NumberBinaryOp::Mul,
        BinOp::Div => NumberBinaryOp::Div,
        BinOp::And | BinOp::Or => unreachable!("numeric prefix requires arithmetic operator"),
    };
    let output = match operation.evaluate(&a, &b, &Factory(arena)) {
        Ok(output) => output,
        Err(NumberOpError::DivByZero) => {
            return Ok(NumericPrefix::Runtime(
                TExpr {
                    kind: TKind::Const(QValue::Number(a.into_number())),
                    ty: left_ty.into_type(),
                    span: left_span,
                },
                TExpr {
                    kind: TKind::Const(QValue::Number(b.into_number())),
                    ty: right_ty.into_type(),
                    span: right_span,
                },
            ));
        }
        Err(error) => {
            return arena.compile(Err(match error {
                NumberOpError::Buffer(error) => error,
                NumberOpError::Overflow | NumberOpError::DivByZero => BudgetError::Overflow,
            }));
        }
    };
    let number = match output {
        NumberOutput::Inline(number) => number,
        NumberOutput::Buffered { parts, limbs } => limbs.transfer(parts),
    };
    let number = ScratchNumber::new(Some(arena), number);
    let digits = arena.compile(canonical_count(&number))?;
    if digits > 38 {
        return Ok(NumericPrefix::TooLong(
            arena.compile(canonical_text(arena, &number, digits))?,
        ));
    }
    Ok(NumericPrefix::Value(TExpr {
        kind: TKind::Const(QValue::Number(number.into_number())),
        ty: ty.into_type(),
        span: left_span.to(right_span),
    }))
}

struct Canonical<'a> {
    output: Option<&'a mut BudgetedText>,
    digits: usize,
    fraction: bool,
    integer_started: bool,
}
impl std::fmt::Write for Canonical<'_> {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        for byte in text.bytes() {
            match byte {
                b'-' => continue,
                b'.' => {
                    self.fraction = true;
                    continue;
                }
                b'0' if !self.fraction && !self.integer_started => continue,
                _ => self.integer_started = true,
            }
            self.digits = self.digits.checked_add(1).ok_or(std::fmt::Error)?;
            if let Some(output) = self.output.as_mut() {
                output
                    .try_push(char::from(byte))
                    .map_err(|_| std::fmt::Error)?;
            }
        }
        Ok(())
    }
}
fn canonical_count(number: &Number) -> AResult<usize> {
    let mut writer = Canonical {
        output: None,
        digits: 0,
        fraction: false,
        integer_started: false,
    };
    number
        .write_xml(&mut writer)
        .map_err(|_| BudgetError::Overflow)?;
    Ok(writer.digits)
}
fn canonical_text(arena: &CompileArena, number: &Number, digits: usize) -> AResult<BudgetedText> {
    let mut text =
        BudgetedText::try_with_capacity(&arena.budget, Category::Execution, u64::MAX, digits)?;
    let mut writer = Canonical {
        output: Some(&mut text),
        digits: 0,
        fraction: false,
        integer_started: false,
    };
    if number.write_xml(&mut writer).is_err() {
        return Err(text.error().copied().unwrap_or(BudgetError::Overflow));
    }
    Ok(text)
}

pub(crate) enum OwnedNumericPrefix<'a> {
    Value(super::ExprValue<'a>),
    Runtime(super::ExprValue<'a>, super::ExprValue<'a>),
    TooLong(BudgetedText),
}
pub(crate) fn numeric_prefix_owned<'a>(
    arena: &'a CompileArena,
    left: super::ExprValue<'a>,
    right: super::ExprValue<'a>,
    op: query_syntax::ast::BinOp,
    ty: ScratchType<'a>,
) -> crate::sema::R<OwnedNumericPrefix<'a>> {
    fn leaf<'a>(
        arena: &'a CompileArena,
        value: crate::tir::TExpr,
    ) -> crate::sema::R<super::ExprValue<'a>> {
        let crate::tir::TExpr {
            kind: crate::tir::TKind::Const(crate::QValue::Number(number)),
            ty,
            span,
        } = value
        else {
            unreachable!("numeric producer returns only number leaves")
        };
        let number = ScratchNumber::new(Some(arena), number);
        let ty = ScratchType::new(Some(arena), ty);
        super::ExprValue::compose(Some(arena), super::ScalarKind::Number(number), ty, span)
    }
    if !left.scope_matches(Some(arena))
        || !right.scope_matches(Some(arena))
        || !ty.scope_matches(Some(arena))
    {
        return super::allocation_error(Some(arena), BudgetError::Overflow);
    }
    match numeric_prefix(
        arena,
        left.into_expr(),
        right.into_expr(),
        op,
        ty.into_type(),
    )? {
        NumericPrefix::Value(value) => leaf(arena, value).map(OwnedNumericPrefix::Value),
        NumericPrefix::Runtime(a, b) => {
            let a = leaf(arena, a)?;
            let b = leaf(arena, b)?;
            Ok(OwnedNumericPrefix::Runtime(a, b))
        }
        NumericPrefix::TooLong(text) => Ok(OwnedNumericPrefix::TooLong(text)),
    }
}
impl ScratchNumber<'_> {
    pub(crate) fn legacy(value: Number) -> Self {
        Self::new(None, value)
    }
}
