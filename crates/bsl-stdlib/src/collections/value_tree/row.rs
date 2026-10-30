use super::super::columns::COLUMN_BASE;
use super::*;
use bsl_vm::MemberKind;
pub(in crate::collections) struct TreeRow {
    pub(super) state: Rc<TreeState>,
    pub(super) record: Rc<Node>,
}
impl TreeRow {
    pub(in crate::collections) fn prepare_fill(
        &self,
        ex: &mut Execution,
        member: MemberId,
        value: Value,
        retained: u64,
    ) -> CallResult<(Value, [u64; 2])> {
        self.record.writable()?;
        let stamp = [self.state.revision.get(), self.state.columns.revision()];
        let column = self.member_column(ex, member)?;
        ex.charge(self.record.cells.borrow().0.len() as u64)?;
        let adjusted = adjust(ex, &column.value_type(), value, retained)?;
        self.check_fill(member, stamp)?;
        Ok((adjusted, stamp))
    }

    pub(in crate::collections) fn check_fill(
        &self,
        member: MemberId,
        stamp: [u64; 2],
    ) -> CallResult<()> {
        self.state.check_source(stamp[0], stamp[1])?;
        self.record.writable()?;
        if !self
            .record
            .cells
            .borrow()
            .0
            .iter()
            .any(|(id, _)| Some(*id) == member.0.checked_sub(COLUMN_BASE))
        {
            return Err(changed().into());
        }
        Ok(())
    }

    pub(in crate::collections) fn publish_fill(&self, member: MemberId, value: &mut Value) {
        let mut cells = self.record.cells.borrow_mut();
        if let Some((_, cell)) = cells
            .0
            .iter_mut()
            .find(|(id, _)| Some(*id) == member.0.checked_sub(COLUMN_BASE))
        {
            std::mem::swap(cell, value);
        } else {
            unreachable!("prepared tree cell disappeared")
        }
        drop(cells);
        self.state.update_bytes(&self.record);
        self.state.bump();
    }

    fn live(&self, ex: &mut Execution, extra: u64) -> CallResult<u64> {
        let live = self.state.live(ex)?.saturating_add(extra).saturating_add(
            if self.record.active.get() {
                0
            } else {
                self.record.bytes.get()
            },
        );
        ex.reserve(crate::present::collection_presentation_live().saturating_add(live))?;
        Ok(live)
    }
    fn read(&self, ex: &mut Execution, id: u32, extra: u64) -> CallResult<Value> {
        let live = self
            .live(ex, extra)?
            .saturating_add(crate::present::collection_presentation_live());
        let cells = self.record.cells.borrow();
        ex.charge(cells.0.len() as u64)?;
        match cells.0.iter().find(|(key, _)| *key == id) {
            Some((_, v)) => clone_cell(ex, v, live.saturating_add(value_bytes(v))),
            None => Ok(Value::Undefined),
        }
    }
    fn write(
        &self,
        ex: &mut Execution,
        column: Rc<ColumnRecord>,
        value: Value,
        extra: u64,
        args: &[Arg],
    ) -> CallResult<()> {
        self.record.writable()?;
        let rev = self.state.revision.get();
        let schema = self.state.columns.revision();
        let input_bytes = value_bytes(&value);
        let live = self
            .state
            .live(ex)?
            .saturating_add(extra)
            .saturating_add(argument_bytes(ex, args))
            .saturating_add(input_bytes);
        ex.reserve(live)?;
        ex.charge(self.record.cells.borrow().0.len() as u64)?;
        let adjusted = adjust(ex, &column.value_type(), value, live)?;
        let live = self
            .state
            .live(ex)?
            .saturating_add(extra)
            .saturating_add(argument_bytes(ex, args))
            .saturating_add(input_bytes)
            .saturating_add(value_bytes(&adjusted));
        ex.reserve(live)?;
        self.state.check_source(rev, schema)?;
        self.record.writable()?;
        let mut retired = super::super::retire::prepare(ex, live, |visit| {
            let cells = self.record.cells.borrow();
            if let Some((_, old)) = cells.0.iter().find(|(id, _)| *id == column.id) {
                visit(old)?;
            }
            Ok(())
        })?;
        self.state.check_source(rev, schema)?;
        self.record.writable()?;
        let live = self
            .state
            .live(ex)?
            .saturating_add(extra)
            .saturating_add(argument_bytes(ex, args))
            .saturating_add(input_bytes)
            .saturating_add(value_bytes(&adjusted))
            .saturating_add(
                (retired.capacity() as u64).saturating_mul(std::mem::size_of::<Value>() as u64),
            );
        ex.reserve(live)?;
        let old = {
            let mut cells = self.record.cells.borrow_mut();
            let (_, cell) = cells
                .0
                .iter_mut()
                .find(|(id, _)| *id == column.id)
                .ok_or_else(changed)?;
            std::mem::replace(cell, adjusted)
        };
        self.state.update_bytes(&self.record);
        self.state.bump();
        super::super::retire::push(&mut retired, old);
        super::super::retire::run(retired);
        Ok(())
    }
    fn numeric_column(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Rc<ColumnRecord>> {
        let live = self
            .live(ex, argument_bytes(ex, args))?
            .saturating_add(crate::present::collection_presentation_live());
        let n = arg_i64_cached(ex, args.first(), live);
        self.live(ex, argument_bytes(ex, args))?;
        let n = n?;
        let i = usize::try_from(n)
            .ok()
            .filter(|i| *i < self.state.columns.len())
            .ok_or_else(|| HostError::key(msg::RANGE))?;
        self.state.columns.cell_column(ex, &Value::from(i as i64))
    }
    fn indexed_column(
        &self,
        ex: &mut Execution,
        key: &Value,
        extra: u64,
    ) -> CallResult<Rc<ColumnRecord>> {
        let rev = self.state.revision.get();
        let schema = self.state.columns.revision();
        let extra = extra.saturating_add(value_bytes(key));
        let live = self.live(ex, extra)?;
        match key {
            Value::Number(_) => return self.state.columns.cell_column(ex, key),
            Value::String(name) => {
                if let Some(column) = self.state.columns.named_record(
                    ex,
                    name,
                    live.saturating_add(crate::present::collection_presentation_live()),
                )? {
                    return Ok(column);
                }
            }
            _ => {}
        }
        let text = crate::present::present_for_collection(ex, key, live);
        let live = self.live(ex, extra)?;
        let text = text?;
        let units = text.len() as u64;
        ex.charge(units / 32 + 1)?;
        ex.reserve(
            crate::present::collection_presentation_live()
                .saturating_add(live)
                .saturating_add(units.saturating_mul(32).saturating_add(2048)),
        )?;
        self.state.check_source(rev, schema)?;
        Err(HostError::key(msg::PROP_NOT_FOUND)
            .with_arg(text.to_string_lossy())
            .into())
    }
    fn member_column(&self, ex: &mut Execution, m: MemberId) -> CallResult<Rc<ColumnRecord>> {
        ex.charge(self.state.columns.len() as u64)?;
        m.0.checked_sub(COLUMN_BASE)
            .and_then(|id| self.state.columns.record(id))
            .ok_or_else(|| not_implemented(idx("ValueTreeRow"), m).into())
    }
}
impl HostObject for TreeRow {
    fn property_names(&self, ex: &mut Execution, retained: u64) -> CallResult<Option<Vec<Str>>> {
        self.state.columns.property_names(ex, retained).map(Some)
    }
    fn session_edges(&self, visit: &mut dyn bsl_vm::SessionEdges) -> bool {
        visit.backing(self.state.clone()) && visit.backing(self.record.clone())
    }
    fn session_clear(&self) {}
    fn session_transfer_supported(&self) -> bool {
        true
    }
    fn session_transfer(
        &self,
        out: &mut dyn bsl_vm::SessionExport,
    ) -> Result<Option<Box<dyn bsl_vm::SessionObject>>, bsl_vm::Exception> {
        super::session::capture(&self.state, Some(&self.record), 1, out).map(Some)
    }
    fn retained_memory(
        &self,
        visit: &mut dyn bsl_vm::RetainedMemory,
    ) -> Result<bool, bsl_vm::Exception> {
        crate::collections::accounting::project(self, visit)
    }
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(idx("ValueTreeRow"))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        match property_name(idx("ValueTreeRow"), m) {
            Some("Parent") => self.state.parent_value(ex, self.record.parent),
            Some("Rows") => self.state.rows(ex, Some(self.record.clone())),
            _ => {
                let column = self.member_column(ex, m)?;
                self.read(ex, column.id, 0)
            }
        }
    }
    fn get_with_retained(
        &self,
        ex: &mut Execution,
        m: MemberId,
        retained: u64,
    ) -> CallResult<Value> {
        if matches!(
            property_name(idx("ValueTreeRow"), m),
            Some("Parent" | "Rows")
        ) {
            return self.get(ex, m);
        }
        let column = self.member_column(ex, m)?;
        let cells = self.record.cells.borrow();
        ex.charge(cells.0.len() as u64)?;
        match cells.0.iter().find(|(key, _)| *key == column.id) {
            Some((_, value)) => clone_cell(ex, value, retained),
            None => Ok(Value::Undefined),
        }
    }
    fn set(&self, ex: &mut Execution, m: MemberId, value: Value) -> CallResult<()> {
        if matches!(
            property_name(idx("ValueTreeRow"), m),
            Some("Parent" | "Rows")
        ) {
            return Err(HostError::key(msg::PROP_NOT_WRITABLE).raw().into());
        }
        let column = self.member_column(ex, m)?;
        self.write(ex, column, value, 0, &[])
    }
    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        match method_name(idx("ValueTreeRow"), m) {
            Some("Owner") => self.state.tree(ex),
            Some("Level") => Ok(Value::from(self.record.level)),
            Some("Get") | Some("Set") => {
                let rev = self.state.revision.get();
                let schema = self.state.columns.revision();
                let column = self.numeric_column(ex, args)?;
                if method_name(idx("ValueTreeRow"), m) == Some("Set") {
                    self.record.writable()?;
                    let live = self
                        .state
                        .live(ex)?
                        .saturating_add(argument_bytes(ex, args));
                    let value = arg_value_cached(ex, args.get(1), live)?;
                    self.state.check_source(rev, schema)?;
                    self.write(ex, column, value, 0, args)?;
                    Ok(Value::Undefined)
                } else {
                    self.state.check_source(rev, schema)?;
                    self.read(ex, column.id, argument_bytes(ex, args))
                }
            }
            _ => Err(not_implemented(idx("ValueTreeRow"), m).into()),
        }
    }
    fn find_member(&self, name: &str) -> Option<MemberId> {
        self.state.columns.find_member(name)
    }
    fn find_member_guarded(
        &self,
        ex: &mut Execution,
        name: &str,
    ) -> Result<Option<MemberId>, bsl_vm::Exception> {
        self.state.columns.find_member_guarded(ex, name)
    }
    fn member_kind(&self, m: MemberId) -> Option<MemberKind> {
        self.state.columns.record(m.0.checked_sub(COLUMN_BASE)?)?;
        Some(MemberKind::Property {
            readable: true,
            writable: true,
        })
    }
    fn index_get(&self, ex: &mut Execution, key: &Value) -> CallResult<Value> {
        let column = self.indexed_column(ex, key, 0)?;
        self.read(ex, column.id, value_bytes(key))
    }
    fn index_set(&self, ex: &mut Execution, key: &Value, value: Value) -> CallResult<()> {
        let rev = self.state.revision.get();
        let schema = self.state.columns.revision();
        let column = self.indexed_column(ex, key, value_bytes(&value))?;
        self.state.check_source(rev, schema)?;
        self.write(ex, column, value, value_bytes(key), &[])
    }
    fn iter_len(&self, _: &mut Execution) -> Option<usize> {
        Some(self.state.columns.len())
    }
    fn iter_get(&self, ex: &mut Execution, i: usize) -> CallResult<Value> {
        self.index_get(ex, &Value::from(i as i64))
    }
    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<Str>> {
        Ok(Some(type_name(ex, idx("ValueTreeRow"))))
    }
    fn boost_hash(&self) -> Option<i32> {
        Some(self.record.hash)
    }
    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        Some(other.as_any().downcast_ref::<Self>().is_some_and(|o| {
            Rc::ptr_eq(&self.state, &o.state) && Rc::ptr_eq(&self.record, &o.record)
        }))
    }
}
