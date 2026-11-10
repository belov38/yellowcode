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

fn exception(ex: &mut Execution, error: bsl_vm::CallError) -> Exception {
    match error {
        bsl_vm::CallError::Host(error) => ex.error(error),
        bsl_vm::CallError::Exc(error) => error,
    }
}
impl ValueTree {
    pub(crate) fn serial_reserve(
        &self,
        ex: &mut Execution,
        columns: usize,
        next_column: u32,
        arena: usize,
        roots: usize,
    ) -> Result<(), Exception> {
        let state = &self.state;
        state
            .columns
            .serial_reserve_entries(ex, columns)
            .map_err(|e| exception(ex, e))?;
        state.columns.serial_set_next(next_column);
        let mut slots: Vec<Option<Rc<Node>>> = vec_with_capacity(ex, arena as u64)?;
        ex.charge(arena as u64 / 64 + 1)?;
        slots.resize_with(arena, || None);
        let ids = vec_with_capacity(ex, roots as u64)?;
        *state.arena.borrow_mut() = slots;
        *state.roots.borrow_mut() = ids;
        Ok(())
    }
    pub(crate) fn serial_restore_column(
        &self,
        ex: &mut Execution,
        id: u32,
        name: Str,
        value_type: Value,
        title: Str,
        width: i64,
    ) -> Result<(), Exception> {
        let column = SerialColumn {
            id,
            name,
            value_type,
            title,
            width,
        };
        let restored = self.state.columns.serial_restore_column(ex, column);
        restored.map_err(|e| exception(ex, e))
    }
    pub(crate) fn serial_restore_row(
        &self,
        ex: &mut Execution,
        id: usize,
        parent: Option<usize>,
        children: usize,
        materialized: bool,
    ) -> Result<bool, Exception> {
        let state = &self.state;
        let parent = match parent {
            Some(p) => Some(state.node(p).ok_or_else(|| ex.error(changed()))?),
            None => None,
        };
        let level = parent.as_ref().map_or(Some(0), |p| p.level.checked_add(1));
        let level = level.ok_or_else(|| ex.out_of_memory())?;
        let full = |ids: &Vec<usize>| ids.len() == ids.capacity();
        if parent.as_ref().map_or_else(
            || full(&state.roots.borrow()),
            |p| full(&p.children.borrow()),
        ) {
            return Err(ex.out_of_memory());
        }
        let mut live = state
            .live(ex)
            .map_err(|e| exception(ex, e))?
            .saturating_add((std::mem::size_of::<Node>() + 4 * std::mem::size_of::<usize>()) as u64)
            .saturating_add((children as u64).saturating_mul(std::mem::size_of::<usize>() as u64));
        let columns = state
            .columns
            .records(ex, live)
            .map_err(|e| exception(ex, e))?;
        live = live.saturating_add(
            (columns.len() as u64).saturating_mul(std::mem::size_of::<(u32, Value)>() as u64),
        );
        ex.reserve(live)?;
        let mut cells = Cells(vec_with_capacity(ex, columns.len() as u64)?);
        for column in &columns {
            let value = if materialized {
                Value::Undefined
            } else {
                adjust(ex, &column.value_type(), Value::Undefined, live)
                    .map_err(|e| exception(ex, e))?
            };
            live = live.saturating_add(value_bytes(&value));
            ex.reserve(live)?;
            cells.0.push((column.id, value));
        }
        let mut grandchildren = Vec::new();
        grandchildren
            .try_reserve_exact(children)
            .map_err(|_| ex.out_of_memory())?;
        let record = Rc::new(Node {
            id,
            parent: parent.as_ref().map(|p| p.id),
            level,
            active: Cell::new(true),
            materialized: Cell::new(materialized),
            children: RefCell::new(grandchildren),
            cells: RefCell::new(cells),
            bytes: Cell::new(0),
            hash: creation_number(),
            rows_hash: creation_number(),
        });
        let mut arena = state.arena.borrow_mut();
        let Some(slot) = arena.get_mut(id).filter(|slot| slot.is_none()) else {
            return Ok(false);
        };
        *slot = Some(record.clone());
        drop(arena);
        match &parent {
            Some(p) => p.children.borrow_mut().push(id),
            None => state.roots.borrow_mut().push(id),
        }
        state.update_bytes(&record);
        Ok(true)
    }
    pub(crate) fn serial_restore_cell(
        &self,
        ex: &mut Execution,
        id: usize,
        position: usize,
        value: Value,
    ) -> Result<(), Exception> {
        let state = &self.state;
        let live = state.live(ex).map_err(|e| exception(ex, e))?;
        ex.reserve(live.saturating_add(value_bytes(&value)))?;
        let node = state.node(id).ok_or_else(|| ex.error(changed()))?;
        ex.charge(node.cells.borrow().0.len() as u64 + 1)?;
        let mut cells = node.cells.borrow_mut();
        let cell = cells
            .0
            .get_mut(position)
            .ok_or_else(|| ex.error(changed()))?;
        cell.1 = value;
        drop(cells);
        state.update_bytes(&node);
        Ok(())
    }
}
