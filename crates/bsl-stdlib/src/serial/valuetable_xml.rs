use crate::collections::ValueTable;
use crate::typedesc::TypeDescription;
use crate::xdto::{XdtoCtx, XdtoData};
use bsl_lib::xdto::QName;
use bsl_vm::{Exception, Execution, ObjRef, Value};

const CORE: &str = "http://v8.1c.ru/8.1/data/core";

type Field = (String, Vec<XdtoData>);

fn vector<T>(ex: &mut Execution, ctx: &mut XdtoCtx, count: usize) -> Result<Vec<T>, Exception> {
    ex.charge(count as u64)?;
    let bytes = count
        .checked_mul(std::mem::size_of::<T>())
        .and_then(|n| u64::try_from(n).ok())
        .ok_or_else(|| ex.out_of_memory())?;
    ctx.account(ex, bytes)?;
    let mut v = Vec::new();
    v.try_reserve_exact(count).map_err(|_| ex.out_of_memory())?;
    Ok(v)
}
fn field(
    ex: &mut Execution,
    ctx: &mut XdtoCtx,
    name: &str,
    data: XdtoData,
) -> Result<Field, Exception> {
    let mut values = vector(ex, ctx, 1)?;
    values.push(data);
    Ok((name.into(), values))
}
fn object(name: &str, props: Vec<Field>) -> XdtoData {
    XdtoData::Object {
        ty: QName::new(CORE, name),
        props,
    }
}
fn admit_value(ex: &mut Execution, ctx: &mut XdtoCtx, value: &Value) -> Result<(), Exception> {
    let bytes = match value {
        Value::String(s) => {
            let n = (s.backing_capacity_units() as u64)
                .saturating_mul(2)
                .saturating_add((s.len() as u64).saturating_mul(128));
            ctx.account(ex, n.saturating_add(2048))?;
            let invalid = bsl_lib::xml::xml_invalid_pos(s.units(), false);
            if invalid != 0 {
                return Err(ctx.fail(crate::xml::bad_char_error(
                    invalid as usize - 1,
                    &crate::xml::shown_text(s.units()),
                )));
            }
            return Ok(());
        }
        Value::Number(n) => (n.backing_capacity_limbs() as u64)
            .saturating_mul(4)
            .saturating_add(n.size_hint_limbs().saturating_mul(64)),
        _ => 0,
    };
    ctx.account(ex, bytes.saturating_add(2048))
}
fn admit_data(ex: &mut Execution, ctx: &mut XdtoCtx, data: &XdtoData) -> Result<(), Exception> {
    ctx.account(ex, 512)?;
    let mut stack = vector(ex, ctx, 1)?;
    stack.push(data);
    while let Some(node) = stack.pop() {
        ex.charge(1)?;
        match node {
            XdtoData::Object { ty, props } => {
                ctx.account(
                    ex,
                    (ty.ns.capacity().saturating_add(ty.name.capacity()) as u64)
                        .saturating_mul(16)
                        .saturating_add((props.capacity() as u64).saturating_mul(512)),
                )?;
                ex.charge(props.len() as u64)?;
                for (name, values) in props {
                    let width = values.len();
                    ex.charge(width as u64)?;
                    let stack_bytes = width
                        .checked_mul(std::mem::size_of::<&XdtoData>())
                        .and_then(|n| u64::try_from(n).ok())
                        .ok_or_else(|| ex.out_of_memory())?;
                    ctx.account(
                        ex,
                        (name.capacity() as u64)
                            .saturating_mul(16)
                            .saturating_add((values.capacity() as u64).saturating_mul(512))
                            .saturating_add(stack_bytes),
                    )?;
                    stack
                        .try_reserve_exact(width)
                        .map_err(|_| ex.out_of_memory())?;
                    stack.extend(values);
                }
            }
            XdtoData::Value {
                ty, lexical, value, ..
            } => {
                ctx.account(
                    ex,
                    (ty.ns
                        .capacity()
                        .saturating_add(ty.name.capacity())
                        .saturating_add(lexical.capacity()) as u64)
                        .saturating_mul(32)
                        .saturating_add(1024),
                )?;
                admit_value(ex, ctx, value)?;
            }
            XdtoData::Nil => ctx.account(ex, 512)?,
        }
    }
    Ok(())
}

pub(crate) fn xml_data(
    ex: &mut Execution,
    table: &ValueTable,
    source: &ObjRef,
    ctx: &mut XdtoCtx,
) -> Result<XdtoData, Exception> {
    let bytes = table.serial_xml_snapshot_bytes(ex)?;
    ctx.account(ex, bytes.saturating_add(2048))?;
    ctx.refresh_xml_prefix(ex)?;
    let retained = ex.measure_retained(ctx.read_scratch(0), |visit| {
        visit.value(&Value::Object(source.clone()))
    })?;
    ctx.account(ex, retained)?;
    let snapshot = table.serial_xml_snapshot(ex)?;
    let held = snapshot.rows.iter().fold(snapshot.columns.len(), |n, row| {
        n.saturating_add(row.cells.len())
    });
    if let Err(e) = ctx.hold(ex, held) {
        ctx.release(held);
        return Err(e);
    }
    if let Err(e) = ctx.enter(ex, false) {
        ctx.release(held);
        return Err(e);
    }
    let result = (|| {
        let entries = snapshot
            .columns
            .len()
            .saturating_add(snapshot.rows.len())
            .saturating_add(snapshot.indexes.len());
        ctx.account(ex, (entries as u64).saturating_mul(4096))?;
        let mut columns = vector(ex, ctx, snapshot.columns.len())?;
        for col in &snapshot.columns {
            let mut props = vector(ex, ctx, 4)?;
            admit_value(ex, ctx, &Value::String(col.name.clone()))?;
            let name = ctx.to_data(ex, &Value::String(col.name.clone()))?;
            props.push(field(ex, ctx, "Name", name)?);
            let ty = ctx.to_data(ex, &col.value_type)?;
            table.serial_check_revision(ex, snapshot.revision)?;
            props.push(field(ex, ctx, "ValueType", ty)?);
            if !col.title.is_empty() {
                admit_value(ex, ctx, &Value::String(col.title.clone()))?;
                let title = ctx.to_data(ex, &Value::String(col.title.clone()))?;
                props.push(field(ex, ctx, "Title", title)?);
            }
            if col.width != 0 {
                let width = ctx.to_data(ex, &Value::from(col.width))?;
                props.push(field(ex, ctx, "Width", width)?);
            }
            columns.push(object("ValueTableColumn", props));
        }
        let mut indexes = vector(ex, ctx, snapshot.indexes.len())?;
        for ids in &snapshot.indexes {
            ex.charge((ids.len() as u64).saturating_mul(snapshot.columns.len() as u64 + 1))?;
            let mut names = vector(ex, ctx, ids.len())?;
            for id in ids {
                let col = snapshot
                    .columns
                    .iter()
                    .find(|c| c.id == *id)
                    .ok_or_else(|| ctx.mismatch(ex))?;
                admit_value(ex, ctx, &Value::String(col.name.clone()))?;
                names.push(ctx.to_data(ex, &Value::String(col.name.clone()))?);
            }
            let mut props = vector(ex, ctx, 1)?;
            props.push(("column".into(), names));
            indexes.push(object("ValueTableIndex", props));
        }
        let mut rows = vector(ex, ctx, snapshot.rows.len())?;
        for row in &snapshot.rows {
            let mut cells = vector(ex, ctx, row.cells.len())?;
            for (position, value) in row.cells.iter().enumerate() {
                let column = snapshot
                    .columns
                    .get(position)
                    .ok_or_else(|| ctx.mismatch(ex))?;
                let description = column
                    .value_type
                    .as_object()
                    .and_then(|o| o.as_any().downcast_ref::<TypeDescription>())
                    .ok_or_else(|| ctx.mismatch(ex))?;
                if let [expected] = description.types()
                    && value.type_id() != *expected
                {
                    return Err(ctx.mismatch(ex));
                }
                ctx.refresh_xml_prefix(ex)?;
                let retained = ex.measure_retained(ctx.read_scratch(0), |visit| {
                    visit.value(&Value::Object(source.clone()))?;
                    for col in &snapshot.columns {
                        visit.value(&col.value_type)?;
                    }
                    for row in &snapshot.rows {
                        for value in &row.cells {
                            visit.value(value)?;
                        }
                    }
                    Ok(())
                })?;
                table.serial_check_revision(ex, snapshot.revision)?;
                ctx.account(ex, retained)?;
                admit_value(ex, ctx, value)?;
                let data = ctx.to_data(ex, value)?;
                ctx.refresh_xml_prefix(ex)?;
                let retained = ex.measure_retained(ctx.read_scratch(0), |visit| {
                    visit.value(&Value::Object(source.clone()))?;
                    for col in &snapshot.columns {
                        visit.value(&col.value_type)?;
                    }
                    for row in &snapshot.rows {
                        for value in &row.cells {
                            visit.value(value)?;
                        }
                    }
                    Ok(())
                })?;
                table.serial_check_revision(ex, snapshot.revision)?;
                ctx.account(ex, retained)?;
                admit_data(ex, ctx, &data)?;
                if let [expected] = description.types() {
                    let expected = crate::xdto::type_qname(ex, *expected);
                    match &data {
                        XdtoData::Value { ty, .. } | XdtoData::Object { ty, .. }
                            if *ty == expected => {}
                        _ => return Err(ctx.mismatch(ex)),
                    }
                }
                cells.push(data);
                table.serial_check_revision(ex, snapshot.revision)?;
            }
            let mut props = vector(ex, ctx, 1)?;
            if !cells.is_empty() {
                props.push(("Value".into(), cells));
            }
            rows.push(object("ValueTableRow", props));
        }
        let mut props = vector(ex, ctx, 3)?;
        if !columns.is_empty() {
            props.push(("column".into(), columns));
        }
        if !indexes.is_empty() {
            props.push(("index".into(), indexes));
        }
        if !rows.is_empty() {
            props.push(("row".into(), rows));
        }
        table.serial_check_revision(ex, snapshot.revision)?;
        Ok(object("ValueTable", props))
    })();
    ctx.leave();
    ctx.release(held);
    result
}
