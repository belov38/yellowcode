use super::codec::{InternalReader, InternalWriter, ValueCodec};
use super::internal::text::{decode_str, encode_str};
use crate::collections::ValueList;
use crate::typedesc::{TypeDescription, idx, new_type_description};
use brace::Node;
use bsl_prim::Str;
use bsl_vm::{Exception, Execution, HostError, ObjRef, Value};
use platform_catalog::TypeIdx;
use std::sync::Arc;

const ITEM_GUID: &str = "1e512aab-1b41-4ef6-9375-f0137be9dd91";
struct ListCodec;
pub(super) fn codecs() -> Vec<(TypeIdx, Arc<dyn ValueCodec>)> {
    vec![(idx("ValueList"), Arc::new(ListCodec))]
}
fn unsupported(ex: &mut Execution) -> Exception {
    ex.error(HostError::not_implemented(
        "СписокЗначений",
        "Внутренняя сериализация картинки или метаданных",
    ))
}
fn empty_picture() -> Node {
    Node::List(vec![
        Node::int(4),
        Node::int(0),
        Node::List(vec![Node::int(0)]),
        Node::str(""),
        Node::int(-1),
        Node::int(-1),
        Node::int(0),
        Node::int(0),
        Node::str(""),
    ])
}
fn atom(n: &Node, expected: &str) -> bool {
    n.as_atom() == Some(expected)
}
fn integer(n: &Node) -> Option<i64> {
    let text = n.as_atom()?;
    if text.len() > 20 {
        return None;
    }
    text.parse().ok()
}
fn is_empty_picture(n: &Node) -> bool {
    let Some([version, mode, data, name, width, height, flag, zero, tail]) = n.as_list() else {
        return false;
    };
    atom(version, "4")
        && atom(mode, "0")
        && matches!(data.as_list(), Some([n]) if atom(n,"0"))
        && name.as_str() == Some("")
        && atom(width, "-1")
        && atom(height, "-1")
        && atom(flag, "0")
        && atom(zero, "0")
        && tail.as_str() == Some("")
}
fn write_inline(
    ex: &mut Execution,
    w: &mut InternalWriter,
    value: &Value,
) -> Result<Node, Exception> {
    let node = w.write_value(ex, value)?;
    if let Node::List(mut fields) = node
        && fields.len() == 3
    {
        return fields.pop().ok_or_else(|| unsupported(ex));
    }
    Err(unsupported(ex))
}
impl ValueCodec for ListCodec {
    fn internal_write(
        &self,
        ex: &mut Execution,
        value: &ObjRef,
        w: &mut InternalWriter,
    ) -> Result<Option<Vec<Node>>, Exception> {
        let Some(list) = value.as_any().downcast_ref::<ValueList>() else {
            return Ok(None);
        };
        let snapshot_bytes = list.serial_snapshot_bytes(ex)?;
        w.account(ex, snapshot_bytes.saturating_add(1024))?;
        let mut snap = list.serial_snapshot(ex)?;
        let held = snap.items.len().saturating_mul(2).saturating_add(1);
        w.hold(ex, held)?;
        let result = (|| {
            w.account(ex, (snap.items.len() as u64).saturating_mul(2048))?;
            let mut entries = Vec::new();
            entries
                .try_reserve_exact(snap.items.len().saturating_add(1))
                .map_err(|_| ex.out_of_memory())?;
            entries.push(Node::int(
                i64::try_from(snap.items.len()).map_err(|_| ex.out_of_memory())?,
            ));
            for item in &snap.items {
                w.account(ex, (item.presentation.len() as u64).saturating_mul(12))?;
                let picture = if matches!(item.picture, Value::Undefined) {
                    empty_picture()
                } else {
                    let node = write_inline(ex, w, &item.picture)?;
                    if !is_empty_picture(&node) {
                        return Err(unsupported(ex));
                    }
                    node
                };
                let value = w.write_value(ex, &item.value)?;
                entries.push(Node::List(vec![
                    Node::atom(ITEM_GUID),
                    Node::List(vec![
                        encode_str(item.presentation.units()),
                        Node::int(i64::from(item.check)),
                        value,
                        picture,
                        Node::int(i64::try_from(item.id).map_err(|_| ex.out_of_memory())?),
                        Node::int(0),
                        Node::str(""),
                    ]),
                ]));
                list.serial_check_revision(ex, snap.revision)?;
            }
            let td = snap
                .policy
                .take()
                .unwrap_or_else(|| TypeDescription::new(&[], None, None, None, None, ex.runtime()));
            let pattern = write_inline(ex, w, &new_type_description(td))?;
            let mut body = vec![
                Node::int(6),
                Node::atom(ITEM_GUID),
                Node::int(0),
                Node::int(0),
                Node::List(entries),
                pattern,
            ];
            if matches!(snap.available, Value::Undefined) {
                body.push(Node::int(0));
            } else {
                if !matches!(&snap.available, Value::Object(o) if o.as_any().is::<ValueList>()) {
                    return Err(unsupported(ex));
                }
                body.push(Node::int(1));
                body.push(write_inline(ex, w, &snap.available)?);
            }
            let next = i64::try_from(snap.next_id).map_err(|_| ex.out_of_memory())?;
            body.push(Node::int(next - 1));
            list.serial_check_revision(ex, snap.revision)?;
            Ok(Some(vec![Node::List(body)]))
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
        let [node] = data else {
            return Err(r.format_error(ex));
        };
        preflight(ex, node)?;
        read_list(ex, node, r)
    }
}
fn read_list(ex: &mut Execution, node: &Node, r: &mut InternalReader) -> Result<Value, Exception> {
    let Some([version, guid, zero1, zero2, entries, pattern, tail @ ..]) = node.as_list() else {
        return Err(r.format_error(ex));
    };
    if !atom(version, "6") || !atom(guid, ITEM_GUID) || !atom(zero1, "0") || !atom(zero2, "0") {
        return Err(r.format_error(ex));
    }
    let (available, last) = match tail {
        [flag, last] if atom(flag, "0") => (None, last),
        [flag, available, last] if atom(flag, "1") => (Some(available), last),
        _ => return Err(r.format_error(ex)),
    };
    let last = integer(last)
        .filter(|n| *n >= -1 && *n < i64::MAX)
        .ok_or_else(|| r.format_error(ex))?;
    let Some([count, items @ ..]) = entries.as_list() else {
        return Err(r.format_error(ex));
    };
    if integer(count).and_then(|n| usize::try_from(n).ok()) != Some(items.len()) {
        return Err(r.format_error(ex));
    }
    let mut ids = crate::collections::vec_with_capacity(ex, items.len() as u64)?;
    for item in items {
        let fields = item_fields(item).ok_or_else(|| r.format_error(ex))?;
        let id = integer(&fields[4])
            .filter(|id| *id >= 0 && *id <= last)
            .ok_or_else(|| r.format_error(ex))?;
        ids.push(id);
    }
    ex.charge((ids.len() as u64).saturating_mul(u64::from((ids.len() as u64).max(1).ilog2()) + 1))?;
    ids.sort_unstable();
    if ids.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(r.format_error(ex));
    }
    drop(ids);
    let td = r.read_inline_object(ex, idx("TypeDescription"), std::slice::from_ref(pattern))?;
    let td = td
        .as_object()
        .and_then(|o| o.as_any().downcast_ref::<TypeDescription>())
        .ok_or_else(|| r.format_error(ex))?;
    ex.reserve(td.backing_bytes().saturating_add(256))?;
    let policy = td.session_copy().map_err(|_| ex.out_of_memory())?;
    let list = ValueList::serial_restore(ex, items.len(), policy, (last + 1) as u64)?;
    for item in items {
        let fields = item_fields(item).ok_or_else(|| r.format_error(ex))?;
        let text_bytes = match &fields[0] {
            Node::Str(s) | Node::Atom(s) => s.len() as u64,
            _ => return Err(r.format_error(ex)),
        };
        ex.reserve(text_bytes.saturating_mul(8))?;
        ex.charge(1 + text_bytes / 64)?;
        let presentation = decode_str(&fields[0]).ok_or_else(|| r.format_error(ex))?;
        let value = r.read_value(ex, &fields[2])?;
        let id = integer(&fields[4]).ok_or_else(|| r.format_error(ex))? as u64;
        list.serial_push(
            id,
            value,
            Str::from_vec(presentation),
            atom(&fields[1], "1"),
        );
    }
    if let Some(node) = available {
        let value = r.read_inline_object(ex, idx("ValueList"), std::slice::from_ref(node))?;
        if !matches!(&value, Value::Object(o) if o.as_any().is::<ValueList>()) {
            return Err(r.format_error(ex));
        }
        list.serial_available(value);
    }
    Ok(Value::Object(list))
}
fn item_fields(node: &Node) -> Option<&[Node]> {
    let [guid, body] = node.as_list()? else {
        return None;
    };
    if !atom(guid, ITEM_GUID) {
        return None;
    }
    let fields = body.as_list()?;
    let [presentation, check, _, picture, _, format_set, format] = fields else {
        return None;
    };
    if !matches!(presentation, Node::Str(_) | Node::Atom(_))
        || !(atom(check, "0") || atom(check, "1"))
        || !is_empty_picture(picture)
        || !atom(format_set, "0")
        || format.as_str() != Some("")
    {
        return None;
    }
    Some(fields)
}
fn preflight(ex: &mut Execution, node: &Node) -> Result<(), Exception> {
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
    Ok(())
}
