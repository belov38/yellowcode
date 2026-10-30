use super::*;
pub(super) struct TreeRows {
    pub(super) state: Rc<TreeState>,
    pub(super) parent: Option<Rc<Node>>,
}
fn grow<T>(ex: &mut Execution, values: &mut Vec<T>, live: u64) -> CallResult<()> {
    if values.len() == values.capacity() {
        let cap = values
            .capacity()
            .checked_mul(2)
            .and_then(|n| n.checked_add(4))
            .ok_or_else(|| ex.out_of_memory())?;
        ex.reserve(
            live.saturating_add((cap as u64).saturating_mul(std::mem::size_of::<T>() as u64)),
        )?;
        values
            .try_reserve_exact(cap - values.len())
            .map_err(|_| ex.out_of_memory())?;
    }
    Ok(())
}
impl TreeRows {
    pub(super) fn with_ids<T>(&self, f: impl FnOnce(&Vec<usize>) -> T) -> T {
        match &self.parent {
            Some(p) => f(&p.children.borrow()),
            None => f(&self.state.roots.borrow()),
        }
    }
    fn with_ids_mut<T>(&self, f: impl FnOnce(&mut Vec<usize>) -> T) -> T {
        match &self.parent {
            Some(p) => f(&mut p.children.borrow_mut()),
            None => f(&mut self.state.roots.borrow_mut()),
        }
    }
    pub(super) fn len(&self) -> usize {
        self.with_ids(Vec::len)
    }
    pub(super) fn writable(&self) -> CallResult<()> {
        self.parent.as_ref().map_or(Ok(()), |p| p.writable())
    }
    fn at(&self, ex: &mut Execution, index: usize) -> CallResult<Value> {
        let node = self
            .with_ids(|ids| ids.get(index).and_then(|id| self.state.node(*id)))
            .ok_or_else(|| HostError::key(msg::INDEX_OFFBOUND))?;
        self.state.row(ex, node)
    }
    fn numeric(&self, ex: &mut Execution, args: &[Arg], insert: bool) -> CallResult<usize> {
        let rev = self.state.revision.get();
        let schema = self.state.columns.revision();
        let live = self
            .state
            .live(ex)?
            .saturating_add(argument_bytes(ex, args));
        let n = arg_i64_cached(ex, args.first(), live)?;
        let live = self
            .state
            .live(ex)?
            .saturating_add(argument_bytes(ex, args));
        ex.reserve(live)?;
        self.state.check_source(rev, schema)?;
        usize::try_from(n)
            .ok()
            .filter(|i| *i < self.len() || (insert && *i == self.len()))
            .ok_or_else(|| HostError::key(msg::RANGE).into())
    }
    fn row_index(&self, ex: &mut Execution, value: &Value) -> CallResult<Option<usize>> {
        let row = value
            .as_object()
            .and_then(|o| o.as_any().downcast_ref::<TreeRow>())
            .ok_or_else(|| type_mismatch(1))?;
        if !Rc::ptr_eq(&self.state, &row.state)
            || !row.record.active.get()
            || row.record.parent != self.parent.as_ref().map(|p| p.id)
        {
            return Ok(None);
        }
        ex.charge(self.len() as u64)?;
        Ok(self.with_ids(|ids| ids.iter().position(|id| *id == row.record.id)))
    }
    fn selector(&self, ex: &mut Execution, value: &Value) -> CallResult<usize> {
        if let Value::Number(n) = value {
            return position(n)
                .filter(|i| *i < self.len())
                .ok_or_else(|| HostError::key(msg::RANGE).into());
        }
        self.row_index(ex, value)?
            .ok_or_else(|| value_mismatch(1).into())
    }
    fn add(&self, ex: &mut Execution, args: &[Arg], insert: bool) -> CallResult<Value> {
        self.writable()?;
        let rev = self.state.revision.get();
        let schema = self.state.columns.revision();
        let i = if insert {
            self.numeric(ex, args, true)?
        } else {
            self.len()
        };
        let id = self.state.arena.borrow().len();
        id.checked_add(1).ok_or_else(|| ex.out_of_memory())?;
        let level = self
            .parent
            .as_ref()
            .map_or(Some(0), |p| p.level.checked_add(1))
            .ok_or_else(|| ex.out_of_memory())?;
        let mut live = self
            .state
            .live(ex)?
            .saturating_add(argument_bytes(ex, args));
        let columns = self.state.columns.records(ex, live)?;
        live = live
            .saturating_add((columns.len() as u64).saturating_mul(
                (std::mem::size_of::<Rc<ColumnRecord>>() + std::mem::size_of::<(u32, Value)>())
                    as u64,
            ))
            .saturating_add(
                (std::mem::size_of::<Node>()
                    + std::mem::size_of::<TreeRow>()
                    + 4 * std::mem::size_of::<usize>()) as u64,
            );
        ex.reserve(live)?;
        ex.charge(columns.len() as u64 + (self.len() - i) as u64 + 1)?;
        let mut cells = Cells(vec_with_capacity(ex, columns.len() as u64)?);
        for column in columns {
            let value = adjust(ex, &column.value_type(), Value::Undefined, live)?;
            live = live.saturating_add(value_bytes(&value));
            ex.reserve(live)?;
            cells.0.push((column.id, value));
            self.state.check_source(rev, schema)?;
        }
        let record = Rc::new(Node {
            id,
            parent: self.parent.as_ref().map(|p| p.id),
            level,
            active: Cell::new(true),
            children: RefCell::new(Vec::new()),
            cells: RefCell::new(cells),
            bytes: Cell::new(0),
            hash: creation_number(),
            rows_hash: creation_number(),
        });
        record.bytes.set(record.bytes());
        let handle = self.state.row(ex, record.clone())?;
        self.state.check_source(rev, schema)?;
        self.writable()?;
        let before = self.state.bytes();
        grow(ex, &mut self.state.arena.borrow_mut(), live)?;
        let live = live.saturating_add(self.state.bytes().saturating_sub(before));
        self.with_ids_mut(|ids| grow(ex, ids, live))?;
        self.state.arena.borrow_mut().push(Some(record.clone()));
        self.with_ids_mut(|ids| ids.insert(i, id));
        self.state.records_bytes.set(
            self.state
                .records_bytes
                .get()
                .saturating_add(record.bytes.get()),
        );
        if let Some(parent) = &self.parent {
            self.state.update_bytes(parent);
        }
        self.state.bump();
        Ok(handle)
    }
    fn detach(&self, ex: &mut Execution, one: Option<usize>, extra: u64) -> CallResult<Value> {
        self.writable()?;
        let count = self.state.arena.borrow().len();
        let live = self.state.live(ex)?.saturating_add(extra).saturating_add(
            (count as u64).saturating_mul(
                (2 * std::mem::size_of::<usize>() + std::mem::size_of::<Rc<Node>>()) as u64,
            ),
        );
        ex.reserve(live)?;
        let mut todo = vec_with_capacity(ex, count as u64)?;
        self.with_ids(|ids| {
            if let Some(i) = one {
                todo.push(ids[i]);
            } else {
                todo.extend(ids.iter().copied());
            }
        });
        let mut nodes = vec_with_capacity(ex, count as u64)?;
        while let Some(id) = todo.pop() {
            ex.charge(1)?;
            let node = self.state.node(id).ok_or_else(changed)?;
            ex.charge(node.children.borrow().len() as u64)?;
            todo.extend(node.children.borrow().iter().copied());
            nodes.push(node);
        }
        let mut retired = super::super::retire::prepare(ex, live, |visit| {
            for node in &nodes {
                for (_, value) in &node.cells.borrow().0 {
                    visit(value)?;
                }
            }
            Ok(())
        })?;
        ex.charge(self.len() as u64)?;
        self.with_ids_mut(|ids| {
            if let Some(i) = one {
                ids.remove(i);
            } else {
                ids.clear();
            }
        });
        for node in nodes {
            self.state.arena.borrow_mut()[node.id] = None;
            node.active.set(false);
            node.children.borrow_mut().clear();
            self.state.records_bytes.set(
                self.state
                    .records_bytes
                    .get()
                    .saturating_sub(node.bytes.get()),
            );
            if Rc::strong_count(&node) == 1 {
                node.cells.borrow_mut().drain_into(&mut retired, true);
            }
        }
        if let Some(parent) = &self.parent {
            self.state.update_bytes(parent);
        }
        self.state.bump();
        super::super::retire::run(retired);
        Ok(Value::Undefined)
    }
}
impl HostObject for TreeRows {
    fn session_edges(&self, visit: &mut dyn bsl_vm::SessionEdges) -> bool {
        visit.backing(self.state.clone())
            && self
                .parent
                .as_ref()
                .is_none_or(|parent| visit.backing(parent.clone()))
    }
    fn session_clear(&self) {}
    fn session_transfer_supported(&self) -> bool {
        true
    }
    fn session_transfer(
        &self,
        out: &mut dyn bsl_vm::SessionExport,
    ) -> Result<Option<Box<dyn bsl_vm::SessionObject>>, bsl_vm::Exception> {
        super::session::capture(&self.state, self.parent.as_ref(), 2, out).map(Some)
    }
    fn retained_memory(
        &self,
        visit: &mut dyn bsl_vm::RetainedMemory,
    ) -> Result<bool, bsl_vm::Exception> {
        crate::collections::accounting::project(self, visit)
    }
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(idx("ValueTreeRowCollection"))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        if property_name(idx("ValueTreeRowCollection"), m) == Some("Parent") {
            match &self.parent {
                Some(node) => self.state.row(ex, node.clone()),
                None => Ok(Value::Undefined),
            }
        } else {
            Err(not_implemented(idx("ValueTreeRowCollection"), m).into())
        }
    }
    fn set(&self, _: &mut Execution, _: MemberId, _: Value) -> CallResult<()> {
        Err(HostError::key(msg::PROP_NOT_WRITABLE).raw().into())
    }
    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        let extra = argument_bytes(ex, args);
        match method_name(idx("ValueTreeRowCollection"), m) {
            Some("Count") => Ok(Value::from(self.len() as i64)),
            Some("UnloadColumn") => self.unload_column(ex, args),
            Some("LoadColumn") => self.load_column(ex, args),
            Some("Find") => self.find(ex, args),
            Some("FindRows") => self.find_rows(ex, args),
            Some("Total") => self.total(ex, args),
            Some("Sort") => self.sort(ex, args),
            Some("Add") => self.add(ex, args, false),
            Some("Insert") => self.add(ex, args, true),
            Some("Get") => {
                let i = self.numeric(ex, args, false)?;
                self.at(ex, i)
            }
            Some("IndexOf") => {
                let live = self.state.live(ex)?.saturating_add(extra);
                let value = arg_value_cached(ex, args.first(), live)?;
                let live = self
                    .state
                    .live(ex)?
                    .saturating_add(argument_bytes(ex, args))
                    .saturating_add(value_bytes(&value));
                ex.reserve(live)?;
                Ok(Value::from(
                    self.row_index(ex, &value)?.map_or(-1, |i| i as i64),
                ))
            }
            Some("Clear") => self.detach(ex, None, extra),
            Some("Delete") | Some("Move") => {
                self.writable()?;
                let rev = self.state.revision.get();
                let schema = self.state.columns.revision();
                let live = self.state.live(ex)?.saturating_add(extra);
                let value = arg_value_cached(ex, args.first(), live)?;
                let live = self
                    .state
                    .live(ex)?
                    .saturating_add(argument_bytes(ex, args))
                    .saturating_add(value_bytes(&value));
                ex.reserve(live)?;
                let offset = if method_name(idx("ValueTreeRowCollection"), m) == Some("Move") {
                    Some(arg_i64_cached(ex, args.get(1), live)?)
                } else {
                    None
                };
                self.state.check_source(rev, schema)?;
                self.writable()?;
                let extra = argument_bytes(ex, args).saturating_add(value_bytes(&value));
                let live = self.state.live(ex)?.saturating_add(extra);
                ex.reserve(live)?;
                let i = self.selector(ex, &value)?;
                if let Some(offset) = offset {
                    let dest = (i as i64)
                        .checked_add(offset)
                        .and_then(|n| usize::try_from(n).ok())
                        .filter(|n| *n < self.len())
                        .ok_or_else(|| value_mismatch(2))?;
                    ex.charge(self.len() as u64)?;
                    self.with_ids_mut(|ids| {
                        let id = ids.remove(i);
                        ids.insert(dest, id);
                    });
                    self.state.bump();
                    Ok(Value::Undefined)
                } else {
                    self.detach(ex, Some(i), extra)
                }
            }
            _ => Err(not_implemented(idx("ValueTreeRowCollection"), m).into()),
        }
    }
    fn index_get(&self, ex: &mut Execution, key: &Value) -> CallResult<Value> {
        let Value::Number(n) = key else {
            return Err(super::super::columns::missing_field(ex, key)?.into());
        };
        self.at(
            ex,
            position(n).ok_or_else(|| HostError::key(msg::INDEX_OFFBOUND))?,
        )
    }
    fn index_set(&self, _: &mut Execution, _: &Value, _: Value) -> CallResult<()> {
        Err(HostError::key(msg::INDEXED_READONLY).into())
    }
    fn iter_len(&self, _: &mut Execution) -> Option<usize> {
        Some(self.len())
    }
    fn iter_get(&self, ex: &mut Execution, i: usize) -> CallResult<Value> {
        self.at(ex, i)
    }
    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<Str>> {
        Ok(Some(type_name(ex, idx("ValueTreeRowCollection"))))
    }
    fn boost_hash(&self) -> Option<i32> {
        Some(
            self.parent
                .as_ref()
                .map_or(self.state.rows_hash, |p| p.rows_hash),
        )
    }
    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        Some(other.as_any().downcast_ref::<Self>().is_some_and(|o| {
            Rc::ptr_eq(&self.state, &o.state)
                && self.parent.as_ref().map(|p| p.id) == o.parent.as_ref().map(|p| p.id)
        }))
    }
}
