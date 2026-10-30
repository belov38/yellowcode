use super::*;
use crate::collections::accounting::{Charge, Result};
pub(in crate::collections) fn object(actual: &dyn Any, a: &mut impl Charge) -> Option<Result<()>> {
    if let Some(table) = actual.downcast_ref::<ValueTable>() {
        Some(state(&table.state, a))
    } else {
        if let Some(row) = actual.downcast_ref::<TableRow>() {
            Some((|| {
                state(&row.state, a)?;
                record(&row.record, a)
            })())
        } else {
            indexes::retained_object(actual, a)
        }
    }
}
fn state(value: &Rc<TableState>, a: &mut impl Charge) -> Result<()> {
    if !a.rc(value)? {
        return Ok(());
    }
    Columns::retained_rc(&value.columns, a)?;
    indexes::Indexes::retained_rc(&value.indexes, a)?;
    let rows = value
        .rows
        .try_borrow()
        .map_err(|_| crate::collections::RetainedChargeError::Borrowed)?;
    a.vector(&rows)?;
    for row in rows.iter() {
        a.work(1)?;
        record(row, a)?;
    }
    Ok(())
}
fn record(value: &Rc<RowRecord>, a: &mut impl Charge) -> Result<()> {
    if !a.rc(value)? {
        return Ok(());
    }
    let cells = value
        .cells
        .try_borrow()
        .map_err(|_| crate::collections::RetainedChargeError::Borrowed)?;
    a.vector(&cells.0)?;
    for (_, value) in &cells.0 {
        a.value(value)?;
    }
    Ok(())
}
