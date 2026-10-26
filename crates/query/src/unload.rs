use crate::{columns, errors, result_obj::ResultData};
use bsl_vm::{Arg, CallResult, Execution, Value};
use query_exec::{
    totals::RowRef,
    walk::{self, TreeRow, Unloaded, Walk},
};

pub(crate) fn unload(ex: &mut Execution, data: &ResultData, walk: Walk) -> CallResult<Value> {
    let count = data
        .result
        .totals
        .as_ref()
        .map_or(data.result.rows.len(), |t| t.records.len());
    let payload = data
        .result
        .rows
        .iter()
        .chain(data.result.totals.iter().flat_map(|t| t.totals.iter()))
        .fold(0u64, |n, row| {
            n.saturating_add(query_exec::ops::row_bytes(row))
        });
    ex.reserve(
        payload.saturating_add(
            (count as u64).saturating_mul(
                256u64.saturating_add((data.names.len() as u64).saturating_mul(128)),
            ),
        ),
    )?;
    let unloaded = walk::unload(&data.result, walk).map_err(|e| errors::walk_error(ex, e))?;
    let out = ex.construct(
        columns::idx(if matches!(unloaded, Unloaded::Table(_)) {
            "ValueTable"
        } else {
            "ValueTree"
        })?,
        &mut [],
    )?;
    add_columns(ex, data, &out)?;
    match unloaded {
        Unloaded::Table(rows) => {
            for source in rows {
                ex.charge(1)?;
                let row = ex.call_method(&out, "Add", &mut [], true)?;
                fill(ex, data, &row, source)?;
            }
        }
        Unloaded::Tree(rows) => {
            let roots = ex.get_member(&out, "Rows")?;
            let mut stack: Vec<(&[TreeRow], usize, Value)> = vec![(&rows, 0, roots)];
            while let Some((nodes, i, destination)) = stack.last_mut() {
                ex.charge(1)?;
                let Some(node) = nodes.get(*i) else {
                    stack.pop();
                    continue;
                };
                *i += 1;
                let row = ex.call_method(destination, "Add", &mut [], true)?;
                fill(ex, data, &row, node.row)?;
                if !node.children.is_empty() {
                    let children = ex.get_member(&row, "Rows")?;
                    stack.push((&node.children, 0, children));
                }
            }
        }
    }
    Ok(out)
}
fn add_columns(ex: &mut Execution, data: &ResultData, out: &Value) -> CallResult<()> {
    let cols = ex.get_member(out, "Columns")?;
    for (i, name) in data.names.iter().enumerate() {
        let col = ex.index_get(&data.columns, &Value::from(i as i64))?;
        let mut td = ex.get_member(&col, "ValueType")?;
        if data.type_code_columns.get(i) == Some(&true) {
            let d = bsl_stdlib::type_description_of(&td)
                .ok_or_else(|| errors::internal("query column type description"))?;
            td = bsl_stdlib::new_type_description(bsl_stdlib::TypeDescription::from_type_array(
                d.types(),
                Some(d.number()),
                Some(d.string()),
                Some(d.date()),
                Some(d.binary()),
                ex.runtime(),
            ));
        }
        ex.call_method(
            &cols,
            "Add",
            &mut [Arg::Val(Value::from(name.as_str())), Arg::Val(td)],
            true,
        )?;
    }
    Ok(())
}
fn fill(ex: &mut Execution, data: &ResultData, row: &Value, source: RowRef) -> CallResult<()> {
    for i in 0..data.names.len() {
        let value = data.cell(source, i)?;
        let value = if data.type_code_columns.get(i) == Some(&true)
            && bsl_stdlib::values::binary_data_bytes(&value).is_some()
        {
            Value::Undefined
        } else {
            value
        };
        ex.index_set(row, &Value::from(i as i64), value)?;
    }
    Ok(())
}
