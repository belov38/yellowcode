use super::codec::{InternalReader, InternalWriter, ValueCodec};
use super::depth::MAX_VALUE_DEPTH;
use super::internal::text::encode_str;
use super::nodes::{NodeList, OwnedNode};
use super::valuetable::{atom, decode_text, next_field, preflight, signed_field, unsigned_field};
use crate::collections::{ValueTable, ValueTree};
use crate::typedesc::idx;
use brace::Node;
use bsl_vm::{Exception, Execution, HostError, ObjRef, Value};
use platform_catalog::TypeIdx;
use std::sync::Arc;

struct TreeCodec;

struct BuiltRows(Vec<Option<Node>>);
impl Drop for BuiltRows {
    fn drop(&mut self) {
        for node in self.0.iter_mut().filter_map(Option::take) {
            drop(OwnedNode(node));
        }
    }
}

fn row_depth(level: usize) -> usize {
    level.saturating_mul(2).saturating_add(4)
}
pub(super) fn codecs() -> Vec<(TypeIdx, Arc<dyn ValueCodec>)> {
    vec![(idx("ValueTree"), Arc::new(TreeCodec))]
}
fn fields(ex: &mut Execution, w: &mut InternalWriter, count: usize) -> Result<NodeList, Exception> {
    w.account(ex, (count as u64).saturating_mul(128).saturating_add(64))?;
    let mut fields = Vec::new();
    fields
        .try_reserve_exact(count)
        .map_err(|_| ex.out_of_memory())?;
    Ok(NodeList(fields))
}
fn unsigned(n: usize) -> Node {
    Node::atom(n.to_string())
}
fn last(n: usize) -> Node {
    n.checked_sub(1).map_or_else(|| Node::int(-1), unsigned)
}
fn changed(ex: &mut Execution) -> Exception {
    ex.error(HostError::not_implemented(
        "ДеревоЗначений",
        "Рекурсивная операция",
    ))
}
const MAX_ROW_LEVEL: usize = (MAX_VALUE_DEPTH - 6) / 2;
fn admit(
    ex: &mut Execution,
    tree: &Value,
    incoming: Option<&Value>,
    scratch: u64,
) -> Result<(), Exception> {
    ex.measure_retained_lenient(scratch, |visit| {
        visit.value(tree)?;
        if let Some(incoming) = incoming {
            visit.value(incoming)?;
        }
        Ok(())
    })?;
    Ok(())
}
fn read_tree(
    ex: &mut Execution,
    body: &Node,
    r: &mut InternalReader,
    scratch: u64,
) -> Result<Value, Exception> {
    let Some([version, columns, rows]) = body.as_list() else {
        return Err(r.format_error(ex));
    };
    let Some([count, cols @ ..]) = columns.as_list() else {
        return Err(r.format_error(ex));
    };
    let Some(row_fields) = rows.as_list() else {
        return Err(r.format_error(ex));
    };
    let expected = cols.len().checked_mul(2).and_then(|n| n.checked_add(5));
    let Some(expected) = expected.filter(|n| row_fields.len() == *n) else {
        return Err(r.format_error(ex));
    };
    if !atom(version, "1")
        || unsigned_field(count) != Some(cols.len() as u64)
        || !atom(&row_fields[0], "2")
        || unsigned_field(&row_fields[1]) != Some(cols.len() as u64)
    {
        return Err(r.format_error(ex));
    }
    let next_col = next_field(&row_fields[expected - 2])
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| *n < u32::MAX - 0x10000);
    let next_row = next_field(&row_fields[expected - 1]).and_then(|n| usize::try_from(n).ok());
    let (Some(next_col), Some(next_row)) = (next_col, next_row) else {
        return Err(r.format_error(ex));
    };
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
            .filter(|n| *n < next_col);
        let Some(id) = id else {
            return Err(r.format_error(ex));
        };
        if ids.contains(&id)
            || unsigned_field(&row_fields[2 + 2 * i]) != Some(i as u64)
            || unsigned_field(&row_fields[3 + 2 * i]) != Some(u64::from(id))
            || pattern.as_list().is_none()
            || signed_field(width).is_none()
        {
            return Err(r.format_error(ex));
        }
        names.push(decode_text(ex, r, name)?);
        let _ = decode_text(ex, r, title)?;
        ids.push(id);
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
    let result = ex.construct(idx("ValueTree"), &mut [])?;
    let Some(tree) = result
        .as_object()
        .and_then(|o| o.as_any().downcast_ref::<ValueTree>())
    else {
        return Err(r.format_error(ex));
    };
    let census = |ex: &mut Execution, incoming: Option<&Value>, extra: u64| {
        admit(ex, &result, incoming, scratch.saturating_add(extra))
    };
    let word = std::mem::size_of::<usize>() as u64;
    let vectors = (cols.len() as u64)
        .saturating_add(next_row as u64)
        .saturating_add(records.len() as u64);
    census(ex, None, vectors.saturating_mul(word))?;
    tree.serial_reserve(ex, cols.len(), next_col, next_row, records.len())?;
    census(ex, None, 0)?;
    for (column, (id, name)) in cols.iter().zip(ids.into_iter().zip(names)) {
        let Some([_, _, pattern, title, width]) = column.as_list() else {
            return Err(r.format_error(ex));
        };
        census(ex, None, 0)?;
        let value_type =
            r.read_inline_object(ex, idx("TypeDescription"), std::slice::from_ref(pattern))?;
        let title = decode_text(ex, r, title)?;
        let Some(width) = signed_field(width) else {
            return Err(r.format_error(ex));
        };
        census(ex, Some(&value_type), 1024)?;
        tree.serial_restore_column(ex, id, name, value_type, title, width)?;
    }
    struct Level<'a> {
        rows: &'a [Node],
        next: usize,
        parent: Option<usize>,
    }
    let mut stack = Vec::new();
    stack.try_reserve(1).map_err(|_| ex.out_of_memory())?;
    stack.push(Level {
        rows: records,
        next: 0,
        parent: None,
    });
    while let Some(top) = stack.last_mut() {
        let Some(row) = top.rows.get(top.next) else {
            stack.pop();
            continue;
        };
        top.next += 1;
        let parent = top.parent;
        let level = stack.len() - 1;
        ex.charge(1)?;
        let Some(fields) = row.as_list() else {
            return Err(r.format_error(ex));
        };
        let id = fields
            .get(1)
            .and_then(unsigned_field)
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n < next_row && parent.is_none_or(|p| *n > p));
        let width = fields
            .get(2)
            .and_then(unsigned_field)
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n == 0 || *n == cols.len());
        let (Some(id), Some(width)) = (id, width) else {
            return Err(r.format_error(ex));
        };
        let children = match (
            fields.get(3 + width),
            fields.get(4 + width),
            fields.get(5 + width),
        ) {
            (Some(flag), None, None) if atom(flag, "0") => Some(&[][..]),
            (Some(flag), Some(list), None) if atom(flag, "1") => match list.as_list() {
                Some([tag, count, rows @ ..])
                    if atom(tag, "1") && unsigned_field(count) == Some(rows.len() as u64) =>
                {
                    Some(rows)
                }
                _ => None,
            },
            _ => None,
        };
        let well_formed =
            fields.first().is_some_and(|tag| atom(tag, "2")) && level <= MAX_ROW_LEVEL;
        let Some(children) = children.filter(|_| well_formed) else {
            return Err(r.format_error(ex));
        };
        let row_bytes = (cols.len() as u64)
            .saturating_mul(std::mem::size_of::<(u32, Value)>() as u64 + word)
            .saturating_add((children.len() as u64).saturating_mul(word));
        census(ex, None, row_bytes.saturating_add(1024))?;
        if !tree.serial_restore_row(ex, id, parent, children.len(), width != 0)? {
            return Err(r.format_error(ex));
        }
        for (i, node) in fields[3..3 + width].iter().enumerate() {
            let value = r.read_value(ex, node)?;
            if matches!(value, Value::Undefined)
                && !matches!(node.as_list(), Some([tag, ..]) if tag.as_str() == Some("U"))
            {
                return Err(ex.error(HostError::not_implemented(
                    "ДеревоЗначений",
                    "Вложенное значение без восстановимых данных",
                )));
            }
            tree.serial_restore_cell(ex, id, i, value)?;
        }
        if !children.is_empty() {
            stack.try_reserve(1).map_err(|_| ex.out_of_memory())?;
            stack.push(Level {
                rows: children,
                next: 0,
                parent: Some(id),
            });
        }
    }
    census(ex, None, 0)?;
    Ok(result)
}
impl ValueCodec for TreeCodec {
    fn internal_write(
        &self,
        ex: &mut Execution,
        object: &ObjRef,
        w: &mut InternalWriter,
    ) -> Result<Option<Vec<Node>>, Exception> {
        let Some(tree) = object.as_any().downcast_ref::<ValueTree>() else {
            return Ok(None);
        };
        let revision = tree.serial_revision();
        let source = Value::Object(object.clone());
        let bytes = tree.serial_snapshot_bytes(ex)?;
        let retained = ex
            .measure_retained_lenient(w.estimate().saturating_add(bytes), |visit| {
                visit.value(&source)
            })?;
        tree.serial_check(ex, revision)?;
        w.account(ex, bytes.saturating_add(retained).saturating_add(1024))?;
        let snapshot = tree.serial_snapshot(ex, revision)?;
        w.check_depth(ex, 3)?;
        for row in snapshot.nodes.iter().flatten() {
            ex.charge(1)?;
            w.check_depth(ex, row_depth(row.level).saturating_add(1))?;
        }
        let held = snapshot
            .nodes
            .iter()
            .flatten()
            .fold(snapshot.columns.len(), |n, row| {
                n.saturating_add(row.cells.len())
            });
        w.hold(ex, held)?;
        let result = (|| {
            let admit = |ex: &mut Execution, w: &InternalWriter| -> Result<(), Exception> {
                ex.measure_retained_lenient(w.estimate(), |visit| {
                    visit.work(
                        (snapshot.nodes.len() as u64).saturating_add(snapshot.columns.len() as u64),
                    )?;
                    visit.value(&source)?;
                    for col in &snapshot.columns {
                        visit.value(&col.value_type)?;
                    }
                    for row in snapshot.nodes.iter().flatten() {
                        visit.work(row.cells.len() as u64)?;
                        for value in &row.cells {
                            visit.value(value)?;
                        }
                    }
                    Ok(())
                })?;
                tree.serial_check(ex, snapshot.revision)
            };
            let mut columns = fields(ex, w, snapshot.columns.len().saturating_add(1))?;
            columns.push(unsigned(snapshot.columns.len()));
            for column in &snapshot.columns {
                admit(ex, w)?;
                let mut pattern =
                    OwnedNode(w.with_depth(ex, 3, |w, ex| w.write_value(ex, &column.value_type))?);
                admit(ex, w)?;
                let Node::List(pattern) = &mut pattern.0 else {
                    return Err(changed(ex));
                };
                if pattern.len() != 3 {
                    return Err(changed(ex));
                }
                let mut item = fields(ex, w, 5)?;
                let pattern = pattern.pop().ok_or_else(|| changed(ex))?;
                item.extend([
                    unsigned(column.id as usize),
                    encode_str(column.name.units()),
                    pattern,
                    encode_str(column.title.units()),
                    Node::int(column.width),
                ]);
                columns.push(item.into_node());
            }
            w.account(ex, (snapshot.nodes.len() as u64).saturating_mul(32))?;
            let mut built: Vec<Option<Node>> = Vec::new();
            built
                .try_reserve_exact(snapshot.nodes.len())
                .map_err(|_| ex.out_of_memory())?;
            built.resize_with(snapshot.nodes.len(), || None);
            let mut built = BuiltRows(built);
            for row in snapshot.nodes.iter().rev().flatten() {
                let mut item = fields(ex, w, row.cells.len().saturating_add(5))?;
                item.extend([Node::int(2), unsigned(row.id), unsigned(row.cells.len())]);
                for cell in &row.cells {
                    tree.serial_check(ex, snapshot.revision)?;
                    item.push(
                        w.with_depth(ex, row_depth(row.level), |w, ex| w.write_value(ex, cell))?,
                    );
                }
                admit(ex, w)?;
                if row.children.is_empty() {
                    item.push(Node::int(0));
                } else {
                    let mut children = fields(ex, w, row.children.len().saturating_add(2))?;
                    children.extend([Node::int(1), unsigned(row.children.len())]);
                    ex.charge(row.children.len() as u64)?;
                    for &id in &row.children {
                        if id <= row.id {
                            return Err(changed(ex));
                        }
                        children.push(
                            built
                                .0
                                .get_mut(id)
                                .and_then(Option::take)
                                .ok_or_else(|| changed(ex))?,
                        );
                    }
                    item.extend([Node::int(1), children.into_node()]);
                }
                let slot = built.0.get_mut(row.id).ok_or_else(|| changed(ex))?;
                *slot = Some(item.into_node());
            }
            let mut records = fields(ex, w, snapshot.roots.len().saturating_add(2))?;
            records.extend([Node::int(1), unsigned(snapshot.roots.len())]);
            ex.charge(snapshot.roots.len() as u64)?;
            for &id in &snapshot.roots {
                records.push(
                    built
                        .0
                        .get_mut(id)
                        .and_then(Option::take)
                        .ok_or_else(|| changed(ex))?,
                );
            }
            let mut rows = fields(
                ex,
                w,
                snapshot.columns.len().saturating_mul(2).saturating_add(5),
            )?;
            rows.extend([Node::int(2), unsigned(snapshot.columns.len())]);
            for (position, column) in snapshot.columns.iter().enumerate() {
                rows.extend([unsigned(position), unsigned(column.id as usize)]);
            }
            rows.extend([
                records.into_node(),
                last(snapshot.next_column as usize),
                last(snapshot.nodes.len()),
            ]);
            let mut body = fields(ex, w, 3)?;
            body.extend([Node::int(1), columns.into_node(), rows.into_node()]);
            let mut payload = fields(ex, w, 1)?;
            payload.push(body.into_node());
            admit(ex, w)?;
            Ok(Some(payload.into_vec()))
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
        let [body] = data else {
            return Err(r.format_error(ex));
        };
        let scratch = preflight(ex, body)?;
        read_tree(ex, body, r, scratch)
    }
}
