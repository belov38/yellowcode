use super::*;
use crate::collections::columns::SerialColumn;
use bsl_vm::Exception;

pub(crate) struct SerialNode {
    pub(crate) id: usize,
    pub(crate) level: usize,
    pub(crate) children: Vec<usize>,
    pub(crate) cells: Vec<Value>,
}
pub(crate) struct SerialSnapshot {
    pub(crate) columns: Vec<SerialColumn>,
    pub(crate) roots: Vec<usize>,
    pub(crate) nodes: Vec<Option<SerialNode>>,
    pub(crate) next_column: u32,
    pub(crate) revision: [u64; 2],
}
impl Drop for SerialNode {
    fn drop(&mut self) {
        super::super::release(std::mem::take(&mut self.cells));
    }
}
impl ValueTree {
    pub(crate) fn serial_revision(&self) -> [u64; 2] {
        [self.state.revision.get(), self.state.columns.revision()]
    }
    pub(crate) fn serial_check(&self, ex: &mut Execution, rev: [u64; 2]) -> Result<(), Exception> {
        self.state
            .check_source(rev[0], rev[1])
            .map_err(|e| match e {
                bsl_vm::CallError::Host(e) => ex.error(e),
                bsl_vm::CallError::Exc(e) => e,
            })
    }
    pub(crate) fn serial_snapshot_bytes(&self, ex: &mut Execution) -> Result<u64, Exception> {
        let arena = self.state.arena.borrow();
        ex.charge(arena.len() as u64 + 1)?;
        let mut bytes = self
            .state
            .columns
            .serial_bytes(ex)?
            .saturating_add((arena.len() as u64).saturating_mul(256))
            .saturating_add((self.state.roots.borrow().len() as u64).saturating_mul(32));
        for node in arena.iter().flatten() {
            let children = node.children.borrow();
            let cells = node.cells.borrow();
            ex.charge((children.len() as u64).saturating_add(cells.0.len() as u64 + 1))?;
            bytes = bytes.saturating_add((children.len() as u64).saturating_mul(32));
            if node.materialized.get() {
                bytes = bytes.saturating_add((self.state.columns.len() as u64).saturating_mul(128));
                for (_, value) in &cells.0 {
                    bytes = bytes.saturating_add(match value {
                        Value::Number(n) => n
                            .size_hint_limbs()
                            .saturating_mul(64)
                            .saturating_add((n.backing_capacity_limbs() as u64).saturating_mul(4))
                            .saturating_add(256),
                        Value::String(s) => (s.len() as u64)
                            .saturating_mul(12)
                            .saturating_add(super::super::retained::text_extra(s)),
                        _ => value_bytes(value),
                    });
                }
            }
        }
        Ok(bytes)
    }
    pub(crate) fn serial_snapshot(
        &self,
        ex: &mut Execution,
        revision: [u64; 2],
    ) -> Result<SerialSnapshot, Exception> {
        self.serial_check(ex, revision)?;
        let columns = self.state.columns.serial_columns(ex)?;
        let arena = self.state.arena.borrow();
        let roots_src = self.state.roots.borrow();
        ex.charge((arena.len() as u64).saturating_add(roots_src.len() as u64))?;
        let mut roots = Vec::new();
        roots
            .try_reserve_exact(roots_src.len())
            .map_err(|_| ex.out_of_memory())?;
        roots.extend_from_slice(&roots_src);
        let mut nodes = Vec::new();
        nodes
            .try_reserve_exact(arena.len())
            .map_err(|_| ex.out_of_memory())?;
        for node in arena.iter() {
            let Some(node) = node else {
                nodes.push(None);
                continue;
            };
            if node.level < 0 || node.level as usize >= crate::serial::depth::MAX_VALUE_DEPTH {
                return Err(crate::serial::cycle_fatal(ex));
            }
            let source_children = node.children.borrow();
            ex.charge(source_children.len() as u64)?;
            let mut children = Vec::new();
            children
                .try_reserve_exact(source_children.len())
                .map_err(|_| ex.out_of_memory())?;
            children.extend_from_slice(&source_children);
            let mut cells = Vec::new();
            if node.materialized.get() {
                let source = node.cells.borrow();
                ex.charge((columns.len() as u64).saturating_mul(source.0.len() as u64 + 1))?;
                cells
                    .try_reserve_exact(columns.len())
                    .map_err(|_| ex.out_of_memory())?;
                for column in &columns {
                    let value = source
                        .0
                        .iter()
                        .find(|(id, _)| *id == column.id)
                        .map(|(_, value)| value)
                        .ok_or_else(|| ex.error(changed()))?;
                    cells.push(match value {
                        Value::Number(n) => Value::Number(
                            n.try_clone_with(|_| Ok::<_, Exception>(()))
                                .map_err(|_| ex.out_of_memory())?,
                        ),
                        _ => value.clone(),
                    });
                }
            }
            nodes.push(Some(SerialNode {
                id: node.id,
                level: node.level as usize,
                children,
                cells,
            }));
        }
        drop(roots_src);
        drop(arena);
        self.serial_check(ex, revision)?;
        Ok(SerialSnapshot {
            columns,
            roots,
            nodes,
            next_column: self.state.columns.serial_next_id(),
            revision,
        })
    }
}
