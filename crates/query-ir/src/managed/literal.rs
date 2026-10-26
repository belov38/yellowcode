use super::CompileArena;
use crate::sema::R;
use crate::tir::{Item, TExpr, TKind};
use crate::types::{NumQ, StrQ};
use crate::{QType, QValue};
use memory_budget::BudgetError;
use query_syntax::{Span, ast::Lit};
use std::alloc::Layout;
use std::mem::ManuallyDrop;

pub(crate) struct OwnedExpr<'a> {
    pub(super) value: ManuallyDrop<TExpr>,
    pub(super) scope: Option<&'a CompileArena>,
    pub(super) bytes: u64,
    pub(super) depth: u16,
}
fn numeric_type(null: bool) -> QType {
    let mut ty = QType::num(NumQ {
        digits: 0,
        scale: 0,
        nonneg: false,
    });
    ty.null = null;
    ty
}
fn add(scope: Option<&CompileArena>, left: u64, right: u64) -> R<u64> {
    super::allocation_result(scope, left.checked_add(right).ok_or(BudgetError::Overflow))
}
impl<'a> OwnedExpr<'a> {
    pub(crate) fn new(
        scope: Option<&'a CompileArena>,
        literal: &Lit,
        span: Span,
    ) -> R<Option<Self>> {
        let (value, ty) = match literal {
            Lit::Number(number) => {
                let Some(number) = super::numeric::ScratchNumber::parsed(scope, &number.digits)?
                else {
                    return Ok(None);
                };
                return Self::number_result(scope, number, span, false).map(Some);
            }
            Lit::String(text) => {
                let text = super::qstr(scope, text)?;
                let len = u32::try_from(text.len_utf16()).unwrap_or(u32::MAX).max(1);
                (
                    QValue::String(text),
                    QType::str_(StrQ { len, fixed: false }),
                )
            }
            Lit::Bool(value) => (QValue::Bool(*value), QType::boolean()),
            Lit::Null => (QValue::Null, QType::null_only()),
            Lit::Undefined => (QValue::Undefined, QType::undefined()),
        };
        Ok(Some(Self {
            value: ManuallyDrop::new(TExpr {
                kind: TKind::Const(value),
                ty,
                span,
            }),
            scope,
            bytes: 0,
            depth: 1,
        }))
    }
    fn number_result(
        scope: Option<&'a CompileArena>,
        number: super::numeric::ScratchNumber<'a>,
        span: Span,
        null: bool,
    ) -> R<Self> {
        let bytes = super::allocation_result(
            scope,
            Layout::array::<u32>(number.backing_capacity_limbs())
                .map(|layout| layout.size() as u64)
                .map_err(|_| BudgetError::Overflow),
        )?;
        Ok(Self {
            value: ManuallyDrop::new(TExpr {
                kind: TKind::Const(QValue::Number(number.into_number())),
                ty: numeric_type(null),
                span,
            }),
            scope,
            bytes,
            depth: 1,
        })
    }
    pub(crate) fn unary_type(op: query_syntax::ast::UnOp, null: bool) -> QType {
        if op == query_syntax::ast::UnOp::Not {
            let mut ty = QType::boolean();
            ty.null = null;
            ty
        } else {
            numeric_type(null)
        }
    }
    pub(crate) fn into_expr(self) -> TExpr {
        let mut owner = ManuallyDrop::new(self);
        unsafe { ManuallyDrop::take(&mut owner.value) }
    }
}
impl std::ops::Deref for OwnedExpr<'_> {
    type Target = TExpr;
    fn deref(&self) -> &TExpr {
        &self.value
    }
}
impl Drop for OwnedExpr<'_> {
    fn drop(&mut self) {
        destroy_expr(unsafe { ManuallyDrop::take(&mut self.value) });
        release(self.scope, self.bytes);
    }
}
pub(super) fn destroy_expr(mut expr: TExpr) {
    loop {
        let TExpr { kind, ty, .. } = expr;
        drop(ty);
        match kind {
            TKind::Not(child)
            | TKind::Neg(child)
            | TKind::IsNull { l: child, .. }
            | TKind::Refs { l: child, .. } => expr = *child,
            TKind::Cast { e, to, derefs, .. } => {
                drop(to);
                drop(derefs);
                expr = *e;
            }
            TKind::Agg {
                arg: Some(child), ..
            } => expr = *child,
            TKind::Arith { first, steps } => {
                for step in steps {
                    destroy_expr(step.rhs);
                    drop(step.ty);
                }
                expr = *first;
            }
            TKind::And(xs) | TKind::Or(xs) | TKind::Func { args: xs, .. } => {
                for child in xs {
                    destroy_expr(child);
                }
                break;
            }
            TKind::Compare(_, a, b) => {
                destroy_expr(*a);
                expr = *b;
            }
            TKind::Between { l, lo, hi, .. } => {
                destroy_expr(*l);
                destroy_expr(*lo);
                expr = *hi;
            }
            TKind::Like { l, pattern, .. } => {
                destroy_expr(*l);
                expr = *pattern;
            }
            TKind::InList { l, rhs, .. } => {
                for child in l.into_iter().chain(rhs) {
                    destroy_expr(child);
                }
                break;
            }
            TKind::InQuery { l, q, .. } => {
                for child in l {
                    destroy_expr(child);
                }
                drop(q);
                break;
            }
            TKind::Case {
                operand,
                whens,
                else_,
            } => {
                if let Some(child) = operand {
                    destroy_expr(*child);
                }
                for (a, b) in whens {
                    destroy_expr(a);
                    destroy_expr(b);
                }
                if let Some(child) = else_ {
                    expr = *child;
                } else {
                    break;
                }
            }
            leaf => {
                drop(leaf);
                break;
            }
        }
    }
}

pub(crate) struct OwnedString<'a> {
    value: ManuallyDrop<String>,
    pub(super) scope: Option<&'a CompileArena>,
}
impl<'a> OwnedString<'a> {
    pub(crate) fn new(scope: Option<&'a CompileArena>, text: &str) -> R<Self> {
        Ok(Self {
            value: ManuallyDrop::new(super::string(scope, text)?),
            scope,
        })
    }
    pub(super) fn bytes(&self) -> u64 {
        self.value.capacity() as u64
    }
    pub(super) fn into_string(self) -> String {
        let mut owner = ManuallyDrop::new(self);
        unsafe { ManuallyDrop::take(&mut owner.value) }
    }
}
impl std::ops::Deref for OwnedString<'_> {
    type Target = str;
    fn deref(&self) -> &str {
        &self.value
    }
}
impl Drop for OwnedString<'_> {
    fn drop(&mut self) {
        let bytes = self.value.capacity() as u64;
        unsafe { ManuallyDrop::drop(&mut self.value) };
        release(self.scope, bytes);
    }
}

pub(crate) struct ExprItem<'a> {
    expr: OwnedExpr<'a>,
    name: OwnedString<'a>,
    unknown: bool,
    nested: Option<super::NativeOwned<'a, Vec<Item>>>,
    bytes: u64,
    depth: u16,
}
impl<'a> ExprItem<'a> {
    pub(crate) fn new(expr: OwnedExpr<'a>, name: OwnedString<'a>, unknown: bool) -> R<Self> {
        if !same_scope(expr.scope, name.scope) {
            return super::allocation_error(expr.scope, BudgetError::Overflow);
        }
        let bytes = add(expr.scope, expr.bytes, name.value.capacity() as u64)?;
        let depth = expr.depth;
        Ok(Self {
            expr,
            name,
            unknown,
            nested: None,
            bytes,
            depth,
        })
    }
    pub(crate) fn with_nested(self, nested: super::FieldItems<'a>) -> R<Self> {
        self.with_nested_owned(nested.into_native_owned()?)
    }
    pub(crate) fn with_nested_owned(
        mut self,
        nested: super::NativeOwned<'a, Vec<Item>>,
    ) -> R<Self> {
        if !nested.scope_matches(self.expr.scope) {
            return super::allocation_error(self.expr.scope, BudgetError::Overflow);
        }
        let depth = super::query::checked_depth(
            self.expr.scope,
            self.depth.max(nested.depth().saturating_add(1)),
        )?;
        self.bytes = add(self.expr.scope, self.bytes, nested.native_bytes())?;
        self.depth = depth;
        self.nested = Some(nested);
        Ok(self)
    }
    fn into_item(self) -> Item {
        Item {
            star: false,
            expr: self.expr.into_expr(),
            name: self.name.into_string(),
            nested: self.nested.map(super::NativeOwned::into_native),
            unknown: self.unknown,
        }
    }
}

pub(crate) struct SelectItems<'a> {
    values: ManuallyDrop<Vec<Item>>,
    scope: Option<&'a CompileArena>,
    payload_bytes: u64,
    vector_bytes: u64,
    depth: u16,
}
impl<'a> SelectItems<'a> {
    pub(crate) fn new(scope: Option<&'a CompileArena>, capacity: usize) -> R<Self> {
        if scope.is_some() && capacity > 4096 {
            return super::allocation_error(scope, BudgetError::Overflow);
        }
        let values = super::vector(scope, capacity)?;
        let vector_bytes = vector_bytes(&values);
        Ok(Self {
            values: ManuallyDrop::new(values),
            scope,
            payload_bytes: 0,
            vector_bytes,
            depth: 0,
        })
    }
    pub(crate) fn push(&mut self, item: ExprItem<'a>) -> R<()> {
        if !same_scope(self.scope, item.expr.scope) {
            return super::allocation_error(self.scope, BudgetError::Overflow);
        }
        if self.scope.is_some() && self.values.len() >= 4096 {
            return super::allocation_error(self.scope, BudgetError::Overflow);
        }
        let bytes = add(self.scope, self.payload_bytes, item.bytes)?;
        super::reserve_additional(self.scope, &mut self.values, 1)?;
        self.vector_bytes = vector_bytes(&self.values);
        self.depth = self.depth.max(item.depth);
        self.values.push(item.into_item());
        self.payload_bytes = bytes;
        Ok(())
    }
    pub(crate) fn append_fields(&mut self, fields: super::FieldItems<'a>) -> R<()> {
        if !fields.scope_matches(self.scope) {
            return super::allocation_error(self.scope, BudgetError::Overflow);
        }
        if self.scope.is_some()
            && self
                .values
                .len()
                .checked_add(fields.len())
                .is_none_or(|len| len > 4096)
        {
            return super::allocation_error(self.scope, BudgetError::Overflow);
        }
        let incoming = super::allocation_result(self.scope, fields.payload_bytes_for_transfer())?;
        let bytes = add(self.scope, self.payload_bytes, incoming)?;
        self.depth = self.depth.max(fields.depth());
        fields.append_to(&mut self.values)?;
        self.vector_bytes = vector_bytes(&self.values);
        self.payload_bytes = bytes;
        Ok(())
    }
    pub(crate) fn items(&self) -> &[Item] {
        &self.values
    }
    pub(crate) fn into_items(self) -> Vec<Item> {
        let mut owner = ManuallyDrop::new(self);
        unsafe { ManuallyDrop::take(&mut owner.values) }
    }
}
impl Drop for SelectItems<'_> {
    fn drop(&mut self) {
        while let Some(item) = self.values.pop() {
            let Item {
                expr, name, nested, ..
            } = item;
            destroy_expr(expr);
            drop(name);
            drop(nested);
        }
        unsafe { ManuallyDrop::drop(&mut self.values) };
        release(self.scope, self.payload_bytes);
        release(self.scope, self.vector_bytes);
    }
}
fn vector_bytes(values: &Vec<Item>) -> u64 {
    (values.capacity() * std::mem::size_of::<Item>()) as u64
}
fn same_scope(left: Option<&CompileArena>, right: Option<&CompileArena>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => std::ptr::eq(left, right),
        _ => false,
    }
}
fn release(scope: Option<&CompileArena>, bytes: u64) {
    if let Some(scope) = scope {
        scope.release_owned(bytes);
    }
}

impl<'a> OwnedExpr<'a> {
    pub(crate) fn into_native_owned(self) -> super::NativeOwned<'a, TExpr> {
        let scope = self.scope;
        let bytes = self.bytes;
        let depth = self.depth;
        super::NativeOwned::leaf(self.into_expr(), scope, bytes).set_depth(depth)
    }
}
impl<'a> OwnedString<'a> {
    pub(crate) fn into_native_owned(self) -> super::NativeOwned<'a, String> {
        let scope = self.scope;
        let bytes = self.bytes();
        super::NativeOwned::leaf(self.into_string(), scope, bytes)
    }
}
impl<'a> SelectItems<'a> {
    pub(crate) fn into_native_owned(self) -> super::NativeOwned<'a, Vec<Item>> {
        let scope = self.scope;
        let bytes = self.payload_bytes + self.vector_bytes;
        let depth = self.depth;
        super::NativeOwned::leaf(self.into_items(), scope, bytes).set_depth(depth)
    }
}
