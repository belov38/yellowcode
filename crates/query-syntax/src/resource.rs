use crate::{MsgArg, MsgId, SyntaxError};
use memory_budget::{BudgetError, Category, MemoryBudget, Reservation};
use std::{
    alloc::{Layout, alloc},
    sync::OnceLock,
};

pub(crate) struct ParseScope {
    pub charge: Reservation,
    error: OnceLock<BudgetError>,
}
impl ParseScope {
    pub fn new(budget: &MemoryBudget) -> Result<Self, BudgetError> {
        Ok(Self {
            charge: budget.try_reserve(Category::Execution, 0)?,
            error: OnceLock::new(),
        })
    }
    pub fn error(&self) -> Option<BudgetError> {
        self.error.get().copied()
    }
    fn fail(&self, error: BudgetError) -> SyntaxError {
        match self.error.set(error) {
            Ok(()) | Err(_) => placeholder(),
        }
    }
    fn reserve(&self, layout: Layout) -> Result<(), SyntaxError> {
        if self.error().is_some() {
            return Err(placeholder());
        }
        self.charge
            .try_grow(layout.size() as u64)
            .map_err(|e| self.fail(e.into()))
    }
}
fn placeholder() -> SyntaxError {
    SyntaxError::new(MsgId::SELECT_ERROR_SYNTAXERROR, None)
}
fn vector<T>(scope: &ParseScope, capacity: usize) -> Result<Vec<T>, SyntaxError> {
    unsafe { vector_with(scope, capacity, alloc) }
}
unsafe fn vector_with<T>(
    scope: &ParseScope,
    capacity: usize,
    allocate: unsafe fn(Layout) -> *mut u8,
) -> Result<Vec<T>, SyntaxError> {
    let layout = Layout::array::<T>(capacity).map_err(|_| scope.fail(BudgetError::Overflow))?;
    scope.reserve(layout)?;
    if layout.size() == 0 {
        return Ok(Vec::new());
    }
    let data = unsafe { allocate(layout) }.cast::<T>();
    if data.is_null() {
        return Err(scope.fail(BudgetError::AllocationFailed));
    }
    Ok(unsafe { Vec::from_raw_parts(data, 0, capacity) })
}
pub(crate) fn push<T>(
    scope: Option<&ParseScope>,
    values: &mut Vec<T>,
    value: T,
) -> Result<(), SyntaxError> {
    if let Some(scope) = scope
        && values.len() == values.capacity()
    {
        let capacity = values
            .len()
            .checked_add(1)
            .and_then(|n| values.capacity().checked_mul(2).map(|cap| cap.max(n)))
            .ok_or_else(|| scope.fail(BudgetError::Overflow))?;
        let mut next = vector(scope, capacity)?;
        next.append(values);
        *values = next;
    }
    values.push(value);
    Ok(())
}
pub(crate) fn list<T, const N: usize>(
    scope: Option<&ParseScope>,
    values: [T; N],
) -> Result<Vec<T>, SyntaxError> {
    let mut out = match scope {
        Some(scope) => vector(scope, N)?,
        None => Vec::with_capacity(N),
    };
    out.extend(values);
    Ok(out)
}
pub(crate) fn boxed<T>(scope: Option<&ParseScope>, value: T) -> Result<Box<T>, SyntaxError> {
    let Some(scope) = scope else {
        return Ok(Box::new(value));
    };
    let layout = Layout::new::<T>();
    scope.reserve(layout)?;
    if layout.size() == 0 {
        return Ok(Box::new(value));
    }
    let data = unsafe { alloc(layout) }.cast::<T>();
    if data.is_null() {
        return Err(scope.fail(BudgetError::AllocationFailed));
    }
    unsafe {
        data.write(value);
        Ok(Box::from_raw(data))
    }
}
pub(crate) fn string(scope: Option<&ParseScope>, text: &str) -> Result<String, SyntaxError> {
    let Some(scope) = scope else {
        return Ok(text.to_string());
    };
    let mut bytes = vector::<u8>(scope, text.len())?;
    bytes.extend_from_slice(text.as_bytes());
    Ok(unsafe { String::from_utf8_unchecked(bytes) })
}
pub(crate) fn text(scope: Option<&ParseScope>, text: &str) -> Result<Box<str>, SyntaxError> {
    let Some(scope) = scope else {
        return Ok(text.into());
    };
    if text.is_empty() {
        return Ok("".into());
    }
    let layout = Layout::array::<u8>(text.len()).map_err(|_| scope.fail(BudgetError::Overflow))?;
    scope.reserve(layout)?;
    let data = unsafe { alloc(layout) };
    if data.is_null() {
        return Err(scope.fail(BudgetError::AllocationFailed));
    }
    unsafe {
        std::ptr::copy_nonoverlapping(text.as_ptr(), data, text.len());
        Ok(Box::from_raw(
            std::ptr::slice_from_raw_parts_mut(data, text.len()) as *mut str,
        ))
    }
}
pub(crate) fn decoded(scope: Option<&ParseScope>, token: &str) -> Result<String, SyntaxError> {
    if scope.is_none() {
        return Ok(crate::lexer::string_value(token));
    }
    let inner = token.strip_prefix('"').unwrap_or(token);
    let inner = inner.strip_suffix('"').unwrap_or(inner);
    let mut bytes = string(scope, inner)?.into_bytes();
    let (mut read, mut write) = (0, 0);
    while let Some(&b) = bytes.get(read) {
        if b == b'"' && bytes.get(read + 1) == Some(&b'"') {
            read += 1;
        }
        if let Some(slot) = bytes.get_mut(write) {
            *slot = b;
        }
        read += 1;
        write += 1;
    }
    bytes.truncate(write);
    Ok(unsafe { String::from_utf8_unchecked(bytes) })
}
pub(crate) fn decoded_text(
    scope: Option<&ParseScope>,
    token: &str,
) -> Result<Box<str>, SyntaxError> {
    let value = decoded(scope, token)?;
    text(scope, &value)
}
pub(crate) fn number(scope: Option<&ParseScope>, token: &str) -> Result<Box<str>, SyntaxError> {
    let (int, frac) = crate::lexer::split_number(token);
    if frac.is_empty() {
        return text(scope, int);
    }
    let mut out = match scope {
        Some(scope) => {
            let capacity = int
                .len()
                .checked_add(frac.len())
                .and_then(|n| n.checked_add(1))
                .ok_or_else(|| scope.fail(BudgetError::Overflow))?;
            let bytes = vector::<u8>(scope, capacity)?;
            unsafe { String::from_utf8_unchecked(bytes) }
        }
        None => String::new(),
    };
    out.push_str(int);
    out.push('.');
    out.push_str(frac);
    text(scope, &out)
}
pub(crate) fn arg(scope: Option<&ParseScope>, value: &str) -> MsgArg {
    match text(scope, value) {
        Ok(value) => MsgArg::Text(value),
        Err(_) => MsgArg::Kw(crate::Kw::Select),
    }
}

pub(crate) enum AliasScope {
    Legacy(std::collections::HashSet<String>),
    Budgeted {
        slots: Vec<Option<String>>,
        len: usize,
    },
}
impl AliasScope {
    pub fn new(scope: Option<&ParseScope>) -> Self {
        if scope.is_some() {
            Self::Budgeted {
                slots: Vec::new(),
                len: 0,
            }
        } else {
            Self::Legacy(Default::default())
        }
    }
    pub fn insert(
        &mut self,
        scope: Option<&ParseScope>,
        value: String,
    ) -> Result<bool, SyntaxError> {
        match self {
            Self::Legacy(values) => Ok(values.insert(value)),
            Self::Budgeted { slots, len } => {
                if slots.is_empty() || *len >= slots.len() / 2 {
                    let capacity = slots
                        .len()
                        .checked_mul(2)
                        .ok_or_else(|| match scope {
                            Some(scope) => scope.fail(BudgetError::Overflow),
                            None => placeholder(),
                        })?
                        .max(8);
                    let mut next = match scope {
                        Some(scope) => vector(scope, capacity)?,
                        None => return Err(placeholder()),
                    };
                    for _ in 0..capacity {
                        next.push(None)
                    }
                    for item in slots.iter_mut().filter_map(Option::take) {
                        insert_slot(&mut next, item);
                    }
                    *slots = next;
                }
                let inserted = insert_slot(slots, value);
                if inserted {
                    *len += 1
                }
                Ok(inserted)
            }
        }
    }
}
fn insert_slot(slots: &mut [Option<String>], value: String) -> bool {
    let hash = value.bytes().fold(14695981039346656037u64, |hash, b| {
        (hash ^ u64::from(b)).wrapping_mul(1099511628211)
    });
    let mut slot = (hash as usize) & (slots.len() - 1);
    loop {
        match slots.get_mut(slot) {
            Some(entry @ None) => {
                *entry = Some(value);
                return true;
            }
            Some(Some(old)) if old == &value => return false,
            _ => slot = (slot + 1) & (slots.len() - 1),
        }
    }
}
pub(crate) fn alias_key(scope: Option<&ParseScope>, name: &str) -> Result<String, SyntaxError> {
    let Some(scope) = scope else {
        return Ok(name.chars().flat_map(char::to_uppercase).collect());
    };
    let capacity = name
        .len()
        .checked_mul(3)
        .ok_or_else(|| scope.fail(BudgetError::Overflow))?;
    let mut bytes = vector::<u8>(scope, capacity)?;
    for c in name.chars().flat_map(char::to_uppercase) {
        let mut scratch = [0; 4];
        bytes.extend_from_slice(c.encode_utf8(&mut scratch).as_bytes());
    }
    Ok(unsafe { String::from_utf8_unchecked(bytes) })
}
