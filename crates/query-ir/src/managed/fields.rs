use super::{AResult, CompileArena, NameKey, reserve_additional, string, type_backing, type_copy};
use crate::QType;
use crate::sema::R;
use crate::tir::{Col, Item, TExpr, TKind};
use memory_budget::BudgetError;
use query_syntax::Span;
use std::alloc::Layout;
use std::mem::ManuallyDrop;

pub(crate) struct FieldItem<'a> {
    value: ManuallyDrop<Item>,
    scope: Option<&'a CompileArena>,
    depth: u16,
}
impl<'a> FieldItem<'a> {
    pub(crate) fn new(
        scope: Option<&'a CompileArena>,
        source: u16,
        col: Col,
        ty: &QType,
        name: &str,
        span: Span,
    ) -> R<Self> {
        let ty = type_copy(scope, ty)?;
        let name = string(scope, name)?;
        Ok(Self {
            value: ManuallyDrop::new(Item {
                expr: TExpr {
                    kind: TKind::Field {
                        level: 0,
                        source,
                        col,
                        derefs: Vec::new(),
                    },
                    ty: ty.into_type(),
                    span,
                },
                name,
                nested: None,
                unknown: false,
            }),
            scope,
            depth: 1,
        })
    }
    pub(crate) fn set_nullable(&mut self, nullable: bool) {
        self.value.expr.ty.null |= nullable;
    }
    pub(crate) fn set_unknown(&mut self, unknown: bool) {
        self.value.unknown = unknown;
    }
    pub(crate) fn set_nested(&mut self, nested: FieldItems<'a>) -> R<()> {
        if !same_scope(self.scope, nested.scope) {
            return super::allocation_error(self.scope, BudgetError::Overflow);
        }
        self.depth = super::query::checked_depth(self.scope, nested.depth.saturating_add(1))?;
        let old = self.value.nested.replace(nested.into_items());
        if let Some(old) = old {
            drop(FieldItems {
                values: ManuallyDrop::new(old),
                scope: self.scope,
                depth: 0,
            });
        }
        Ok(())
    }
    fn into_item(self) -> Item {
        let mut owner = ManuallyDrop::new(self);
        unsafe { ManuallyDrop::take(&mut owner.value) }
    }
}
impl Drop for FieldItem<'_> {
    fn drop(&mut self) {
        let bytes = item_backing(&self.value);
        unsafe { ManuallyDrop::drop(&mut self.value) };
        release(self.scope, bytes);
    }
}

pub(crate) struct FieldItems<'a> {
    values: ManuallyDrop<Vec<Item>>,
    scope: Option<&'a CompileArena>,
    depth: u16,
}
impl<'a> FieldItems<'a> {
    pub(crate) fn into_native_owned(self) -> crate::sema::R<super::NativeOwned<'a, Vec<Item>>> {
        let scope = self.scope;
        let bytes = super::allocation_result(scope, self.native_bytes_for_transfer())?;
        let depth = self.depth;
        Ok(super::NativeOwned::leaf(self.into_items(), scope, bytes).set_depth(depth))
    }

    pub(crate) fn new(scope: Option<&'a CompileArena>) -> Self {
        Self {
            values: ManuallyDrop::new(Vec::new()),
            scope,
            depth: 0,
        }
    }
    pub(crate) fn depth(&self) -> u16 {
        self.depth
    }
    pub(crate) fn len(&self) -> usize {
        self.values.len()
    }
    pub(crate) fn scope_matches(&self, scope: Option<&CompileArena>) -> bool {
        same_scope(self.scope, scope)
    }
    pub(super) fn native_bytes_for_transfer(&self) -> AResult<u64> {
        let storage = Layout::array::<Item>(self.values.capacity())
            .map_err(|_| BudgetError::Overflow)?
            .size() as u64;
        self.payload_bytes_for_transfer()?
            .checked_add(storage)
            .ok_or(BudgetError::Overflow)
    }
    pub(super) fn payload_bytes_for_transfer(&self) -> AResult<u64> {
        self.values.iter().try_fold(0_u64, |bytes, item| {
            bytes
                .checked_add(item_backing(item)?)
                .ok_or(BudgetError::Overflow)
        })
    }
    pub(crate) fn push(&mut self, item: FieldItem<'a>) -> R<()> {
        if !same_scope(self.scope, item.scope) {
            return super::allocation_error(self.scope, BudgetError::Overflow);
        }
        if self.scope.is_some() && self.values.len() >= 4096 {
            return super::allocation_error(self.scope, BudgetError::Overflow);
        }
        reserve_additional(self.scope, &mut self.values, 1)?;
        self.depth = self.depth.max(item.depth);
        self.values.push(item.into_item());
        Ok(())
    }
    pub(crate) fn rename_with(&mut self, mut name: impl FnMut(&str) -> R<NameKey>) -> R<()> {
        for item in self.values.iter_mut() {
            let next = string(self.scope, &name(&item.name)?)?;
            let old = std::mem::replace(&mut item.name, next);
            let bytes = old.capacity() as u64;
            drop(old);
            release(self.scope, Ok(bytes));
        }
        Ok(())
    }
    pub(crate) fn append(&mut self, other: Self) -> R<()> {
        if !same_scope(self.scope, other.scope) {
            return super::allocation_error(self.scope, BudgetError::Overflow);
        }
        if self.scope.is_some()
            && self
                .values
                .len()
                .checked_add(other.len())
                .is_none_or(|len| len > 4096)
        {
            return super::allocation_error(self.scope, BudgetError::Overflow);
        }
        self.depth = self.depth.max(other.depth);
        other.append_to(&mut self.values)
    }
    pub(crate) fn append_to(mut self, destination: &mut Vec<Item>) -> R<()> {
        reserve_additional(self.scope, destination, self.values.len())?;
        destination.append(&mut self.values);
        Ok(())
    }
    pub(crate) fn into_items(self) -> Vec<Item> {
        let mut owner = ManuallyDrop::new(self);
        unsafe { ManuallyDrop::take(&mut owner.values) }
    }
}
impl Drop for FieldItems<'_> {
    fn drop(&mut self) {
        let bytes = list_backing(&self.values);
        unsafe { ManuallyDrop::drop(&mut self.values) };
        release(self.scope, bytes);
    }
}

fn same_scope(a: Option<&CompileArena>, b: Option<&CompileArena>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => std::ptr::eq(a, b),
        _ => false,
    }
}
fn release(scope: Option<&CompileArena>, bytes: AResult<u64>) {
    if let Some(scope) = scope {
        let _ = scope
            .compile(bytes.and_then(|bytes| scope.charge.shrink(bytes).map_err(BudgetError::from)));
    }
}
pub(super) fn item_backing(item: &Item) -> AResult<u64> {
    debug_assert!(matches!(&item.expr.kind, TKind::Field { derefs, .. } if derefs.is_empty()));
    let bytes = type_backing(&item.expr.ty)?
        .checked_add(item.name.capacity() as u64)
        .ok_or(BudgetError::Overflow)?;
    let nested = item
        .nested
        .as_ref()
        .map(list_backing)
        .transpose()?
        .unwrap_or(0);
    bytes.checked_add(nested).ok_or(BudgetError::Overflow)
}
fn list_backing(items: &Vec<Item>) -> AResult<u64> {
    let bytes = Layout::array::<Item>(items.capacity())
        .map_err(|_| BudgetError::Overflow)?
        .size() as u64;
    items.iter().try_fold(bytes, |bytes, item| {
        bytes
            .checked_add(item_backing(item)?)
            .ok_or(BudgetError::Overflow)
    })
}
