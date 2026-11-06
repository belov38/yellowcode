use super::codec::{InternalReader, InternalWriter, ValueCodec};
use super::internal::text::encode_str;
use super::nodes::{NodeList, OwnedNode};
use crate::collections::ValueTree;
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
impl ValueCodec for TreeCodec {
    fn internal_write(
        &self,
        ex: &mut Execution,
        object: &ObjRef,
        w: &mut InternalWriter,
    ) -> Result<Option<Vec<Node>>, Exception> {
        if w.is_storage() {
            return Ok(None);
        }
        let Some(tree) = object.as_any().downcast_ref::<ValueTree>() else {
            return Ok(None);
        };
        let revision = tree.serial_revision();
        let source = Value::Object(object.clone());
        let bytes = tree.serial_snapshot_bytes(ex)?;
        let retained = ex.measure_retained(w.estimate().saturating_add(bytes), |visit| {
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
                ex.measure_retained(w.estimate(), |visit| {
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
                    admit(ex, w)?;
                    item.push(
                        w.with_depth(ex, row_depth(row.level), |w, ex| w.write_value(ex, cell))?,
                    );
                    admit(ex, w)?;
                }
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
        _: &mut Execution,
        _: &[Node],
        _: &mut InternalReader,
    ) -> Result<Value, Exception> {
        Ok(Value::Undefined)
    }
    fn storable(&self) -> bool {
        false
    }
}
