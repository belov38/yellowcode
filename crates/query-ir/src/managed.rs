pub(crate) use diagnostic::Argument;
pub(crate) use diagnostic::{DiagnosticProjection, Failure, ProjectedArgument};
pub(crate) mod statement;
use memory_budget::{BudgetError, MemoryBudget, Reservation};
use std::alloc::{Layout, alloc};

struct LocalCell<T>(std::cell::RefCell<T>);
impl<T> LocalCell<T> {
    fn new(value: T) -> Self {
        Self(std::cell::RefCell::new(value))
    }
    fn lock(
        &self,
    ) -> Result<std::cell::RefMut<'_, T>, std::sync::PoisonError<std::cell::RefMut<'_, T>>> {
        Ok(self.0.borrow_mut())
    }
}

pub(crate) type AResult<T> = Result<T, BudgetError>;

mod fields;
mod literal;
mod scalar;
pub(crate) use literal::{ExprItem, OwnedExpr, OwnedExpr as LiteralExpr, OwnedString, SelectItems};
pub(crate) use scalar::{
    ExprList, ExprValue, OwnedDerefs, OwnedSteps, PathKind, PathValue, QueryBox, ScalarKind,
    ScalarString, WhenList,
};
mod native;
mod numeric;
pub(crate) mod query;
pub(crate) use fields::{FieldItem, FieldItems};
pub(crate) use native::{NativeOwned, NativeVec, owned_copy};
#[allow(dead_code)]
mod diagnostic;
pub(crate) use numeric::ScratchNumber;
pub(crate) use numeric::{OwnedNumericPrefix, number_neg, numeric_prefix_owned};

struct NativeCharge(std::cell::RefCell<Reservation>);
impl NativeCharge {
    fn try_grow(&self, bytes: u64) -> Result<(), memory_budget::MemoryLimit> {
        self.0.borrow().try_grow(bytes)
    }
    fn shrink(&self, bytes: u64) -> Result<(), memory_budget::MemoryLimit> {
        self.0.borrow().shrink(bytes)
    }
    fn split(&self, bytes: u64) -> Result<Reservation, memory_budget::MemoryLimit> {
        self.0.borrow_mut().split(bytes)
    }
}
pub(crate) struct CompileArena {
    budget: MemoryBudget,
    charge: NativeCharge,
    error: LocalCell<Option<BudgetError>>,
    drop_error: LocalCell<Option<(u64, BudgetError)>>,
    diagnostic: std::cell::RefCell<Option<Failure>>,
    shared_depth: usize,
    column_depth: std::cell::Cell<usize>,
    thread_confined: std::marker::PhantomData<std::rc::Rc<()>>,
}
impl CompileArena {
    pub(crate) fn child(budget: &MemoryBudget, shared_depth: usize) -> AResult<Self> {
        Ok(Self {
            budget: budget.clone(),
            charge: NativeCharge(std::cell::RefCell::new(
                budget.try_reserve(memory_budget::Category::Execution, 0)?,
            )),
            error: LocalCell::new(None),
            drop_error: LocalCell::new(None),
            diagnostic: std::cell::RefCell::new(None),
            shared_depth,
            column_depth: std::cell::Cell::new(0),
            thread_confined: std::marker::PhantomData,
        })
    }
    pub(crate) fn checkpoint(&self) -> crate::sema::R<()> {
        if let Some(error) = self.error() {
            return self.compile(Err(error));
        }
        self.compile(
            self.budget
                .check_admission(memory_budget::Category::Execution)
                .map_err(BudgetError::from),
        )
    }
    fn reserve(&self, layout: Layout) -> AResult<()> {
        if let Some(error) = self.error() {
            return Err(error);
        }
        self.charge.try_grow(layout.size() as u64)?;
        Ok(())
    }
    pub(crate) fn vector<T>(&self, capacity: usize) -> AResult<Vec<T>> {
        let layout = Layout::array::<T>(capacity).map_err(|_| BudgetError::Overflow)?;
        self.reserve(layout)?;
        if layout.size() == 0 {
            return Ok(Vec::new());
        }
        let data = unsafe { alloc(layout) }.cast::<T>();
        if data.is_null() {
            self.charge.shrink(layout.size() as u64)?;
            return Err(BudgetError::AllocationFailed);
        }
        Ok(unsafe { Vec::from_raw_parts(data, 0, capacity) })
    }
    pub(crate) fn ensure_capacity<T>(&self, values: &mut Vec<T>, required: usize) -> AResult<()> {
        if required > values.capacity() {
            let capacity = values
                .capacity()
                .checked_mul(2)
                .ok_or(BudgetError::Overflow)?
                .max(required);
            let mut next = self.vector(capacity)?;
            next.append(values);
            let old = std::mem::replace(values, next);
            let old_layout =
                Layout::array::<T>(old.capacity()).map_err(|_| BudgetError::Overflow)?;
            drop(old);
            self.charge.shrink(old_layout.size() as u64)?;
        }
        Ok(())
    }
    pub(crate) fn push<T>(&self, values: &mut Vec<T>, value: T) -> AResult<()> {
        let required = values.len().checked_add(1).ok_or(BudgetError::Overflow)?;
        self.ensure_capacity(values, required)?;
        values.push(value);
        Ok(())
    }
    pub(crate) fn boxed<T>(&self, value: T) -> AResult<Box<T>> {
        let layout = Layout::new::<T>();
        self.reserve(layout)?;
        if layout.size() == 0 {
            return Ok(Box::new(value));
        }
        let data = unsafe { alloc(layout) }.cast::<T>();
        if data.is_null() {
            self.charge.shrink(layout.size() as u64)?;
            return Err(BudgetError::AllocationFailed);
        }
        unsafe {
            data.write(value);
            Ok(Box::from_raw(data))
        }
    }
    pub(crate) fn string(&self, value: &str) -> AResult<String> {
        let mut bytes = self.vector::<u8>(value.len())?;
        bytes.extend_from_slice(value.as_bytes());
        Ok(unsafe { String::from_utf8_unchecked(bytes) })
    }
}

impl CompileArena {
    fn release_owned(&self, bytes: u64) {
        if let Err(error) = self.charge.shrink(bytes) {
            let error = BudgetError::from(error);
            let mut proof = self
                .drop_error
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if proof.is_none() {
                *proof = Some((bytes, error));
            }
            let mut first = self
                .error
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if first.is_none() {
                *first = Some(error);
            }
        }
    }
    pub(crate) fn error(&self) -> Option<BudgetError> {
        *self
            .error
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
    fn diagnostic_error(&self, mut failure: Failure) -> query_syntax::QueryError {
        if !failure.same_scope(&self.budget) {
            failure = Failure::Resource(BudgetError::Overflow);
        }
        let resource = {
            let mut stored = self
                .error
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if stored.is_none() {
                *stored = failure.resource();
            }
            *stored
        };
        if let Some(error) = resource {
            drop(failure);
            let previous = self.diagnostic.replace(Some(Failure::Resource(error)));
            drop(previous);
            return query_syntax::QueryError::new(
                query_syntax::MsgId::SELECT_ERROR_SYNTAXERROR,
                None,
            );
        }
        let mut slot = self.diagnostic.borrow_mut();
        if slot.is_none() {
            *slot = Some(failure);
        } else {
            drop(slot);
            drop(failure);
            slot = self.diagnostic.borrow_mut();
        }
        let Some(diagnostic) = slot.as_ref().and_then(Failure::managed) else {
            drop(slot);
            return self.diagnostic_error(Failure::Resource(BudgetError::Overflow));
        };
        query_syntax::QueryError::new(diagnostic.msg, diagnostic.at)
    }
    pub(crate) fn take_diagnostic(&self) -> Option<Failure> {
        self.diagnostic.take()
    }
    pub(crate) fn retain_scalar_error(
        &self,
        error: query_syntax::QueryError,
    ) -> query_syntax::QueryError {
        if let Some(first) = self.error() {
            return self.diagnostic_error(Failure::Resource(first));
        }
        if self.diagnostic.borrow().is_some() {
            return error;
        }
        if !error.subst.is_empty()
            || error.quoted.is_some()
            || error.detail.is_some()
            || error.cause.is_some()
        {
            return self.diagnostic_error(Failure::Resource(BudgetError::Overflow));
        }
        self.diagnostic_error(Failure::new(Some(&self.budget), error.msg, error.at))
    }
    pub(crate) fn unpositioned_error(&self, msg: query_syntax::MsgId) -> query_syntax::QueryError {
        self.diagnostic_error(Failure::new(Some(&self.budget), msg, None))
    }
    pub(crate) fn quoted_error(
        &self,
        msg: query_syntax::MsgId,
        at: query_syntax::ErrAt,
        text: &str,
    ) -> query_syntax::QueryError {
        if let Some(error) = self.error() {
            return self.diagnostic_error(Failure::Resource(error));
        }
        self.diagnostic_error(
            Failure::new(Some(&self.budget), msg, Some(at)).with_quoted(Argument::Text(text)),
        )
    }
    pub(crate) fn quoted_parts_error<'a>(
        &self,
        msg: query_syntax::MsgId,
        at: query_syntax::ErrAt,
        parts: impl IntoIterator<Item = &'a str>,
        separator: &str,
    ) -> query_syntax::QueryError {
        if let Some(error) = self.error() {
            return self.diagnostic_error(Failure::Resource(error));
        }
        self.diagnostic_error(
            Failure::new(Some(&self.budget), msg, Some(at)).with_quoted_parts(parts, separator),
        )
    }
    pub(crate) fn semantic_subst_error<'a>(
        &self,
        msg: query_syntax::MsgId,
        at: Option<query_syntax::ErrAt>,
        arguments: impl IntoIterator<Item = Argument<'a>>,
    ) -> query_syntax::QueryError {
        if let Some(error) = self.error() {
            return self.resource_error(error);
        }
        let mut failure = Failure::new(Some(&self.budget), msg, at);
        for argument in arguments {
            failure = failure.with_subst(argument);
        }
        self.diagnostic_error(failure)
    }
    pub(crate) fn semantic_error(
        &self,
        msg: query_syntax::MsgId,
        at: Option<query_syntax::ErrAt>,
        quoted: Option<Argument<'_>>,
    ) -> query_syntax::QueryError {
        if let Some(error) = self.error() {
            return self.resource_error(error);
        }
        let mut failure = Failure::new(Some(&self.budget), msg, at);
        if let Some(argument) = quoted {
            failure = failure.with_quoted(argument);
        }
        self.diagnostic_error(failure)
    }
    pub(crate) fn quoted_chars_error(
        &self,
        msg: query_syntax::MsgId,
        at: query_syntax::ErrAt,
        chars: impl IntoIterator<Item = char>,
    ) -> query_syntax::QueryError {
        if let Some(error) = self.error() {
            return self.resource_error(error);
        }
        self.diagnostic_error(
            Failure::new(Some(&self.budget), msg, Some(at)).with_quoted_chars(chars),
        )
    }
    pub(crate) fn resource_error(&self, error: BudgetError) -> query_syntax::QueryError {
        self.diagnostic_error(Failure::Resource(error))
    }
    pub(crate) fn quoted_parts_cause_error<'a>(
        &self,
        msg: query_syntax::MsgId,
        at: Option<query_syntax::ErrAt>,
        parts: impl IntoIterator<Item = &'a str>,
        separator: &str,
        cause_msg: query_syntax::MsgId,
        cause_subst: Option<&str>,
    ) -> query_syntax::QueryError {
        if let Some(error) = self.error() {
            return self.resource_error(error);
        }
        let failure = Failure::new(Some(&self.budget), msg, at).with_quoted_parts(parts, separator);
        if failure.resource().is_some() {
            return self.diagnostic_error(failure);
        }
        let mut cause = Failure::new(Some(&self.budget), cause_msg, None);
        if let Some(text) = cause_subst {
            cause = cause.with_subst(Argument::Text(text));
        }
        self.diagnostic_error(failure.with_cause(cause))
    }
    fn compile<T>(&self, result: AResult<T>) -> crate::sema::R<T> {
        result.map_err(|error| self.diagnostic_error(Failure::Resource(error)))
    }
}
pub(crate) fn share_owned<'a, T: native::SharedPayload>(
    scope: Option<&'a CompileArena>,
    value: NativeOwned<'a, T>,
) -> crate::sema::R<crate::QShared<T>> {
    if !value.scope_matches(scope) {
        return allocation_error(scope, BudgetError::Overflow);
    }
    match scope {
        Some(scope) => scope.compile(
            value
                .into_durable()
                .and_then(native::DurableOwned::into_shared),
        ),
        None => Ok(std::sync::Arc::new(value.into_native()).into()),
    }
}
pub(crate) fn copy<T: ManagedClone + Clone>(
    scope: Option<&CompileArena>,
    value: &T,
) -> crate::sema::R<T> {
    owned_copy(scope, value).map(NativeOwned::into_native)
}
pub(crate) fn vector<T>(scope: Option<&CompileArena>, capacity: usize) -> crate::sema::R<Vec<T>> {
    match scope {
        Some(scope) => scope.compile(scope.vector(capacity)),
        None => Ok(Vec::with_capacity(capacity)),
    }
}
pub(crate) fn push<T>(
    scope: Option<&CompileArena>,
    values: &mut Vec<T>,
    value: T,
) -> crate::sema::R<()> {
    match scope {
        Some(scope) => scope.compile(scope.push(values, value)),
        None => {
            values.push(value);
            Ok(())
        }
    }
}
pub(crate) fn reserve_additional<T>(
    scope: Option<&CompileArena>,
    values: &mut Vec<T>,
    count: usize,
) -> crate::sema::R<()> {
    match scope {
        Some(scope) => {
            let required = values.len().checked_add(count).ok_or(BudgetError::Overflow);
            scope.compile(required.and_then(|required| scope.ensure_capacity(values, required)))
        }
        None => {
            values.reserve(count);
            Ok(())
        }
    }
}
pub(crate) fn list<T, const N: usize>(
    scope: Option<&CompileArena>,
    values: [T; N],
) -> crate::sema::R<Vec<T>> {
    let mut output = vector(scope, N)?;
    output.extend(values);
    Ok(output)
}
pub(crate) fn string(scope: Option<&CompileArena>, value: &str) -> crate::sema::R<String> {
    match scope {
        Some(scope) => scope.compile(scope.string(value)),
        None => Ok(value.to_string()),
    }
}
pub(crate) enum TypeView<'a> {
    Borrowed(&'a crate::QType),
    Owned(ScratchType<'a>),
}
impl std::ops::Deref for TypeView<'_> {
    type Target = crate::QType;
    fn deref(&self) -> &Self::Target {
        match self {
            Self::Borrowed(value) => value,
            Self::Owned(value) => value,
        }
    }
}

pub(crate) enum ScopeType {
    Legacy(crate::QType),
    Managed(memory_budget::Charged<crate::QType>),
}
impl ScopeType {
    pub(crate) fn copy(scope: Option<&CompileArena>, value: &crate::QType) -> crate::sema::R<Self> {
        match scope {
            None => Ok(Self::Legacy(value.clone())),
            Some(scope) => {
                scope.compile(value.try_clone_budgeted(&scope.budget).map(Self::Managed))
            }
        }
    }
    pub(crate) fn merge(
        &self,
        scope: Option<&CompileArena>,
        other: &crate::QType,
    ) -> crate::sema::R<Self> {
        match scope {
            None => Ok(Self::Legacy((**self).merge(other))),
            Some(scope) => scope.compile(
                self.try_merge_budgeted(other, &scope.budget)
                    .map(Self::Managed),
            ),
        }
    }
    pub(crate) fn with_null(self) -> Self {
        match self {
            Self::Legacy(value) => Self::Legacy(value.with_null()),
            Self::Managed(value) => Self::Managed(value.map(crate::QType::with_null)),
        }
    }
}
impl std::ops::Deref for ScopeType {
    type Target = crate::QType;
    fn deref(&self) -> &crate::QType {
        match self {
            Self::Legacy(value) => value,
            Self::Managed(value) => value,
        }
    }
}

pub(crate) struct ScratchType<'a> {
    value: std::mem::ManuallyDrop<crate::QType>,
    scope: Option<&'a CompileArena>,
}
impl<'a> ScratchType<'a> {
    pub(crate) fn new(scope: Option<&'a CompileArena>, value: crate::QType) -> Self {
        Self {
            value: std::mem::ManuallyDrop::new(value),
            scope,
        }
    }
    pub(super) fn scope_matches(&self, scope: Option<&CompileArena>) -> bool {
        match (self.scope, scope) {
            (Some(a), Some(b)) => std::ptr::eq(a, b),
            (None, None) => true,
            _ => false,
        }
    }
    pub(crate) fn into_type(self) -> crate::QType {
        let mut owner = std::mem::ManuallyDrop::new(self);
        unsafe { std::mem::ManuallyDrop::take(&mut owner.value) }
    }
    pub(crate) fn with_string(mut self, qualifier: crate::StrQ) -> Self {
        self.value.string = Some(qualifier);
        self
    }
    pub(crate) fn without_null(mut self) -> Self {
        self.value.null = false;
        self
    }
    pub(crate) fn with_null(mut self) -> Self {
        self.value.null = true;
        self
    }
}
impl std::ops::Deref for ScratchType<'_> {
    type Target = crate::QType;
    fn deref(&self) -> &Self::Target {
        &self.value
    }
}
fn type_backing(value: &crate::QType) -> AResult<u64> {
    let refs = Layout::array::<crate::types::RefAtom>(value.refs.capacity())
        .map_err(|_| BudgetError::Overflow)?
        .size();
    let others = Layout::array::<crate::types::Other>(value.others.capacity())
        .map_err(|_| BudgetError::Overflow)?
        .size();
    refs.checked_add(others)
        .map(|size| size as u64)
        .ok_or(BudgetError::Overflow)
}
impl Drop for ScratchType<'_> {
    fn drop(&mut self) {
        let bytes = type_backing(&self.value);
        unsafe { std::mem::ManuallyDrop::drop(&mut self.value) };
        if let Some(scope) = self.scope {
            let result =
                bytes.and_then(|bytes| scope.charge.shrink(bytes).map_err(BudgetError::from));
            let _ = scope.compile(result);
        }
    }
}
pub(crate) fn type_copy<'a>(
    scope: Option<&'a CompileArena>,
    value: &crate::QType,
) -> crate::sema::R<ScratchType<'a>> {
    copy(scope, value).map(|value| ScratchType::new(scope, value))
}
pub(crate) fn type_merge<'a>(
    scope: Option<&'a CompileArena>,
    left: &crate::QType,
    right: &crate::QType,
) -> crate::sema::R<ScratchType<'a>> {
    merge(scope, left, right).map(|value| ScratchType::new(scope, value))
}
pub(crate) fn replace_type(
    scope: Option<&CompileArena>,
    target: &mut crate::QType,
    value: crate::QType,
) -> crate::sema::R<()> {
    drop(ScratchType::new(scope, std::mem::replace(target, value)));
    Ok(())
}

pub(crate) fn merge(
    scope: Option<&CompileArena>,
    left: &crate::QType,
    right: &crate::QType,
) -> crate::sema::R<crate::QType> {
    let Some(scope) = scope else {
        return Ok(left.merge(right));
    };
    fn union<T: Copy + Ord>(scope: &CompileArena, left: &[T], right: &[T]) -> AResult<Vec<T>> {
        let capacity = left
            .len()
            .checked_add(right.len())
            .ok_or(BudgetError::Overflow)?;
        let mut values = scope.vector(capacity)?;
        values.extend_from_slice(left);
        values.extend_from_slice(right);
        values.sort_unstable();
        values.dedup();
        Ok(values)
    }
    let result = (|| {
        let mut output = ScratchType::new(Some(scope), left.merged_qualifiers(right));
        output.value.refs = union(scope, &left.refs, &right.refs)?;
        output.value.others = union(scope, &left.others, &right.others)?;
        Ok(output.into_type())
    })();
    scope.compile(result)
}

pub(crate) fn type_result(
    scope: Option<&CompileArena>,
    recipe: crate::types::rules::TypeResult<'_>,
) -> crate::sema::R<crate::QType> {
    use crate::types::rules::TypeResult;
    if scope.is_none() {
        return Ok(recipe.into_legacy());
    }
    match recipe {
        TypeResult::Scalar(value) => Ok(value),
        TypeResult::Other(atom) => other(scope, atom),
        TypeResult::WithoutNull(value) => Ok(type_copy(scope, value)?.without_null().into_type()),
        TypeResult::UnionWithoutNull(left, right) => {
            Ok(type_merge(scope, left, right)?.without_null().into_type())
        }
        TypeResult::RefPresentation(value) => {
            let mut output = ScratchType::new(
                scope,
                crate::QType::str_(crate::StrQ {
                    len: 0,
                    fixed: false,
                }),
            );
            output.value.refs = copy(scope, &value.refs)?;
            let count = value
                .others
                .iter()
                .filter(|atom| matches!(atom, crate::Other::RoutePoint(_)))
                .count();
            output.value.others = vector(scope, count)?;
            output.value.others.extend(
                value
                    .others
                    .iter()
                    .copied()
                    .filter(|atom| matches!(atom, crate::Other::RoutePoint(_))),
            );
            Ok(output.into_type())
        }
    }
}

pub(crate) fn number(
    scope: Option<&CompileArena>,
    text: &str,
) -> crate::sema::R<Option<bsl_prim::Number>> {
    let Some(scope) = scope else {
        return Ok(crate::fold::number_literal(text));
    };
    let output = numeric::parse_number(scope, text);
    scope.compile(output)
}
pub(crate) fn qstr(scope: Option<&CompileArena>, text: &str) -> crate::sema::R<crate::QStr> {
    match scope {
        Some(scope) => scope.compile(crate::QStr::try_from_str_budgeted(text, &scope.budget)),
        None => Ok(crate::QStr::from_str(text)),
    }
}

pub(crate) fn collect<T>(
    scope: Option<&CompileArena>,
    values: impl IntoIterator<Item = crate::sema::R<T>>,
) -> crate::sema::R<Vec<T>> {
    let values = values.into_iter();
    let mut output = vector(scope, values.size_hint().0)?;
    for value in values {
        push(scope, &mut output, value?)?;
    }
    Ok(output)
}

pub(crate) enum NameKey {
    Legacy(String),
    Managed(memory_budget::BudgetedText),
}
impl NameKey {
    pub(crate) fn formatted(
        scope: Option<&CompileArena>,
        render: impl FnOnce(&mut dyn std::fmt::Write) -> std::fmt::Result,
    ) -> crate::sema::R<Self> {
        match scope {
            None => {
                let mut output = String::new();
                render(&mut output).map_err(|_| {
                    query_syntax::QueryError::new(
                        query_syntax::MsgId::SELECT_ERROR_SYNTAXERROR,
                        None,
                    )
                })?;
                Ok(Self::Legacy(output))
            }
            Some(scope) => {
                let mut output = scope.compile(memory_budget::BudgetedText::new(
                    &scope.budget,
                    memory_budget::Category::Execution,
                    u64::MAX,
                ))?;
                if render(&mut output).is_err() {
                    return scope.compile(Err(output
                        .error()
                        .copied()
                        .or_else(|| scope.error())
                        .unwrap_or(BudgetError::Overflow)));
                }
                Ok(Self::Managed(output))
            }
        }
    }
    pub(crate) fn raw(scope: Option<&CompileArena>, value: &str) -> crate::sema::R<Self> {
        match scope {
            None => Ok(Self::Legacy(value.to_string())),
            Some(scope) => scope.compile((|| {
                let mut text = memory_budget::BudgetedText::new(
                    &scope.budget,
                    memory_budget::Category::Execution,
                    u64::MAX,
                )?;
                text.try_push_str(value)?;
                Ok(Self::Managed(text))
            })()),
        }
    }
    pub(crate) fn suffixed(
        scope: Option<&CompileArena>,
        value: &str,
        suffix: &str,
    ) -> crate::sema::R<Self> {
        match scope {
            None => Ok(Self::Legacy(format!("{value}{suffix}"))),
            Some(scope) => scope.compile((|| {
                let mut text = memory_budget::BudgetedText::new(
                    &scope.budget,
                    memory_budget::Category::Execution,
                    u64::MAX,
                )?;
                text.try_push_str(value)?;
                text.try_push_str(suffix)?;
                Ok(Self::Managed(text))
            })()),
        }
    }
    pub(crate) fn numbered(
        scope: Option<&CompileArena>,
        value: &str,
        number: u32,
    ) -> crate::sema::R<Self> {
        match scope {
            None => Ok(Self::Legacy(format!("{value}{number}"))),
            Some(scope) => scope.compile((|| {
                let mut text = memory_budget::BudgetedText::new(
                    &scope.budget,
                    memory_budget::Category::Execution,
                    u64::MAX,
                )?;
                text.try_push_str(value)?;
                let mut divisor = 1;
                while number / divisor >= 10 {
                    divisor *= 10;
                }
                loop {
                    text.try_push(char::from(b'0' + ((number / divisor) % 10) as u8))?;
                    if divisor == 1 {
                        break;
                    }
                    divisor /= 10;
                }
                Ok(Self::Managed(text))
            })()),
        }
    }
    pub(crate) fn parameter(scope: Option<&CompileArena>, name: &str) -> crate::sema::R<Self> {
        match scope {
            None => Ok(Self::Legacy(name.to_lowercase())),
            Some(scope) => scope.compile((|| {
                let mut key = memory_budget::BudgetedText::new(
                    &scope.budget,
                    memory_budget::Category::Execution,
                    u64::MAX,
                )?;
                for ch in bsl_prim::lowercase(name) {
                    key.try_push(ch)?;
                }
                Ok(Self::Managed(key))
            })()),
        }
    }

    pub(crate) fn joined<'a>(
        scope: Option<&CompileArena>,
        parts: impl Iterator<Item = &'a str>,
    ) -> crate::sema::R<Self> {
        match scope {
            None => Ok(Self::Legacy(bsl_prim::fold_name(
                &parts.collect::<Vec<_>>().join("."),
            ))),
            Some(scope) => scope.compile((|| {
                let mut key = memory_budget::BudgetedText::new(
                    &scope.budget,
                    memory_budget::Category::Execution,
                    u64::MAX,
                )?;
                for (i, part) in parts.enumerate() {
                    if i != 0 {
                        key.try_push('.')?;
                    }
                    for c in part.chars() {
                        let mut lower = c.to_lowercase();
                        key.try_push(match (lower.next(), lower.next()) {
                            (Some(c), None) => c,
                            _ => c,
                        })?;
                    }
                }
                Ok(Self::Managed(key))
            })()),
        }
    }
    pub(crate) fn folded(scope: Option<&CompileArena>, value: &str) -> crate::sema::R<Self> {
        Self::joined(scope, std::iter::once(value))
    }
    pub(crate) fn push_str(
        &mut self,
        scope: Option<&CompileArena>,
        value: &str,
    ) -> crate::sema::R<()> {
        match self {
            Self::Legacy(text) => {
                text.push_str(value);
                Ok(())
            }
            Self::Managed(text) => {
                let result = text.try_push_str(value);
                match scope {
                    Some(scope) => scope.compile(result),
                    None => result.map_err(|_| {
                        query_syntax::QueryError::new(
                            query_syntax::MsgId::SELECT_ERROR_SYNTAXERROR,
                            None,
                        )
                    }),
                }
            }
        }
    }
    pub(crate) fn as_str(&self) -> &str {
        match self {
            Self::Legacy(value) => value,
            Self::Managed(value) => value.as_str(),
        }
    }
}
impl std::ops::Deref for NameKey {
    type Target = str;
    fn deref(&self) -> &str {
        self.as_str()
    }
}
impl std::borrow::Borrow<str> for NameKey {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}
impl PartialEq for NameKey {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}
impl Eq for NameKey {}
impl std::hash::Hash for NameKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::hash::Hash::hash(self.as_str(), state);
    }
}

pub(crate) enum ScratchVec<T> {
    Legacy(Vec<T>),
    Managed(memory_budget::BudgetedVec<T>),
}
impl<T> ScratchVec<T> {
    pub(crate) fn new(scope: Option<&CompileArena>, capacity: usize) -> crate::sema::R<Self> {
        match scope {
            None => Ok(Self::Legacy(Vec::with_capacity(capacity))),
            Some(scope) => scope.compile(
                memory_budget::BudgetedVec::try_with_capacity(
                    &scope.budget,
                    memory_budget::Category::Execution,
                    capacity,
                )
                .map(Self::Managed),
            ),
        }
    }
    pub(crate) fn clear(&mut self) {
        match self {
            Self::Legacy(values) => values.clear(),
            Self::Managed(values) => values.clear(),
        }
    }
    pub(crate) fn extend(
        &mut self,
        scope: Option<&CompileArena>,
        values: impl IntoIterator<Item = T>,
    ) -> crate::sema::R<()> {
        for value in values {
            self.push(scope, value)?;
        }
        Ok(())
    }
    pub(crate) fn pop(&mut self) -> Option<T> {
        match self {
            Self::Legacy(values) => values.pop(),
            Self::Managed(values) => values.pop(),
        }
    }
    pub(crate) fn push(&mut self, scope: Option<&CompileArena>, value: T) -> crate::sema::R<()> {
        match self {
            Self::Legacy(values) => {
                values.push(value);
                Ok(())
            }
            Self::Managed(values) => {
                let result = values.try_push(value);
                match scope {
                    Some(scope) => scope.compile(result),
                    None => result.map_err(|_| {
                        query_syntax::QueryError::new(
                            query_syntax::MsgId::SELECT_ERROR_SYNTAXERROR,
                            None,
                        )
                    }),
                }
            }
        }
    }
}
impl<T> Default for ScratchVec<T> {
    fn default() -> Self {
        Self::Legacy(Vec::new())
    }
}
impl<T> std::ops::Deref for ScratchVec<T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        match self {
            Self::Legacy(values) => values,
            Self::Managed(values) => values.as_slice(),
        }
    }
}
impl<T> std::ops::DerefMut for ScratchVec<T> {
    fn deref_mut(&mut self) -> &mut [T] {
        match self {
            Self::Legacy(values) => values,
            Self::Managed(values) => values.as_mut_slice(),
        }
    }
}
impl<'a, T> IntoIterator for &'a ScratchVec<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
pub(crate) fn scratch_collect<T>(
    scope: Option<&CompileArena>,
    values: impl Iterator<Item = crate::sema::R<T>>,
) -> crate::sema::R<ScratchVec<T>> {
    let mut output = ScratchVec::new(scope, values.size_hint().0)?;
    for value in values {
        output.push(scope, value?)?;
    }
    Ok(output)
}

pub(crate) fn reference(
    scope: Option<&CompileArena>,
    atom: crate::RefAtom,
) -> crate::sema::R<crate::QType> {
    Ok(crate::QType {
        refs: list(scope, [atom])?,
        ..crate::QType::default()
    })
}
pub(crate) fn other(
    scope: Option<&CompileArena>,
    atom: crate::Other,
) -> crate::sema::R<crate::QType> {
    Ok(crate::QType {
        others: list(scope, [atom])?,
        ..crate::QType::default()
    })
}

pub(crate) enum Utf16Buffer<'a> {
    Legacy(Vec<u16>),
    Managed {
        values: memory_budget::BudgetedVec<u16>,
        scope: &'a CompileArena,
    },
}
impl<'a> Utf16Buffer<'a> {
    pub(crate) fn new(scope: Option<&'a CompileArena>, initial: &[u16]) -> crate::sema::R<Self> {
        match scope {
            None => Ok(Self::Legacy(initial.to_vec())),
            Some(scope) => scope.compile((|| {
                let mut values = memory_budget::BudgetedVec::try_with_capacity(
                    &scope.budget,
                    memory_budget::Category::Execution,
                    initial.len(),
                )?;
                values.try_extend_from_slice(initial)?;
                Ok(Self::Managed { values, scope })
            })()),
        }
    }
    pub(crate) fn extend(&mut self, units: &[u16]) -> crate::sema::R<()> {
        match self {
            Self::Legacy(values) => {
                values.extend_from_slice(units);
                Ok(())
            }
            Self::Managed { values, scope } => scope.compile(values.try_extend_from_slice(units)),
        }
    }
    pub(crate) fn finish(self) -> crate::sema::R<crate::QStr> {
        match self {
            Self::Legacy(values) => Ok(crate::QStr(values.into())),
            Self::Managed { values, scope } => {
                scope.compile(crate::QStr::try_from_budgeted_units(values))
            }
        }
    }
}

pub(crate) struct Index<K, V> {
    slots: ScratchVec<Option<(u64, K, V)>>,
    len: usize,
    hash: std::collections::hash_map::RandomState,
}
impl<K: std::hash::Hash + Eq, V> Default for Index<K, V> {
    fn default() -> Self {
        Self::new()
    }
}
impl<K: std::hash::Hash + Eq, V> Index<K, V> {
    pub(crate) fn new() -> Self {
        Self {
            slots: ScratchVec::Legacy(Vec::new()),
            len: 0,
            hash: std::collections::hash_map::RandomState::new(),
        }
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }
    fn hash<Q: std::hash::Hash + ?Sized>(&self, key: &Q) -> u64 {
        use std::hash::BuildHasher;
        self.hash.hash_one(key)
    }
    fn position<Q: std::hash::Hash + Eq + ?Sized>(&self, hash: u64, key: &Q) -> Option<usize>
    where
        K: std::borrow::Borrow<Q>,
    {
        if self.slots.is_empty() {
            return None;
        }
        let mut at = (hash as usize) & (self.slots.len() - 1);
        loop {
            match self.slots.get(at)? {
                Some((found, stored, _)) if *found == hash && stored.borrow() == key => {
                    return Some(at);
                }
                Some(_) => at = (at + 1) & (self.slots.len() - 1),
                None => return None,
            }
        }
    }
    pub(crate) fn get<Q: std::hash::Hash + Eq + ?Sized>(&self, key: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
    {
        let at = self.position(self.hash(key), key)?;
        self.slots.get(at)?.as_ref().map(|(_, _, value)| value)
    }
    pub(crate) fn get_mut<Q: std::hash::Hash + Eq + ?Sized>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: std::borrow::Borrow<Q>,
    {
        let at = self.position(self.hash(key), key)?;
        self.slots.get_mut(at)?.as_mut().map(|(_, _, value)| value)
    }
    pub(crate) fn insert(
        &mut self,
        scope: Option<&CompileArena>,
        key: K,
        value: V,
    ) -> crate::sema::R<Option<V>> {
        let hash = self.hash(&key);
        if let Some(at) = self.position(hash, &key) {
            if let Some(Some((_, _, stored))) = self.slots.get_mut(at) {
                return Ok(Some(std::mem::replace(stored, value)));
            }
            return allocation_error(scope, BudgetError::Overflow);
        }
        if self.len >= self.slots.len() / 2 {
            let capacity = self.slots.len().checked_mul(2).map(|n| n.max(8));
            let capacity = match (scope, capacity) {
                (_, Some(n)) => n,
                (Some(scope), None) => return scope.compile(Err(BudgetError::Overflow)),
                (None, None) => return allocation_error(None, BudgetError::Overflow),
            };
            let mut next = ScratchVec::new(scope, capacity)?;
            for _ in 0..capacity {
                next.push(scope, None)?;
            }
            for entry in self.slots.iter_mut() {
                if let Some(entry) = entry.take()
                    && !Self::place(&mut next, entry)
                {
                    return allocation_error(scope, BudgetError::Overflow);
                }
            }
            self.slots = next;
        }
        if !Self::place(&mut self.slots, (hash, key, value)) {
            return allocation_error(scope, BudgetError::Overflow);
        }
        self.len += 1;
        Ok(None)
    }
    fn place(slots: &mut [Option<(u64, K, V)>], entry: (u64, K, V)) -> bool {
        let mut entry = Some(entry);
        let Some(mask) = slots.len().checked_sub(1) else {
            return false;
        };
        let mut at = entry.as_ref().map_or(0, |entry| entry.0 as usize) & mask;
        for _ in 0..slots.len() {
            let Some(slot) = slots.get_mut(at) else {
                return false;
            };
            if slot.is_none() {
                *slot = entry.take();
                return true;
            }
            at = (at + 1) & mask;
        }
        false
    }
}

pub(crate) fn allocation_result<T>(
    scope: Option<&CompileArena>,
    value: AResult<T>,
) -> crate::sema::R<T> {
    match value {
        Ok(value) => Ok(value),
        Err(error) => allocation_error(scope, error),
    }
}
pub(crate) fn allocation_error<T>(
    scope: Option<&CompileArena>,
    error: BudgetError,
) -> crate::sema::R<T> {
    match scope {
        Some(scope) => scope.compile(Err(error)),
        None => Err(query_syntax::QueryError::new(
            query_syntax::MsgId::SELECT_ERROR_SYNTAXERROR,
            None,
        )),
    }
}

pub(crate) mod ir;
mod structural;
pub(crate) use structural::ManagedClone;
