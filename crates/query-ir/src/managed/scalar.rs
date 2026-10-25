use super::{CompileArena, NativeOwned, OwnedExpr, OwnedString, ScratchType};
use crate::{
    QStr, QValue,
    sema::R,
    tir::{self, TExpr, TKind},
};
use memory_budget::BudgetError;
use query_syntax::{
    Func, Kw, Span,
    ast::{BinOp, CmpOp},
};
use std::mem::ManuallyDrop;

pub(crate) struct ExprValue<'a>(OwnedExpr<'a>);
impl<'a> ExprValue<'a> {
    pub(crate) fn owned(value: OwnedExpr<'a>) -> Self {
        Self(value)
    }
    pub(crate) fn into_native_owned(self) -> R<NativeOwned<'a, TExpr>> {
        Ok(self.0.into_native_owned())
    }
    pub(crate) fn span(mut self, span: Span) -> Self {
        self.0.value.span = span;
        self
    }
    pub(crate) fn into_expr(self) -> TExpr {
        self.0.into_expr()
    }
    pub(crate) fn into_owned(self) -> OwnedExpr<'a> {
        self.0
    }
    fn bytes(&self) -> Option<u64> {
        Some(self.0.bytes)
    }
    pub(super) fn scope_matches(&self, scope: Option<&CompileArena>) -> bool {
        same_scope(scope, self.0.scope)
    }
    pub(crate) fn copied(value: NativeOwned<'a, TExpr>) -> Self {
        let (value, scope, bytes, depth) = value.into_expr_parts();
        Self(OwnedExpr {
            value: ManuallyDrop::new(value),
            scope,
            bytes,
            depth,
        })
    }
    pub(crate) fn replace_type(
        self,
        scope: Option<&'a CompileArena>,
        ty: ScratchType<'a>,
        span: Span,
    ) -> R<Self> {
        if !self.scope_matches(scope) || !ty.scope_matches(scope) {
            return super::allocation_error(scope, BudgetError::Overflow);
        }
        if scope.is_none() {
            let bytes = self.bytes();
            let depth = self.0.depth;
            let mut value = self.into_expr();
            value.ty = ty.into_type();
            value.span = span;
            return Ok(Self::finished(value, scope, bytes.unwrap_or(0), depth));
        }
        let old = super::allocation_result(scope, super::type_backing(&self.ty))?;
        let next = super::allocation_result(scope, super::type_backing(&ty))?;
        let bytes = match self.bytes() {
            Some(bytes) => Some(sum(
                scope,
                super::allocation_result(
                    scope,
                    bytes.checked_sub(old).ok_or(BudgetError::Overflow),
                )?,
                next,
            )?),
            None => None,
        };
        let depth = self.0.depth;
        let mut value = self.into_expr();
        drop(ScratchType::new(
            scope,
            std::mem::replace(&mut value.ty, ty.into_type()),
        ));
        value.span = span;
        Ok(Self::finished(
            value,
            scope,
            super::allocation_result(scope, bytes.ok_or(BudgetError::Overflow))?,
            depth,
        ))
    }
    fn finished(value: TExpr, scope: Option<&'a CompileArena>, bytes: u64, depth: u16) -> Self {
        Self(OwnedExpr {
            value: ManuallyDrop::new(value),
            scope,
            bytes,
            depth,
        })
    }
    pub(crate) fn boxed(self, scope: Option<&'a CompileArena>) -> R<ExprBox<'a>> {
        if !self.scope_matches(scope) {
            return super::allocation_error(scope, BudgetError::Overflow);
        }
        let bytes = plus(
            scope,
            self.bytes(),
            Some(std::mem::size_of::<TExpr>() as u64),
        )?;
        let depth = self.0.depth;
        super::query::checked_depth(scope, depth.saturating_add(1))?;
        let mut block = super::vector(scope, 1)?;
        block.push(self.into_expr());
        let value = unsafe { Box::from_raw(Box::into_raw(block.into_boxed_slice()) as *mut TExpr) };
        Ok(Part {
            value: ManuallyDrop::new(value),
            scope,
            bytes,
            depth,
        })
    }
    pub(crate) fn extend_field(
        self,
        scope: Option<&'a CompileArena>,
        derefs: OwnedDerefs<'a>,
        ty: ScratchType<'a>,
        span: Span,
    ) -> R<Self> {
        if !self.scope_matches(scope)
            || !ty.scope_matches(scope)
            || !same_scope(scope, derefs.scope)
        {
            return super::allocation_error(scope, BudgetError::Overflow);
        }
        if let TKind::Field {
            level,
            source,
            col,
            derefs: old,
        } = &self.kind
        {
            let (level, source, col) = (*level, *source, *col);
            let mut next = OwnedDerefs::new(
                scope,
                super::allocation_result(
                    scope,
                    old.len()
                        .checked_add(derefs.len())
                        .ok_or(BudgetError::Overflow),
                )?,
            )?;
            for step in old {
                next.push(&step.name, &step.targets)?;
            }
            next.append(derefs)?;
            let result = Self::compose(
                scope,
                ScalarKind::Field {
                    level,
                    source,
                    col,
                    derefs: next,
                },
                ty,
                span,
            )?;
            drop(self);
            Ok(result)
        } else {
            drop(derefs);
            self.replace_type(scope, ty, span)
        }
    }
    pub(crate) fn compose(
        scope: Option<&'a CompileArena>,
        kind: ScalarKind<'a>,
        ty: ScratchType<'a>,
        span: Span,
    ) -> R<Self> {
        if !ty.scope_matches(scope) || !kind.scope_matches(scope) {
            return super::allocation_error(scope, BudgetError::Overflow);
        }
        let depth = super::query::checked_depth(scope, kind.depth().saturating_add(1))?;
        let type_bytes = super::allocation_result(scope, super::type_backing(&ty))?;
        let bytes = plus(scope, kind.bytes(scope)?, Some(type_bytes))?;
        let value = TExpr {
            kind: kind.finish(),
            ty: ty.into_type(),
            span,
        };
        Ok(Self::finished(
            value,
            scope,
            super::allocation_result(scope, bytes.ok_or(BudgetError::Overflow))?,
            depth,
        ))
    }
}
impl std::ops::Deref for ExprValue<'_> {
    type Target = TExpr;
    fn deref(&self) -> &TExpr {
        &self.0
    }
}
fn same_scope(a: Option<&CompileArena>, b: Option<&CompileArena>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => std::ptr::eq(a, b),
        (None, None) => true,
        _ => false,
    }
}
fn sum(scope: Option<&CompileArena>, a: u64, b: u64) -> R<u64> {
    super::allocation_result(scope, a.checked_add(b).ok_or(BudgetError::Overflow))
}
fn plus(scope: Option<&CompileArena>, a: Option<u64>, b: Option<u64>) -> R<Option<u64>> {
    match (a, b) {
        (Some(a), Some(b)) => sum(scope, a, b).map(Some),
        _ => Ok(None),
    }
}

pub(crate) trait Destroy {
    fn destroy(self);
}
impl Destroy for TExpr {
    fn destroy(self) {
        super::literal::destroy_expr(self);
    }
}
impl Destroy for Box<TExpr> {
    fn destroy(self) {
        (*self).destroy();
    }
}
impl Destroy for (TExpr, TExpr) {
    fn destroy(self) {
        self.0.destroy();
        self.1.destroy();
    }
}
impl Destroy for tir::ArithStep {
    fn destroy(self) {
        self.rhs.destroy();
        drop(self.ty);
    }
}
impl Destroy for tir::Deref {
    fn destroy(self) {
        drop(self);
    }
}
impl<T: Destroy> Destroy for Vec<T> {
    fn destroy(mut self) {
        while let Some(value) = self.pop() {
            value.destroy();
        }
    }
}

pub(crate) struct Part<'a, T: Destroy> {
    value: ManuallyDrop<T>,
    scope: Option<&'a CompileArena>,
    bytes: Option<u64>,
    depth: u16,
}
impl<T: Destroy> Part<'_, T> {
    fn finish(self) -> T {
        let mut this = ManuallyDrop::new(self);
        unsafe { ManuallyDrop::take(&mut this.value) }
    }
}
impl<T: Destroy> Drop for Part<'_, T> {
    fn drop(&mut self) {
        unsafe { ManuallyDrop::take(&mut self.value) }.destroy();
        if let (Some(scope), Some(bytes)) = (self.scope, self.bytes) {
            scope.release_owned(bytes);
        }
    }
}
impl<T: Destroy> std::ops::Deref for Part<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}
pub(crate) type ExprBox<'a> = Part<'a, Box<TExpr>>;
pub(crate) type ExprList<'a> = Part<'a, Vec<TExpr>>;
pub(crate) type WhenList<'a> = Part<'a, Vec<(TExpr, TExpr)>>;
pub(crate) type OwnedSteps<'a> = Part<'a, Vec<tir::ArithStep>>;
pub(crate) type OwnedDerefs<'a> = Part<'a, Vec<tir::Deref>>;
impl<'a, T: Destroy> Part<'a, Vec<T>> {
    pub(crate) fn new(scope: Option<&'a CompileArena>, capacity: usize) -> R<Self> {
        let value = super::vector(scope, capacity)?;
        let bytes =
            std::alloc::Layout::array::<T>(value.capacity()).map_err(|_| BudgetError::Overflow);
        let bytes = super::allocation_result(scope, bytes)?.size() as u64;
        Ok(Self {
            value: ManuallyDrop::new(value),
            scope,
            bytes: Some(bytes),
            depth: 0,
        })
    }
    fn admit(&mut self, payload: Option<u64>) -> R<()> {
        self.admit_count(payload, 1)
    }
    fn admit_count(&mut self, payload: Option<u64>, count: usize) -> R<()> {
        let old = (self.value.capacity() * std::mem::size_of::<T>()) as u64;
        let mut bytes = plus(self.scope, self.bytes, payload)?;
        if self.scope.is_some() {
            let required = super::allocation_result(
                self.scope,
                self.value
                    .len()
                    .checked_add(count)
                    .ok_or(BudgetError::Overflow),
            )?;
            let capacity = if required > self.value.capacity() {
                super::allocation_result(
                    self.scope,
                    self.value
                        .capacity()
                        .checked_mul(2)
                        .ok_or(BudgetError::Overflow),
                )?
                .max(required)
            } else {
                self.value.capacity()
            };
            let layout = super::allocation_result(
                self.scope,
                std::alloc::Layout::array::<T>(capacity).map_err(|_| BudgetError::Overflow),
            )?;
            bytes = match bytes {
                Some(n) => Some(sum(
                    self.scope,
                    super::allocation_result(
                        self.scope,
                        n.checked_sub(old).ok_or(BudgetError::Overflow),
                    )?,
                    layout.size() as u64,
                )?),
                None => None,
            };
        }
        super::reserve_additional(self.scope, &mut self.value, count)?;
        self.bytes = bytes;
        Ok(())
    }
}
impl<'a> ExprList<'a> {
    pub(crate) fn push(&mut self, value: ExprValue<'a>) -> R<()> {
        if !value.scope_matches(self.scope) {
            return super::allocation_error(self.scope, BudgetError::Overflow);
        }
        super::query::checked_depth(self.scope, value.0.depth.saturating_add(1))?;
        self.admit(value.bytes())?;
        self.depth = self.depth.max(value.0.depth);
        self.value.push(value.into_expr());
        Ok(())
    }
}
impl<'a> WhenList<'a> {
    pub(crate) fn push(&mut self, a: ExprValue<'a>, b: ExprValue<'a>) -> R<()> {
        if !a.scope_matches(self.scope) || !b.scope_matches(self.scope) {
            return super::allocation_error(self.scope, BudgetError::Overflow);
        }
        super::query::checked_depth(self.scope, a.0.depth.max(b.0.depth).saturating_add(1))?;
        self.admit(plus(self.scope, a.bytes(), b.bytes())?)?;
        self.depth = self.depth.max(a.0.depth).max(b.0.depth);
        self.value.push((a.into_expr(), b.into_expr()));
        Ok(())
    }
}
impl<'a> OwnedSteps<'a> {
    pub(crate) fn push(
        &mut self,
        op: BinOp,
        op_span: Span,
        rhs: ExprValue<'a>,
        ty: ScratchType<'a>,
    ) -> R<()> {
        if !rhs.scope_matches(self.scope) || !ty.scope_matches(self.scope) {
            return super::allocation_error(self.scope, BudgetError::Overflow);
        }
        super::query::checked_depth(self.scope, rhs.0.depth.saturating_add(1))?;
        let tb = super::allocation_result(self.scope, super::type_backing(&ty))?;
        self.admit(plus(self.scope, rhs.bytes(), Some(tb))?)?;
        self.depth = self.depth.max(rhs.0.depth);
        self.value.push(tir::ArithStep {
            op,
            op_span,
            rhs: rhs.into_expr(),
            ty: ty.into_type(),
        });
        Ok(())
    }
}
impl<'a> OwnedDerefs<'a> {
    pub(crate) fn append(&mut self, mut other: Self) -> R<()> {
        if !same_scope(self.scope, other.scope) {
            return super::allocation_error(self.scope, BudgetError::Overflow);
        }
        let source = (other.value.capacity() * std::mem::size_of::<tir::Deref>()) as u64;
        let payload = other.bytes.and_then(|n| n.checked_sub(source));
        self.admit_count(payload, other.value.len())?;
        self.value.append(&mut other.value);
        other.bytes = Some(source);
        Ok(())
    }

    pub(crate) fn push(
        &mut self,
        name: &str,
        targets: &[(crate::schema::QTableId, crate::schema::QFieldId)],
    ) -> R<()> {
        let name = OwnedString::new(self.scope, name)?;
        let mut owned_targets = super::NativeVec::new(self.scope, targets.len())?;
        for value in targets {
            owned_targets.push_copy(*value)?;
        }
        let owned_targets = owned_targets.finish();
        let bytes = sum(self.scope, name.bytes(), owned_targets.native_bytes())?;
        self.admit(Some(bytes))?;
        self.value.push(tir::Deref {
            name: name.into_string(),
            targets: owned_targets.into_native(),
        });
        Ok(())
    }
}

pub(crate) struct ScalarString {
    value: QStr,
    managed: bool,
}
impl ScalarString {
    pub(crate) fn from_buffer(buffer: super::Utf16Buffer<'_>) -> R<Self> {
        let managed = match &buffer {
            super::Utf16Buffer::Legacy(_) => false,
            super::Utf16Buffer::Managed { values, scope } => {
                if !values.reservation().budget().same_instance(&scope.budget)
                    || values.reservation().category() != memory_budget::Category::Execution
                {
                    return super::allocation_error(Some(scope), BudgetError::Overflow);
                }
                true
            }
        };
        Ok(Self {
            value: buffer.finish()?,
            managed,
        })
    }
    pub(crate) fn legacy(value: QStr) -> Self {
        Self {
            value,
            managed: false,
        }
    }
    fn scope_matches(&self, scope: Option<&CompileArena>) -> bool {
        match (self.managed, scope) {
            (false, None) => true,
            (true, Some(scope)) => self.value.managed_reservation().is_some_and(|charge| {
                charge.budget().same_instance(&scope.budget)
                    && charge.category() == memory_budget::Category::Execution
            }),
            _ => false,
        }
    }
}

pub(crate) enum ScalarKind<'a> {
    Number(super::numeric::ScratchNumber<'a>),
    Date(bsl_prim::BslDate),
    Type(ScratchType<'a>),
    String(ScalarString),
    Value(PathValue<'a>),
    Param(OwnedString<'a>),
    Field {
        level: u16,
        source: u16,
        col: tir::Col,
        derefs: OwnedDerefs<'a>,
    },
    Neg(ExprBox<'a>),
    Not(ExprBox<'a>),
    Arith {
        first: ExprBox<'a>,
        steps: OwnedSteps<'a>,
    },
    And(ExprList<'a>),
    Or(ExprList<'a>),
    Compare(CmpOp, ExprBox<'a>, ExprBox<'a>),
    InList {
        l: ExprList<'a>,
        not: bool,
        hierarchy: bool,
        rhs: ExprList<'a>,
    },
    InQuery {
        l: ExprList<'a>,
        not: bool,
        hierarchy: bool,
        q: QueryBox<'a>,
    },
    Between {
        l: ExprBox<'a>,
        not: bool,
        lo: ExprBox<'a>,
        hi: ExprBox<'a>,
    },
    IsNull {
        l: ExprBox<'a>,
        not: bool,
    },
    Refs {
        l: ExprBox<'a>,
        table: crate::schema::QTableId,
    },
    Like {
        l: ExprBox<'a>,
        not: bool,
        pattern: ExprBox<'a>,
        escape: Option<char>,
    },
    Case {
        operand: Option<ExprBox<'a>>,
        whens: WhenList<'a>,
        else_: Option<ExprBox<'a>>,
    },
    Cast {
        e: ExprBox<'a>,
        to: ScratchType<'a>,
        derefs: OwnedDerefs<'a>,
        num: Option<crate::NumQ>,
    },
    Func {
        f: Func,
        args: ExprList<'a>,
    },
    Unit(Kw),
    Agg {
        f: Func,
        distinct: bool,
        arg: Option<ExprBox<'a>>,
        nested: bool,
    },
}
pub(crate) struct QueryBox<'a> {
    value: NativeOwned<'a, Box<tir::Query>>,
    scope: Option<&'a CompileArena>,
}
impl<'a> QueryBox<'a> {
    pub(crate) fn from_subquery(
        scope: Option<&'a CompileArena>,
        value: NativeOwned<'a, tir::Query>,
    ) -> R<Self> {
        if !value.scope_matches(scope) {
            return super::allocation_error(scope, BudgetError::Overflow);
        }
        let depth = super::query::checked_depth(scope, value.depth().saturating_add(1))?;
        Ok(Self {
            value: super::allocation_result(scope, value.boxed())?.set_depth(depth),
            scope,
        })
    }
}
impl ScalarKind<'_> {
    fn depth(&self) -> u16 {
        match self {
            Self::Neg(a) | Self::Not(a) => a.depth,
            Self::Arith { first, steps } => first.depth.max(steps.depth),
            Self::And(a) | Self::Or(a) | Self::Func { args: a, .. } => a.depth,
            Self::Compare(_, a, b) => a.depth.max(b.depth),
            Self::InList { l, rhs, .. } => l.depth.max(rhs.depth),
            Self::InQuery { l, q, .. } => l.depth.max(q.value.depth()),
            Self::Between { l, lo, hi, .. } => l.depth.max(lo.depth).max(hi.depth),
            Self::IsNull { l, .. } | Self::Refs { l, .. } => l.depth,
            Self::Like { l, pattern, .. } => l.depth.max(pattern.depth),
            Self::Case {
                operand,
                whens,
                else_,
            } => whens
                .depth
                .max(operand.as_ref().map_or(0, |e| e.depth))
                .max(else_.as_ref().map_or(0, |e| e.depth)),
            Self::Cast { e, .. } => e.depth,
            Self::Agg { arg, .. } => arg.as_ref().map_or(0, |e| e.depth),
            _ => 0,
        }
    }
    fn scope_matches(&self, scope: Option<&CompileArena>) -> bool {
        let check = |part| same_scope(scope, part);
        match self {
            Self::Type(ty) => ty.scope_matches(scope),
            Self::Number(n) => n.scope_matches(scope),
            Self::String(text) => text.scope_matches(scope),
            Self::Neg(a) | Self::Not(a) => check(a.scope),
            Self::Arith { first, steps } => check(first.scope) && check(steps.scope),
            Self::And(a) | Self::Or(a) | Self::Func { args: a, .. } => check(a.scope),
            Self::Compare(_, a, b) => check(a.scope) && check(b.scope),
            Self::InList { l, rhs, .. } => check(l.scope) && check(rhs.scope),
            Self::InQuery { l, q, .. } => check(l.scope) && check(q.scope),
            Self::Between { l, lo, hi, .. } => check(l.scope) && check(lo.scope) && check(hi.scope),
            Self::IsNull { l, .. } | Self::Refs { l, .. } => check(l.scope),
            Self::Like { l, pattern, .. } => check(l.scope) && check(pattern.scope),
            Self::Case {
                operand,
                whens,
                else_,
            } => {
                operand.as_ref().is_none_or(|a| check(a.scope))
                    && check(whens.scope)
                    && else_.as_ref().is_none_or(|a| check(a.scope))
            }
            Self::Cast { e, to, derefs, .. } => {
                check(e.scope) && to.scope_matches(scope) && check(derefs.scope)
            }
            Self::Agg { arg: Some(a), .. } => check(a.scope),
            Self::Field { derefs, .. } => check(derefs.scope),
            Self::Param(a) => check(a.scope),
            Self::Value(a) => check(a.scope),
            _ => true,
        }
    }
    fn bytes(&self, scope: Option<&CompileArena>) -> R<Option<u64>> {
        let mut bytes = Some(0);
        macro_rules! add {
            ($v:expr) => {{
                bytes = plus(scope, bytes, $v)?;
            }};
        }
        match self {
            Self::Date(_) | Self::String(_) | Self::Unit(_) | Self::Agg { arg: None, .. } => {}
            Self::Number(a) => add!(Some(a.backing_capacity_limbs() as u64 * 4)),
            Self::Type(a) => add!(Some(super::allocation_result(
                scope,
                super::type_backing(a)
            )?)),
            Self::Value(a) => add!(Some(a.bytes)),
            Self::Param(a) => add!(Some(a.bytes())),
            Self::Field { derefs, .. } => add!(derefs.bytes),
            Self::Neg(a) | Self::Not(a) => add!(a.bytes),
            Self::Arith { first, steps } => {
                add!(first.bytes);
                add!(steps.bytes);
            }
            Self::And(a) | Self::Or(a) | Self::Func { args: a, .. } => add!(a.bytes),
            Self::Compare(_, a, b) => {
                add!(a.bytes);
                add!(b.bytes);
            }
            Self::InList { l, rhs, .. } => {
                add!(l.bytes);
                add!(rhs.bytes);
            }
            Self::InQuery { l, q, .. } => {
                add!(l.bytes);
                add!(Some(q.value.native_bytes()));
            }
            Self::Between { l, lo, hi, .. } => {
                add!(l.bytes);
                add!(lo.bytes);
                add!(hi.bytes);
            }
            Self::IsNull { l, .. } | Self::Refs { l, .. } => add!(l.bytes),
            Self::Like { l, pattern, .. } => {
                add!(l.bytes);
                add!(pattern.bytes);
            }
            Self::Case {
                operand,
                whens,
                else_,
            } => {
                if let Some(a) = operand {
                    add!(a.bytes);
                }
                add!(whens.bytes);
                if let Some(a) = else_ {
                    add!(a.bytes);
                }
            }
            Self::Cast { e, to, derefs, .. } => {
                add!(e.bytes);
                add!(Some(super::allocation_result(
                    scope,
                    super::type_backing(to)
                )?));
                add!(derefs.bytes);
            }
            Self::Agg { arg: Some(a), .. } => add!(a.bytes),
        }
        Ok(bytes)
    }
    fn finish(self) -> TKind {
        match self {
            Self::Number(a) => TKind::Const(QValue::Number(a.into_number())),
            Self::Date(a) => TKind::Const(QValue::Date(a)),
            Self::Type(a) => TKind::Const(QValue::Type(a.into_type())),
            Self::String(a) => TKind::Const(QValue::String(a.value)),
            Self::Value(a) => TKind::Value(a.finish()),
            Self::Param(a) => TKind::Param(a.into_string()),
            Self::Field {
                level,
                source,
                col,
                derefs,
            } => TKind::Field {
                level,
                source,
                col,
                derefs: derefs.finish(),
            },
            Self::Neg(a) => TKind::Neg(a.finish()),
            Self::Not(a) => TKind::Not(a.finish()),
            Self::Arith { first, steps } => TKind::Arith {
                first: first.finish(),
                steps: steps.finish(),
            },
            Self::And(a) => TKind::And(a.finish()),
            Self::Or(a) => TKind::Or(a.finish()),
            Self::Compare(op, a, b) => TKind::Compare(op, a.finish(), b.finish()),
            Self::InList {
                l,
                not,
                hierarchy,
                rhs,
            } => TKind::InList {
                l: l.finish(),
                not,
                hierarchy,
                rhs: rhs.finish(),
            },
            Self::InQuery {
                l,
                not,
                hierarchy,
                q,
            } => TKind::InQuery {
                l: l.finish(),
                not,
                hierarchy,
                q: q.value.into_native(),
            },
            Self::Between { l, not, lo, hi } => TKind::Between {
                l: l.finish(),
                not,
                lo: lo.finish(),
                hi: hi.finish(),
            },
            Self::IsNull { l, not } => TKind::IsNull { l: l.finish(), not },
            Self::Refs { l, table } => TKind::Refs {
                l: l.finish(),
                table,
            },
            Self::Like {
                l,
                not,
                pattern,
                escape,
            } => TKind::Like {
                l: l.finish(),
                not,
                pattern: pattern.finish(),
                escape,
            },
            Self::Case {
                operand,
                whens,
                else_,
            } => TKind::Case {
                operand: operand.map(Part::finish),
                whens: whens.finish(),
                else_: else_.map(Part::finish),
            },
            Self::Cast { e, to, derefs, num } => TKind::Cast {
                e: e.finish(),
                to: to.into_type(),
                derefs: derefs.finish(),
                num,
            },
            Self::Func { f, args } => TKind::Func {
                f,
                args: args.finish(),
            },
            Self::Unit(a) => TKind::Unit(a),
            Self::Agg {
                f,
                distinct,
                arg,
                nested,
            } => TKind::Agg {
                f,
                distinct,
                arg: arg.map(Part::finish),
                nested,
            },
        }
    }
}
pub(crate) enum PathKind<'a> {
    Enum {
        table: crate::schema::QTableId,
        value: brace::Guid,
    },
    Predefined {
        table: crate::schema::QTableId,
        name: OwnedString<'a>,
    },
    EmptyRef(crate::schema::QTableId),
    SysEnum(platform_catalog::TypeIdx, u32),
    BusinessProcessPoint {
        table: crate::schema::QTableId,
        name: OwnedString<'a>,
    },
    TypeRef(crate::schema::QTableId),
    Unknown,
}
pub(crate) struct PathValue<'a> {
    value: ManuallyDrop<crate::schema::ValuePath>,
    scope: Option<&'a CompileArena>,
    bytes: u64,
}
impl<'a> PathValue<'a> {
    pub(crate) fn new(scope: Option<&'a CompileArena>, kind: PathKind<'a>) -> R<Self> {
        let bytes = match &kind {
            PathKind::Predefined { name, .. } | PathKind::BusinessProcessPoint { name, .. } => {
                if !same_scope(scope, name.scope) {
                    return super::allocation_error(scope, BudgetError::Overflow);
                }
                name.bytes()
            }
            _ => 0,
        };
        use crate::schema::ValuePath;
        let value = match kind {
            PathKind::Enum { table, value } => ValuePath::Enum { table, value },
            PathKind::Predefined { table, name } => ValuePath::Predefined {
                table,
                name: name.into_string(),
            },
            PathKind::EmptyRef(table) => ValuePath::EmptyRef(table),
            PathKind::SysEnum(ty, value) => ValuePath::SysEnum(ty, value),
            PathKind::BusinessProcessPoint { table, name } => ValuePath::BusinessProcessPoint {
                table,
                name: name.into_string(),
            },
            PathKind::TypeRef(table) => ValuePath::TypeRef(table),
            PathKind::Unknown => ValuePath::Unknown,
        };
        Ok(Self {
            value: ManuallyDrop::new(value),
            scope,
            bytes,
        })
    }
    fn finish(self) -> crate::schema::ValuePath {
        let mut this = ManuallyDrop::new(self);
        unsafe { ManuallyDrop::take(&mut this.value) }
    }
}
impl std::ops::Deref for PathValue<'_> {
    type Target = crate::schema::ValuePath;
    fn deref(&self) -> &Self::Target {
        &self.value
    }
}
impl Drop for PathValue<'_> {
    fn drop(&mut self) {
        unsafe { ManuallyDrop::drop(&mut self.value) };
        if let Some(scope) = self.scope {
            scope.release_owned(self.bytes);
        }
    }
}
