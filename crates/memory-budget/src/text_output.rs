use crate::{BudgetError, BudgetedText, Category, LimitReason, MemoryLimit};
use std::fmt;

pub trait TextOutput: fmt::Write {
    fn try_push_str(&mut self, text: &str) -> Result<(), BudgetError>;
    fn try_push(&mut self, value: char) -> Result<(), BudgetError> {
        self.try_push_str(value.encode_utf8(&mut [0; 4]))
    }
    fn error(&self) -> Option<&BudgetError>;
}
impl TextOutput for BudgetedText {
    fn try_push_str(&mut self, text: &str) -> Result<(), BudgetError> {
        BudgetedText::try_push_str(self, text)
    }
    fn error(&self) -> Option<&BudgetError> {
        BudgetedText::error(self)
    }
}

#[derive(Debug)]
pub struct TextCount {
    len: u64,
    limit: u64,
    category: Category,
    error: Option<BudgetError>,
}
impl TextCount {
    pub fn new(category: Category, limit: u64) -> Self {
        Self {
            len: 0,
            limit,
            category,
            error: None,
        }
    }
    pub fn len(&self) -> u64 {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn error(&self) -> Option<&BudgetError> {
        self.error.as_ref()
    }
    pub fn try_push_str(&mut self, text: &str) -> Result<(), BudgetError> {
        if let Some(error) = self.error {
            return Err(error);
        }
        let result = (|| {
            let bytes = u64::try_from(text.len()).map_err(|_| BudgetError::Overflow)?;
            let next = self.len.checked_add(bytes).ok_or(BudgetError::Overflow)?;
            if next > self.limit {
                return Err(BudgetError::Limit(MemoryLimit {
                    category: self.category,
                    requested: next,
                    reason: LimitReason::Category,
                }));
            }
            self.len = next;
            Ok(())
        })();
        if let Err(error) = result {
            self.error = Some(error);
        }
        result
    }
    pub fn try_push(&mut self, value: char) -> Result<(), BudgetError> {
        self.try_push_str(value.encode_utf8(&mut [0; 4]))
    }
}
impl TextOutput for TextCount {
    fn try_push_str(&mut self, text: &str) -> Result<(), BudgetError> {
        TextCount::try_push_str(self, text)
    }
    fn error(&self) -> Option<&BudgetError> {
        TextCount::error(self)
    }
}
impl fmt::Write for TextCount {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.try_push_str(text).map_err(|_| fmt::Error)
    }
    fn write_char(&mut self, value: char) -> fmt::Result {
        self.try_push(value).map_err(|_| fmt::Error)
    }
}
