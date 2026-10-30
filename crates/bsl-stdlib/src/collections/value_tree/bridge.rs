use super::*;

fn text_slack(value: &Value) -> u64 {
    match value {
        Value::String(text) => super::super::retained::text_extra(text),
        _ => 0,
    }
}
fn state_live(state: &TreeState, ex: &mut Execution) -> CallResult<u64> {
    let base = state.live(ex)?;
    let revision = state.revision.get();
    let schema = state.columns.revision();
    if let Some((old_revision, old_schema, slack)) = state.producer_text_slack.get()
        && old_revision == revision
        && old_schema == schema
    {
        ex.charge(1)?;
        return Ok(base.saturating_add(slack));
    }
    let arena = state.arena.borrow();
    ex.charge(arena.len() as u64)?;
    let mut slack = 0u64;
    for node in arena.iter().flatten() {
        let cells = node.cells.borrow();
        ex.charge(cells.0.len() as u64)?;
        for (_, value) in &cells.0 {
            slack = slack.saturating_add(text_slack(value));
        }
    }
    ex.charge(state.columns.len() as u64)?;
    for index in 0..state.columns.len() {
        let record = state.columns.cell_column(ex, &Value::from(index as i64))?;
        slack = slack.saturating_add(record.text_slack_bytes());
    }
    state
        .producer_text_slack
        .set(Some((revision, schema, slack)));
    Ok(base.saturating_add(slack))
}
fn value_bytes_direct(value: &Value) -> u64 {
    match value {
        Value::Number(number) => (number.backing_capacity_limbs() as u64).saturating_mul(4),
        Value::String(text) => (text.len() as u64).saturating_mul(2),
        _ => crate::values::binary_data_bytes_direct(value).map_or(0, |bytes| bytes.len() as u64),
    }
}
fn wrapper_direct(object: &dyn HostObject) -> Option<(&Rc<TreeState>, Option<&Rc<Node>>)> {
    let actual: &dyn Any = object;
    if let Some(tree) = actual.downcast_ref::<ValueTree>() {
        Some((&tree.state, None))
    } else if let Some(row) = actual.downcast_ref::<TreeRow>() {
        Some((&row.state, Some(&row.record)))
    } else {
        actual
            .downcast_ref::<TreeRows>()
            .map(|rows| (&rows.state, rows.parent.as_ref()))
    }
}
fn detached_live(node: &Node, ex: &mut Execution) -> CallResult<u64> {
    let cells = node.cells.borrow();
    ex.charge(cells.0.len() as u64)?;
    Ok(
        ((std::mem::size_of::<Node>() + 2 * std::mem::size_of::<usize>()) as u64)
            .saturating_add(
                (node.children.borrow().capacity() as u64)
                    .saturating_mul(std::mem::size_of::<usize>() as u64),
            )
            .saturating_add(
                (cells.0.capacity() as u64)
                    .saturating_mul(std::mem::size_of::<(u32, Value)>() as u64),
            )
            .saturating_add(cells.0.iter().fold(0u64, |sum, (_, value)| {
                sum.saturating_add(value_bytes_direct(value))
                    .saturating_add(text_slack(value))
            })),
    )
}
fn original_live(receiver: &Rc<TreeState>, ex: &mut Execution, value: &Value) -> CallResult<u64> {
    let mut live = (std::mem::size_of::<Value>() as u64)
        .saturating_add(value_bytes_direct(value))
        .saturating_add(text_slack(value));
    if let Value::Object(object) = value {
        live = live.saturating_add(
            std::alloc::Layout::new::<[usize; 2]>()
                .extend(std::alloc::Layout::for_value(object.as_ref()))
                .map_or(u64::MAX, |(layout, _)| layout.pad_to_align().size() as u64),
        );
        if let Some((state, own)) = wrapper_direct(object.as_ref()) {
            if !Rc::ptr_eq(receiver, state) {
                live = live.saturating_add(state_live(state, ex)?);
            }
            if let Some(record) = own {
                let represented = state
                    .arena
                    .borrow()
                    .get(record.id)
                    .and_then(Option::as_ref)
                    .is_some_and(|current| Rc::ptr_eq(current, record));
                if !represented {
                    live = live.saturating_add(detached_live(record, ex)?);
                }
            }
        }
    }
    Ok(live)
}
fn diagnostic(
    ex: &mut Execution,
    live: u64,
    error: fn(usize) -> HostError,
) -> CallResult<Vec<Value>> {
    ex.reserve(live.saturating_add(2048))?;
    Err(error(1).into())
}
impl ValueTree {
    fn producer_live(&self, ex: &mut Execution) -> CallResult<u64> {
        Ok(state_live(&self.state, ex)?.saturating_add(
            (std::mem::size_of::<Self>() + 2 * std::mem::size_of::<usize>()) as u64,
        ))
    }
    pub fn columns(&self, ex: &mut Execution) -> CallResult<Value> {
        let live = self.producer_live(ex)?;
        ex.reserve(live)?;
        Ok(Value::Object(self.state.columns.clone()))
    }
    pub fn roots(&self, ex: &mut Execution) -> CallResult<Vec<Value>> {
        let live = self.producer_live(ex)?;
        self.produce(ex, &self.state.roots.borrow(), None, live)
    }
    pub fn children(&self, ex: &mut Execution, value: &Value) -> CallResult<Vec<Value>> {
        let live = self
            .producer_live(ex)?
            .saturating_add(original_live(&self.state, ex, value)?);
        ex.reserve(live)?;
        let Some(row) = value.as_object().and_then(|object| {
            let actual: &dyn Any = object.as_ref();
            actual.downcast_ref::<TreeRow>()
        }) else {
            return diagnostic(ex, live, type_mismatch);
        };
        ex.charge(1)?;
        if !Rc::ptr_eq(&self.state, &row.state)
            || !row.record.active.get()
            || !self
                .state
                .arena
                .borrow()
                .get(row.record.id)
                .and_then(Option::as_ref)
                .is_some_and(|current| Rc::ptr_eq(current, &row.record))
        {
            return diagnostic(ex, live, value_mismatch);
        }
        self.produce(ex, &row.record.children.borrow(), Some(row.record.id), live)
    }
    fn produce(
        &self,
        ex: &mut Execution,
        ids: &[usize],
        parent: Option<usize>,
        live: u64,
    ) -> CallResult<Vec<Value>> {
        let revision = self.state.revision.get();
        let schema = self.state.columns.revision();
        ex.reserve(live)?;
        ex.charge(ids.len() as u64)?;
        let arena = self.state.arena.borrow();
        for id in ids {
            if !arena
                .get(*id)
                .and_then(Option::as_ref)
                .is_some_and(|record| {
                    record.active.get() && record.id == *id && record.parent == parent
                })
            {
                ex.reserve(live.saturating_add(2048))?;
                return Err(changed().into());
            }
        }
        let wrappers = (ids.len() as u64).saturating_mul(
            (std::mem::size_of::<TreeRow>() + 2 * std::mem::size_of::<usize>()) as u64,
        );
        let base = live.saturating_add(wrappers);
        ex.reserve(base.saturating_add(
            (ids.len() as u64).saturating_mul(std::mem::size_of::<Value>() as u64),
        ))?;
        if self.state.revision.get() != revision || self.state.columns.revision() != schema {
            ex.reserve(live.saturating_add(2048))?;
            return Err(changed().into());
        }
        let mut result = vec_with_capacity(ex, ids.len() as u64)?;
        ex.reserve(base.saturating_add(
            (result.capacity() as u64).saturating_mul(std::mem::size_of::<Value>() as u64),
        ))?;
        for id in ids {
            if let Some(Some(record)) = arena.get(*id) {
                result.push(Value::Object(Rc::new(TreeRow {
                    state: self.state.clone(),
                    record: record.clone(),
                })));
            }
        }
        Ok(result)
    }
}
