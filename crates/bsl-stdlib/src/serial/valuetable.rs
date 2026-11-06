use super::codec::{InternalReader, InternalWriter, ValueCodec};
use super::internal::text::{decode_str, encode_str};
use crate::collections::ValueTable;
use crate::typedesc::idx;
use brace::Node;
use bsl_prim::Str;
use bsl_vm::{Exception, Execution, HostError, ObjRef, Value};
use platform_catalog::TypeIdx;
use std::sync::Arc;

struct TableCodec;
pub(super) fn codecs() -> Vec<(TypeIdx, Arc<dyn ValueCodec>)> {
    vec![(idx("ValueTable"), Arc::new(TableCodec))]
}
fn unsupported(ex: &mut Execution) -> Exception {
    ex.error(HostError::not_implemented(
        "ТаблицаЗначений",
        "Чтение внутреннего формата",
    ))
}
fn fields(
    ex: &mut Execution,
    w: &mut InternalWriter,
    count: usize,
) -> Result<Vec<Node>, Exception> {
    w.account(ex, (count as u64).saturating_mul(128).saturating_add(64))?;
    let mut out = Vec::new();
    out.try_reserve_exact(count)
        .map_err(|_| ex.out_of_memory())?;
    Ok(out)
}
fn unsigned(value: u64) -> Node {
    Node::atom(value.to_string())
}
fn last(next: u64) -> Node {
    next.checked_sub(1).map_or_else(|| Node::int(-1), unsigned)
}
impl ValueCodec for TableCodec {
    fn internal_write(
        &self,
        ex: &mut Execution,
        value: &ObjRef,
        w: &mut InternalWriter,
    ) -> Result<Option<Vec<Node>>, Exception> {
        if w.is_storage() {
            return Ok(None);
        }
        let Some(table) = value.as_any().downcast_ref::<ValueTable>() else {
            return Ok(None);
        };
        let bytes = table.serial_snapshot_bytes(ex)?;
        w.account(ex, bytes.saturating_add(1024))?;
        let snap = table.serial_snapshot(ex)?;
        let held = snap.rows.iter().fold(snap.columns.len(), |n, row| {
            n.saturating_add(row.cells.len())
        });
        w.hold(ex, held)?;
        let result = (|| {
            let mut columns = fields(ex, w, snap.columns.len().saturating_add(1))?;
            columns.push(unsigned(snap.columns.len() as u64));
            for column in &snap.columns {
                let mut column_fields = fields(ex, w, 5)?;
                let pattern = w.write_value(ex, &column.value_type)?;
                table.serial_check_revision(ex, snap.revision)?;
                let Node::List(mut pattern_fields) = pattern else {
                    return Err(unsupported(ex));
                };
                if pattern_fields.len() != 3 {
                    return Err(unsupported(ex));
                }
                let pattern = pattern_fields.pop().ok_or_else(|| unsupported(ex))?;
                column_fields.extend([
                    unsigned(u64::from(column.id)),
                    encode_str(column.name.units()),
                    pattern,
                    encode_str(column.title.units()),
                    Node::int(column.width),
                ]);
                columns.push(Node::List(column_fields));
            }
            let mut records = fields(ex, w, snap.rows.len().saturating_add(2))?;
            records.extend([Node::int(1), unsigned(snap.rows.len() as u64)]);
            for row in &snap.rows {
                let mut record = fields(ex, w, row.cells.len().saturating_add(4))?;
                record.extend([
                    Node::int(2),
                    unsigned(row.id),
                    unsigned(row.cells.len() as u64),
                ]);
                for value in &row.cells {
                    record.push(w.write_value(ex, value)?);
                    table.serial_check_revision(ex, snap.revision)?;
                }
                record.push(Node::int(0));
                records.push(Node::List(record));
            }
            let mut rows = fields(
                ex,
                w,
                snap.columns.len().saturating_mul(2).saturating_add(5),
            )?;
            rows.extend([Node::int(2), unsigned(snap.columns.len() as u64)]);
            for (position, column) in snap.columns.iter().enumerate() {
                rows.extend([unsigned(position as u64), unsigned(u64::from(column.id))]);
            }
            rows.extend([
                Node::List(records),
                last(u64::from(snap.next_column)),
                last(snap.next_row),
            ]);
            let mut indexes = fields(ex, w, snap.indexes.len().saturating_add(2))?;
            indexes.extend([Node::int(0), unsigned(snap.indexes.len() as u64)]);
            for definition in &snap.indexes {
                let mut ids = fields(ex, w, definition.len().saturating_add(2))?;
                ids.extend([Node::int(0), unsigned(definition.len() as u64)]);
                ids.extend(definition.iter().map(|id| unsigned(u64::from(*id))));
                indexes.push(Node::List(ids));
            }
            let mut body = fields(ex, w, 4)?;
            body.extend([
                Node::int(9),
                Node::List(columns),
                Node::List(rows),
                Node::List(indexes),
            ]);
            let mut payload = fields(ex, w, 1)?;
            payload.push(Node::List(body));
            table.serial_check_revision(ex, snap.revision)?;
            Ok(Some(payload))
        })();
        w.release(held);
        result
    }
    fn internal_read(
        &self,
        ex: &mut Execution,
        data: &[Node],
        r: &mut InternalReader,
    ) -> Result<Value, Exception> {
        if data.is_empty() {
            return Ok(Value::Undefined);
        }
        if r.is_storage() {
            return Err(unsupported(ex));
        }
        let [node] = data else {
            return Err(r.format_error(ex));
        };
        let scratch = preflight(ex, node)?;
        read_table(ex, node, r, scratch)
    }
    fn storable(&self) -> bool {
        false
    }
}

fn preflight(ex: &mut Execution, node: &Node) -> Result<u64, Exception> {
    ex.charge(1)?;
    ex.reserve(512)?;
    let mut stack = Vec::new();
    stack.try_reserve_exact(1).map_err(|_| ex.out_of_memory())?;
    stack.push(node);
    let mut bytes = 512u64;
    while let Some(node) = stack.pop() {
        ex.charge(1)?;
        bytes = bytes.saturating_add(512);
        match node {
            Node::List(items) => {
                ex.charge(items.len() as u64)?;
                bytes = bytes.saturating_add(
                    (items.len() as u64).saturating_mul(std::mem::size_of::<&Node>() as u64),
                );
                ex.reserve(bytes)?;
                stack
                    .try_reserve(items.len())
                    .map_err(|_| ex.out_of_memory())?;
                stack.extend(items);
            }
            Node::Str(s) | Node::Atom(s) => {
                ex.charge(s.len() as u64 / 64)?;
                bytes = bytes.saturating_add((s.len() as u64).saturating_mul(32));
                ex.reserve(bytes)?;
            }
        }
    }
    Ok(bytes)
}

fn atom(node: &Node, value: &str) -> bool {
    node.as_atom() == Some(value)
}
fn unsigned_field(node: &Node) -> Option<u64> {
    let text = node.as_atom()?;
    if text.len() > 20 {
        return None;
    }
    text.parse().ok()
}
fn signed_field(node: &Node) -> Option<i64> {
    let text = node.as_atom()?;
    if text.len() > 20 {
        return None;
    }
    text.parse().ok()
}
fn next_field(node: &Node) -> Option<u64> {
    if atom(node, "-1") {
        Some(0)
    } else {
        unsigned_field(node)?.checked_add(1)
    }
}
fn decode_text(ex: &mut Execution, r: &mut InternalReader, node: &Node) -> Result<Str, Exception> {
    let len = match node {
        Node::Str(s) | Node::Atom(s) => s.len(),
        _ => return Err(r.format_error(ex)),
    };
    ex.charge(len as u64 / 64 + 1)?;
    ex.reserve((len as u64).saturating_mul(8).saturating_add(64))?;
    let text = decode_str(node).ok_or_else(|| r.format_error(ex))?;
    Ok(Str::from_vec(text))
}
fn read_table(
    ex: &mut Execution,
    node: &Node,
    r: &mut InternalReader,
    scratch: u64,
) -> Result<Value, Exception> {
    let Some([version, columns, rows, indexes]) = node.as_list() else {
        return Err(r.format_error(ex));
    };
    if !atom(version, "9") {
        return Err(r.format_error(ex));
    }
    let Some([count, cols @ ..]) = columns.as_list() else {
        return Err(r.format_error(ex));
    };
    if unsigned_field(count) != Some(cols.len() as u64) {
        return Err(r.format_error(ex));
    }
    let Some(row_fields) = rows.as_list() else {
        return Err(r.format_error(ex));
    };
    let expected = cols
        .len()
        .checked_mul(2)
        .and_then(|n| n.checked_add(5))
        .ok_or_else(|| r.format_error(ex))?;
    if row_fields.len() != expected
        || !atom(&row_fields[0], "2")
        || unsigned_field(&row_fields[1]) != Some(cols.len() as u64)
    {
        return Err(r.format_error(ex));
    }
    let next_col = next_field(&row_fields[expected - 2])
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| *n < u32::MAX - 0x10000)
        .ok_or_else(|| r.format_error(ex))?;
    let next_row = next_field(&row_fields[expected - 1]).ok_or_else(|| r.format_error(ex))?;
    let mut ids = Vec::new();
    let mut names = Vec::new();
    ids.try_reserve_exact(cols.len())
        .map_err(|_| ex.out_of_memory())?;
    names
        .try_reserve_exact(cols.len())
        .map_err(|_| ex.out_of_memory())?;
    ex.charge((cols.len() as u64).saturating_mul(cols.len() as u64 + 1))?;
    for (i, column) in cols.iter().enumerate() {
        let Some([id, name, pattern, title, width]) = column.as_list() else {
            return Err(r.format_error(ex));
        };
        let id = unsigned_field(id)
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n < next_col)
            .ok_or_else(|| r.format_error(ex))?;
        if ids.contains(&id)
            || unsigned_field(&row_fields[2 + 2 * i]) != Some(i as u64)
            || unsigned_field(&row_fields[3 + 2 * i]) != Some(id as u64)
            || pattern.as_list().is_none()
            || signed_field(width).is_none()
        {
            return Err(r.format_error(ex));
        }
        let name = decode_text(ex, r, name)?;
        let _ = decode_text(ex, r, title)?;
        ids.push(id);
        names.push(name);
    }
    if !ValueTable::serial_names_valid(ex, &names)? {
        return Err(r.format_error(ex));
    }
    let Some([records_tag, count, records @ ..]) = row_fields[expected - 3].as_list() else {
        return Err(r.format_error(ex));
    };
    if !atom(records_tag, "1") || unsigned_field(count) != Some(records.len() as u64) {
        return Err(r.format_error(ex));
    }
    let mut row_ids = Vec::new();
    row_ids
        .try_reserve_exact(records.len())
        .map_err(|_| ex.out_of_memory())?;
    ex.charge((records.len() as u64).saturating_mul(records.len() as u64 + 1))?;
    for record in records {
        let Some(fields) = record.as_list() else {
            return Err(r.format_error(ex));
        };
        if fields.len() < 4 || !atom(&fields[0], "2") {
            return Err(r.format_error(ex));
        }
        let id = unsigned_field(&fields[1])
            .filter(|id| *id < next_row)
            .ok_or_else(|| r.format_error(ex))?;
        let width = unsigned_field(&fields[2])
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(|| r.format_error(ex))?;
        if row_ids.contains(&id)
            || (width != 0 && width != cols.len())
            || width.checked_add(4) != Some(fields.len())
            || !atom(&fields[fields.len() - 1], "0")
        {
            return Err(r.format_error(ex));
        }
        row_ids.push(id);
    }
    let Some([tag, count, definitions @ ..]) = indexes.as_list() else {
        return Err(r.format_error(ex));
    };
    if !atom(tag, "0") || unsigned_field(count) != Some(definitions.len() as u64) {
        return Err(r.format_error(ex));
    }
    for definition in definitions {
        let Some([tag, count, members @ ..]) = definition.as_list() else {
            return Err(r.format_error(ex));
        };
        if !atom(tag, "0")
            || members.is_empty()
            || unsigned_field(count) != Some(members.len() as u64)
        {
            return Err(r.format_error(ex));
        }
        ex.charge(
            (members.len() as u64).saturating_mul(
                (members.len() as u64)
                    .saturating_add(ids.len() as u64)
                    .saturating_add(1),
            ),
        )?;
        for (i, member) in members.iter().enumerate() {
            let id = unsigned_field(member)
                .and_then(|n| u32::try_from(n).ok())
                .filter(|id| ids.contains(id))
                .ok_or_else(|| r.format_error(ex))?;
            if members[..i]
                .iter()
                .any(|n| unsigned_field(n) == Some(id as u64))
            {
                return Err(r.format_error(ex));
            }
        }
    }
    let result = ValueTable::serial_new(ex)?;
    let table = result
        .as_object()
        .and_then(|o| o.as_any().downcast_ref::<ValueTable>())
        .ok_or_else(|| r.format_error(ex))?;
    let vector_bytes = (cols.len() as u64)
        .saturating_add(records.len() as u64)
        .saturating_add(definitions.len() as u64)
        .saturating_mul(std::mem::size_of::<usize>() as u64);
    ValueTable::serial_admit(ex, &result, None, scratch.saturating_add(vector_bytes))?;
    table.serial_reserve_entries(
        ex,
        cols.len(),
        records.len(),
        definitions.len(),
        &result,
        scratch,
    )?;
    ValueTable::serial_admit(ex, &result, None, scratch)?;
    for (i, column) in cols.iter().enumerate() {
        let fields = column.as_list().ok_or_else(|| r.format_error(ex))?;
        ValueTable::serial_admit(ex, &result, None, scratch)?;
        let value_type =
            r.read_inline_object(ex, idx("TypeDescription"), std::slice::from_ref(&fields[2]))?;
        let title = decode_text(ex, r, &fields[3])?;
        let width = signed_field(&fields[4]).ok_or_else(|| r.format_error(ex))?;
        ValueTable::serial_admit(ex, &result, Some(&value_type), scratch.saturating_add(1024))?;
        table.serial_restore_column(ex, ids[i], names[i].clone(), value_type, title, width)?;
    }
    table.serial_restore_counters(next_col, next_row);
    for definition in definitions {
        let fields = definition.as_list().ok_or_else(|| r.format_error(ex))?;
        let mut members = Vec::new();
        members
            .try_reserve_exact(fields.len() - 2)
            .map_err(|_| ex.out_of_memory())?;
        for id in &fields[2..] {
            members.push(
                unsigned_field(id)
                    .and_then(|n| u32::try_from(n).ok())
                    .ok_or_else(|| r.format_error(ex))?,
            );
        }
        ValueTable::serial_admit(
            ex,
            &result,
            None,
            scratch
                .saturating_add(
                    (members.len() as u64).saturating_mul(2 * std::mem::size_of::<usize>() as u64),
                )
                .saturating_add(512),
        )?;
        table.serial_restore_index(ex, &members)?;
    }
    for record in records {
        let fields = record.as_list().ok_or_else(|| r.format_error(ex))?;
        let id = unsigned_field(&fields[1]).ok_or_else(|| r.format_error(ex))?;
        let width = fields.len() - 4;
        ValueTable::serial_admit(
            ex,
            &result,
            None,
            scratch
                .saturating_add((cols.len() as u64).saturating_mul(
                    (std::mem::size_of::<(u32, Value)>() + std::mem::size_of::<usize>()) as u64,
                ))
                .saturating_add(1024),
        )?;
        table.serial_restore_row(ex, id, width != 0)?;
        for (i, node) in fields[3..3 + width].iter().enumerate() {
            ValueTable::serial_admit(ex, &result, None, scratch)?;
            let value = r.read_value(ex, node)?;
            if matches!(value, Value::Undefined)
                && !matches!(node.as_list(), Some([tag, ..]) if tag.as_str() == Some("U"))
            {
                return Err(ex.error(HostError::not_implemented(
                    "ТаблицаЗначений",
                    "Вложенное значение без восстановимых данных",
                )));
            }
            ValueTable::serial_admit(ex, &result, Some(&value), scratch)?;
            table.serial_restore_cell(ex, i, value)?;
        }
    }
    ValueTable::serial_admit(ex, &result, None, scratch)?;
    Ok(result)
}
