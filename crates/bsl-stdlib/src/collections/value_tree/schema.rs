use super::*;
struct PreparedRows {
    state: Rc<TreeState>,
    revision: u64,
    schema: u64,
    cells: Vec<(Rc<Node>, Cells)>,
    clear: bool,
    retired: Vec<Value>,
}
impl PreparedSchemaChange for PreparedRows {
    fn check(&self) -> CallResult<()> {
        self.state.check_source(self.revision, self.schema)
    }
    fn commit(mut self: Box<Self>) {
        if self.clear {
            let mut arena = self.state.arena.borrow_mut();
            self.state.roots.borrow_mut().clear();
            self.state.records_bytes.set(0);
            for slot in arena.iter_mut() {
                if let Some(node) = slot.take() {
                    node.active.set(false);
                    node.children.borrow_mut().clear();
                    if Rc::strong_count(&node) == 1 {
                        node.cells.borrow_mut().drain_into(&mut self.retired, true);
                    }
                }
            }
        } else {
            for (node, cells) in &mut self.cells {
                std::mem::swap(&mut *node.cells.borrow_mut(), cells);
                self.state.update_bytes(node);
                cells.drain_into(&mut self.retired, true);
            }
        }
        self.state.bump();
        super::super::retire::run(std::mem::take(&mut self.retired));
    }
}
impl SchemaRows for TreeState {
    fn revision(&self) -> u64 {
        self.revision.get()
    }
    fn check(&self, revision: u64) -> CallResult<()> {
        if self.revision.get() != revision {
            Err(changed().into())
        } else {
            Ok(())
        }
    }
    fn owned_bytes(&self) -> u64 {
        self.bytes()
    }
    fn prepare(
        self: Rc<Self>,
        ex: &mut Execution,
        change: SchemaChange,
        live: u64,
    ) -> CallResult<Box<dyn PreparedSchemaChange>> {
        let revision = self.revision.get();
        let schema = self.columns.revision();
        let count = self.arena.borrow().len();
        let mut live = live
            .saturating_add(self.bytes())
            .saturating_add(std::mem::size_of::<PreparedRows>() as u64)
            .saturating_add((count as u64).saturating_mul(
                (std::mem::size_of::<Rc<Node>>() + std::mem::size_of::<(Rc<Node>, Cells)>()) as u64,
            ));
        ex.reserve(live)?;
        ex.charge(count as u64)?;
        let clear = matches!(change, SchemaChange::Clear);
        let mut prepared = PreparedRows {
            state: self.clone(),
            revision,
            schema,
            cells: vec_with_capacity(ex, if clear { 0 } else { count as u64 })?,
            clear,
            retired: Vec::new(),
        };
        if !clear {
            let mut nodes = vec_with_capacity(ex, count as u64)?;
            nodes.extend(self.arena.borrow().iter().flatten().cloned());
            for node in nodes {
                let n = node.cells.borrow().0.len();
                let cap = n + usize::from(matches!(change, SchemaChange::Insert { .. }));
                live =
                    live.saturating_add(
                        (cap as u64).saturating_mul(std::mem::size_of::<(u32, Value)>() as u64),
                    );
                ex.reserve(live)?;
                ex.charge(n as u64 + 1)?;
                let mut cells = Cells(vec_with_capacity(ex, cap as u64)?);
                for (id, value) in &node.cells.borrow().0 {
                    if matches!(change, SchemaChange::Delete { id: removed } if *id == removed) {
                        continue;
                    }
                    live = live.saturating_add(scalar_clone_bytes(value));
                    cells.0.push((*id, clone_cell(ex, value, live)?));
                }
                if let SchemaChange::Insert { id, ref value_type } = change {
                    let value = adjust(ex, value_type, Value::Undefined, live)?;
                    live = live.saturating_add(value_bytes(&value));
                    ex.reserve(live)?;
                    cells.0.push((id, value));
                }
                prepared.cells.push((node, cells));
                self.check_source(revision, schema)?;
            }
        }
        prepared.retired =
            super::super::retire::prepare(ex, live, |visit| self.visit_values(visit))?;
        Ok(Box::new(prepared))
    }
}
