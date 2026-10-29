use super::*;

struct CopyCells(Cells);
impl Drop for CopyCells {
    fn drop(&mut self) {
        self.0.0.clear();
    }
}
struct CopyArena(Vec<Option<Rc<Node>>>);
impl Drop for CopyArena {
    fn drop(&mut self) {
        for node in self.0.iter().flatten() {
            node.cells.borrow_mut().0.clear();
        }
    }
}
fn copy_value(ex: &mut Execution, value: &Value, live: u64) -> CallResult<Value> {
    if let Value::Number(number) = value {
        ex.charge(number.size_hint_limbs() / 64 + 1)?;
        return number
            .try_clone_with(|limbs| {
                ex.reserve(live.saturating_add((limbs as u64).saturating_mul(4)))
            })
            .map(Value::Number)
            .map_err(|error| match error {
                bsl_prim::number::NumberAllocError::Reserve(error) => error.into(),
                _ => ex.out_of_memory().into(),
            });
    }
    Ok(value.clone())
}
fn text_slack(value: &Value) -> u64 {
    match value {
        Value::String(text) => {
            (text.backing_capacity_units().saturating_sub(text.len()) as u64).saturating_mul(2)
        }
        _ => 0,
    }
}
fn copy_value_bytes(value: &Value) -> u64 {
    match value {
        Value::Number(number) => (number.backing_capacity_limbs() as u64).saturating_mul(4),
        Value::String(text) => (text.len() as u64).saturating_mul(2),
        _ => crate::values::binary_data_bytes_direct(value).map_or(0, |bytes| bytes.len() as u64),
    }
}
fn copy_argument_bytes(ex: &Execution, args: &[Arg]) -> u64 {
    args.iter().fold(
        (args.len() as u64).saturating_mul(std::mem::size_of::<Arg>() as u64),
        |bytes, arg| {
            bytes.saturating_add(match arg {
                Arg::Val(value) => copy_value_bytes(value).saturating_add(match value {
                    Value::Object(object) => std::alloc::Layout::new::<[usize; 2]>()
                        .extend(std::alloc::Layout::for_value(object.as_ref()))
                        .map_or(u64::MAX, |(layout, _)| layout.pad_to_align().size() as u64),
                    _ => 0,
                }),
                Arg::Place(place) => place.numeric_backing_bytes(ex),
                Arg::Omitted => 0,
            })
        },
    )
}
fn copied_node_bytes(node: &Node) -> u64 {
    let cells = node.cells.borrow();
    ((std::mem::size_of::<Node>() + 2 * std::mem::size_of::<usize>()) as u64)
        .saturating_add(
            (node.children.borrow().capacity() as u64)
                .saturating_mul(std::mem::size_of::<usize>() as u64),
        )
        .saturating_add(
            (cells.0.capacity() as u64).saturating_mul(std::mem::size_of::<(u32, Value)>() as u64),
        )
        .saturating_add(cells.0.iter().fold(0u64, |bytes, (_, value)| {
            bytes.saturating_add(copy_value_bytes(value))
        }))
}
fn payload_bytes(value: &Value) -> u64 {
    copy_value_bytes(value).saturating_add(text_slack(value))
}
impl ValueTree {
    pub(super) fn copy(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let revision = self.state.revision.get();
        let schema = self.state.columns.revision();
        ex.charge((args.len() as u64).saturating_mul(2))?;
        let mut live = self
            .state
            .live(ex)?
            .saturating_add(copy_argument_bytes(ex, args))
            .saturating_add(
                (std::mem::size_of::<Self>() + 2 * std::mem::size_of::<usize>()) as u64,
            );
        for arg in args {
            if let Arg::Val(value) = arg {
                live = live.saturating_add(text_slack(value));
            }
        }
        ex.charge(self.state.arena.borrow().len() as u64)?;
        for node in self.state.arena.borrow().iter().flatten() {
            let cells = node.cells.borrow();
            ex.charge(cells.0.len() as u64)?;
            for (_, value) in &cells.0 {
                live = live.saturating_add(text_slack(value));
            }
        }
        let mut schema_slack = 0u64;
        ex.charge(self.state.columns.len() as u64)?;
        for index in 0..self.state.columns.len() {
            let column = self
                .state
                .columns
                .cell_column(ex, &Value::from(index as i64))?;
            schema_slack = schema_slack.saturating_add(column.text_slack_bytes());
        }
        live = live.saturating_add(schema_slack);
        ex.reserve(live)?;
        let columns = self.state.columns.records(ex, live)?;
        live = live.saturating_add(
            (columns.capacity() as u64)
                .saturating_mul(std::mem::size_of::<Rc<ColumnRecord>>() as u64),
        );
        let (copied_columns, copied_live) =
            self.state
                .columns
                .copy_selected(ex, &columns, live.saturating_add(schema_slack))?;
        live = copied_live;
        let count = self.state.arena.borrow().len();
        let root_count = self.state.roots.borrow().len();
        ex.charge(count as u64)?;
        live = live
            .saturating_add(
                (std::mem::size_of::<TreeState>()
                    + std::mem::size_of::<Self>()
                    + 4 * std::mem::size_of::<usize>()) as u64,
            )
            .saturating_add(
                (count as u64).saturating_mul(std::mem::size_of::<Option<Rc<Node>>>() as u64),
            )
            .saturating_add(
                (root_count as u64).saturating_mul(std::mem::size_of::<usize>() as u64),
            );
        ex.reserve(live)?;
        let mut arena = CopyArena(vec_with_capacity(ex, count as u64)?);
        let mut roots = vec_with_capacity(ex, root_count as u64)?;
        let mut records_bytes = 0u64;
        for id in 0..count {
            ex.charge(1)?;
            let Some(source) = self.state.node(id) else {
                arena.0.push(None);
                continue;
            };
            let children = source.children.borrow();
            live = live
                .saturating_add(
                    (std::mem::size_of::<Node>() + 2 * std::mem::size_of::<usize>()) as u64,
                )
                .saturating_add(
                    (children.len() as u64).saturating_mul(std::mem::size_of::<usize>() as u64),
                )
                .saturating_add(
                    (columns.len() as u64)
                        .saturating_mul(std::mem::size_of::<(u32, Value)>() as u64),
                );
            ex.reserve(live)?;
            ex.charge(children.len() as u64)?;
            let mut copied_children = vec_with_capacity(ex, children.len() as u64)?;
            copied_children.extend(children.iter().copied());
            drop(children);
            let mut cells = CopyCells(Cells(vec_with_capacity(ex, columns.len() as u64)?));
            for (new_id, column) in columns.iter().enumerate() {
                let source_cells = source.cells.borrow();
                ex.charge(source_cells.0.len() as u64)?;
                let value = match source_cells
                    .0
                    .iter()
                    .find(|(old_id, _)| *old_id == column.id)
                {
                    Some((_, value)) => copy_value(ex, value, live)?,
                    None => Value::Undefined,
                };
                live = live.saturating_add(payload_bytes(&value));
                ex.reserve(live)?;
                cells.0.0.push((new_id as u32, value));
            }
            ex.charge(columns.len() as u64)?;
            let node = Rc::new(Node {
                id,
                parent: source.parent,
                level: source.level,
                active: Cell::new(true),
                children: RefCell::new(copied_children),
                cells: RefCell::new(Cells(std::mem::take(&mut cells.0.0))),
                bytes: Cell::new(0),
                hash: creation_number(),
                rows_hash: creation_number(),
            });
            node.bytes.set(copied_node_bytes(&node));
            records_bytes = records_bytes.saturating_add(node.bytes.get());
            arena.0.push(Some(node));
        }
        ex.charge(root_count as u64)?;
        roots.extend(self.state.roots.borrow().iter().copied());
        self.state.check_source(revision, schema)?;
        ex.reserve(live)?;
        let state = Rc::new(TreeState {
            columns: copied_columns,
            producer_text_slack: Cell::new(None),
            arena: RefCell::new(std::mem::take(&mut arena.0)),
            roots: RefCell::new(roots),
            revision: Cell::new(0),
            records_bytes: Cell::new(records_bytes),
            hash: creation_number(),
            rows_hash: creation_number(),
        });
        let observer: Rc<dyn SchemaRows> = state.clone();
        state.columns.attach_rows(Rc::downgrade(&observer));
        Ok(Value::Object(Rc::new(Self { state })))
    }
}
