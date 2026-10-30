use super::*;
use crate::collections::accounting::{Charge, Result};
pub(in crate::collections) fn object(actual: &dyn Any, a: &mut impl Charge) -> Option<Result<()>> {
    if let Some(tree) = actual.downcast_ref::<ValueTree>() {
        Some(state(&tree.state, a))
    } else if let Some(row) = actual.downcast_ref::<TreeRow>() {
        Some((|| {
            state(&row.state, a)?;
            node(&row.record, a)
        })())
    } else {
        actual.downcast_ref::<TreeRows>().map(|rows| {
            (|| {
                state(&rows.state, a)?;
                if let Some(record) = &rows.parent {
                    node(record, a)?;
                }
                Ok(())
            })()
        })
    }
}
fn state(value: &Rc<TreeState>, a: &mut impl Charge) -> Result<()> {
    if !a.rc(value)? {
        return Ok(());
    }
    Columns::retained_rc(&value.columns, a)?;
    let arena = value
        .arena
        .try_borrow()
        .map_err(|_| crate::collections::RetainedChargeError::Borrowed)?;
    a.vector(&arena)?;
    a.vector(
        &*value
            .roots
            .try_borrow()
            .map_err(|_| crate::collections::RetainedChargeError::Borrowed)?,
    )?;
    for record in arena.iter() {
        a.work(1)?;
        if let Some(record) = record {
            node(record, a)?;
        }
    }
    Ok(())
}
fn node(value: &Rc<Node>, a: &mut impl Charge) -> Result<()> {
    if !a.rc(value)? {
        return Ok(());
    }
    a.vector(
        &*value
            .children
            .try_borrow()
            .map_err(|_| crate::collections::RetainedChargeError::Borrowed)?,
    )?;
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
