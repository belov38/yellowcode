use super::*;
use crate::collections::accounting::{Charge, Result};
pub(in crate::collections) fn object(actual: &dyn Any, a: &mut impl Charge) -> Option<Result<()>> {
    if let Some(columns) = actual.downcast_ref::<Columns>() {
        Some(columns.retained(a))
    } else {
        actual.downcast_ref::<Column>().map(|column| {
            (|| {
                schema(&column.schema, a)?;
                ColumnRecord::retained_rc(&column.record, a)
            })()
        })
    }
}
fn schema(value: &Rc<Schema>, a: &mut impl Charge) -> Result<()> {
    if !a.rc(value)? {
        return Ok(());
    }
    let rows = value
        .rows
        .try_borrow()
        .map_err(|_| crate::collections::RetainedChargeError::Borrowed)?;
    if let Some(owner) = &*rows {
        a.block(
            0,
            owner.as_ptr() as *const () as usize,
            value.rows_allocation.get(),
        )?;
    }
    let indexes = value
        .indexes
        .try_borrow()
        .map_err(|_| crate::collections::RetainedChargeError::Borrowed)?;
    if let Some(owner) = &*indexes {
        a.block(
            0,
            owner.as_ptr() as *const () as usize,
            value.indexes_allocation.get(),
        )?;
    }
    let entries = value
        .entries
        .try_borrow()
        .map_err(|_| crate::collections::RetainedChargeError::Borrowed)?;
    a.vector(&entries)?;
    for record in entries.iter() {
        ColumnRecord::retained_rc(record, a)?;
    }
    Ok(())
}
impl Columns {
    pub(in crate::collections) fn retained_rc(value: &Rc<Self>, a: &mut impl Charge) -> Result<()> {
        a.shared_object(value)
    }
    fn retained(&self, a: &mut impl Charge) -> Result<()> {
        schema(&self.schema, a)
    }
}
impl ColumnRecord {
    pub(in crate::collections) fn retained_rc(value: &Rc<Self>, a: &mut impl Charge) -> Result<()> {
        if !a.rc(value)? {
            return Ok(());
        }
        let data = value
            .data
            .try_borrow()
            .map_err(|_| crate::collections::RetainedChargeError::Borrowed)?;
        a.text(&data.name)?;
        a.text(&data.title)?;
        a.string(&data.folded)?;
        a.value(&data.value_type)
    }
}
