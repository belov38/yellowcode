use bsl_prim::{BslDate, Number, RoundMode};
use memory_budget::BudgetError;
use query_ir::{
    DateQ, Other, QType,
    managed_value::{ManagedValue, NumberValueError, ScalarValue, ValueRef, row::ScopedRow},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CastError {
    Memory(BudgetError),
    FieldOverflow,
    Column { index: usize, columns: usize },
}
impl From<BudgetError> for CastError {
    fn from(error: BudgetError) -> Self {
        Self::Memory(error)
    }
}
impl std::fmt::Display for CastError {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Memory(error) => error.fmt(out),
            Self::FieldOverflow => out.write_str("DBERROR_SQLFIELDOVERFLOW"),
            Self::Column { index, columns } => {
                write!(out, "column {index} outside {columns} columns")
            }
        }
    }
}
impl std::error::Error for CastError {}

pub(super) fn integer_fits(number: &Number, limit: usize) -> bool {
    struct Width {
        significant: usize,
        limit: usize,
        overflow: bool,
    }
    impl std::fmt::Write for Width {
        fn write_str(&mut self, text: &str) -> std::fmt::Result {
            for byte in text.bytes() {
                match byte {
                    b'-' => {}
                    b'.' => return Err(std::fmt::Error),
                    b'0' if self.significant == 0 => {}
                    _ => {
                        self.significant += 1;
                        if self.significant > self.limit {
                            self.overflow = true;
                            return Err(std::fmt::Error);
                        }
                    }
                }
            }
            Ok(())
        }
    }
    let mut width = Width {
        significant: 0,
        limit,
        overflow: false,
    };
    let _ = number.write_xml(&mut width);
    !width.overflow
}

pub fn try_cast(
    source: &ScopedRow,
    index: usize,
    to: &QType,
    ref_tables: &[u32],
) -> Result<ManagedValue, CastError> {
    let value = source.values().get(index).ok_or(CastError::Column {
        index,
        columns: source.values().len(),
    })?;
    let null = || ManagedValue::scalar(ScalarValue::Null);
    Ok(match value.view() {
        ValueRef::Number(n) => match to.number {
            None => null(),
            Some(q) if q.digits == 0 => value.clone(),
            Some(q) => {
                let rounded = ManagedValue::try_number_round(
                    n,
                    i64::from(q.scale),
                    RoundMode::HalfAwayFromZero,
                    source.budget(),
                )
                .map_err(|e| match e {
                    NumberValueError::Budget(e) => CastError::Memory(e),
                    NumberValueError::DivByZero => CastError::Memory(BudgetError::Overflow),
                })?;
                if let ValueRef::Number(n) = rounded.view()
                    && !integer_fits(n, usize::from(q.digits.saturating_sub(q.scale)))
                {
                    return Err(CastError::FieldOverflow);
                }
                rounded
            }
        },
        ValueRef::String(text) => match to.string {
            None => null(),
            Some(q)
                if q.len == 0
                    || u64::try_from(text.len()).map_err(|_| BudgetError::Overflow)?
                        <= u64::from(q.len) =>
            {
                value.clone()
            }
            Some(q) => {
                let end = usize::try_from(q.len).map_err(|_| BudgetError::Overflow)?;
                let cut = text.get(..end).ok_or(BudgetError::Overflow)?;
                let cut = if cut.last().is_some_and(|x| (0xD800..0xDC00).contains(x)) {
                    cut.get(..cut.len() - 1).ok_or(BudgetError::Overflow)?
                } else {
                    cut
                };
                ManagedValue::try_utf16(cut, source.budget())?
            }
        },
        ValueRef::Date(date) => match to.date {
            None => null(),
            Some(DateQ::DateTime) => value.clone(),
            Some(q) => {
                let day = 86_400 * BslDate::TICKS_PER_SECOND;
                let ticks = if q == DateQ::Date {
                    date.ticks() - date.ticks() % day
                } else {
                    date.ticks() % day
                };
                ManagedValue::scalar(ScalarValue::Date(
                    BslDate::from_ticks(ticks).unwrap_or(date),
                ))
            }
        },
        ValueRef::Bool(_) if to.boolean => value.clone(),
        ValueRef::Undefined if to.is_undefined() => value.clone(),
        ValueRef::Ref { table, .. } if ref_tables.contains(&table) => value.clone(),
        ValueRef::Uuid(_) if to.others.contains(&Other::Uuid) => value.clone(),
        ValueRef::Binary(_) if to.others.contains(&Other::Binary) => value.clone(),
        ValueRef::Storage(_) if to.others.contains(&Other::Storage) => value.clone(),
        ValueRef::Type(_) if to.others.contains(&Other::Type) => value.clone(),
        ValueRef::SysEnum(t, _) if to.others.contains(&Other::SysEnum(t)) => value.clone(),
        _ => null(),
    })
}
