use super::*;
use crate::collections::columns::COLUMN_BASE;
use bsl_vm::MemberKind;

pub(super) struct Cells(pub(super) Vec<(u32, Value)>);
impl Cells {
    pub(super) fn retire_into(&mut self, out: &mut Vec<Value>) {
        for (_, value) in self.0.drain(..) {
            super::super::retire::push(out, value);
        }
    }

    fn take_values(&mut self) -> Vec<Value> {
        if self.0.is_empty() {
            return Vec::new();
        }
        std::mem::take(&mut self.0)
            .into_iter()
            .map(|(_, v)| v)
            .collect()
    }
}
impl Drop for Cells {
    fn drop(&mut self) {
        release(self.take_values());
    }
}
pub(super) struct RowRecord {
    pub(super) id: u64,
    pub(super) hash: i32,
    pub(super) member: Cell<bool>,
    pub(super) cells: RefCell<Cells>,
    pub(super) bytes: Cell<u64>,
}
impl RowRecord {
    pub(super) fn calculate_bytes(&self) -> u64 {
        let cells = self.cells.borrow();
        let base = (std::mem::size_of::<Self>() + 2 * std::mem::size_of::<usize>()) as u64
            + (cells.0.capacity() as u64)
                .saturating_mul(std::mem::size_of::<(u32, Value)>() as u64);
        cells
            .0
            .iter()
            .fold(base, |n, (_, v)| n.saturating_add(value_bytes(v)))
    }
    pub(super) fn take_values(&self) -> Vec<Value> {
        self.cells.borrow_mut().take_values()
    }
}
impl Drop for RowRecord {
    fn drop(&mut self) {
        release(self.cells.get_mut().take_values());
    }
}
pub(crate) struct TableRow {
    pub(super) state: Rc<TableState>,
    pub(super) record: Rc<RowRecord>,
}
impl TableRow {
    pub(in crate::collections) fn retirement_work(&self) -> u64 {
        (self.state.len() as u64).saturating_add(self.state.columns.len() as u64)
    }
    pub(in crate::collections) fn visit_values(
        &self,
        visit: &mut super::super::retire::Visitor<'_>,
    ) -> Result<(), bsl_vm::Exception> {
        self.state.visit_values(visit)?;
        for (_, value) in &self.record.cells.borrow().0 {
            visit(value)?;
        }
        Ok(())
    }
    pub(in crate::collections) fn retire_into(&self, out: &mut Vec<Value>) {
        if Rc::strong_count(&self.state) == 1 {
            self.state.retire_into(Some(&self.record), out);
        }
        if Rc::strong_count(&self.record) == 1 {
            self.record.cells.borrow_mut().retire_into(out);
        }
    }

    fn writable(&self) -> CallResult<()> {
        if !self.record.member.get() {
            return Err(HostError::key("core85:IDS_ERR_DELETED_ROW").into());
        }
        Ok(())
    }
    fn read(&self, ex: &mut Execution, id: u32) -> CallResult<Value> {
        let cells = self.record.cells.borrow();
        ex.charge(cells.0.len() as u64)?;
        match cells.0.iter().find(|(key, _)| *key == id) {
            Some((_, value)) => clone_cell(ex, value, 0),
            None => Ok(Value::Undefined),
        }
    }
    fn write(&self, ex: &mut Execution, column: Rc<ColumnRecord>, value: Value) -> CallResult<()> {
        self.writable()?;
        let revision = self.state.revision.get();
        let schema = self.state.columns.revision();
        let live = self.state.live(ex)?;
        ex.charge(self.record.cells.borrow().0.len() as u64)?;
        let adjusted = adjust_cell(ex, &column.value_type(), value, live)?;
        ex.reserve(live.saturating_add(value_bytes(&adjusted)))?;
        self.state.check_revision(revision, schema)?;
        self.writable()?;
        let old = {
            let mut cells = self.record.cells.borrow_mut();
            let (_, cell) = cells
                .0
                .iter_mut()
                .find(|(id, _)| *id == column.id)
                .ok_or_else(changed)?;
            std::mem::replace(cell, adjusted)
        };
        let before = self.record.bytes.get();
        let after = self.record.calculate_bytes();
        self.record.bytes.set(after);
        self.state.records_bytes.set(
            self.state
                .records_bytes
                .get()
                .saturating_sub(before)
                .saturating_add(after),
        );
        self.state.bump();
        release(vec![old]);
        Ok(())
    }
    fn numeric_column(
        &self,
        ex: &mut Execution,
        arg: Option<&Arg>,
    ) -> CallResult<Rc<ColumnRecord>> {
        let n = arg_i64_cached(ex, arg, 0)?;
        let i = usize::try_from(n)
            .ok()
            .filter(|i| *i < self.state.columns.len())
            .ok_or_else(|| HostError::key(msg::RANGE))?;
        self.state.columns.cell_column(ex, &Value::from(i as i64))
    }
    pub(crate) fn take_values(&self) -> Vec<Value> {
        let mut values = if Rc::strong_count(&self.state) == 1 {
            self.state.take_values(Some(&self.record))
        } else {
            Vec::new()
        };
        if Rc::strong_count(&self.record) == 1 {
            values.extend(self.record.take_values());
        }
        values
    }
}
impl Drop for TableRow {
    fn drop(&mut self) {
        release(self.take_values());
    }
}
impl HostObject for TableRow {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(idx("ValueTableRow"))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        ex.charge(self.state.columns.len() as u64)?;
        let column =
            m.0.checked_sub(COLUMN_BASE)
                .and_then(|id| self.state.columns.record(id))
                .ok_or_else(|| not_implemented(idx("ValueTableRow"), m))?;
        self.read(ex, column.id)
    }
    fn set(&self, ex: &mut Execution, m: MemberId, value: Value) -> CallResult<()> {
        ex.charge(self.state.columns.len() as u64)?;
        let column =
            m.0.checked_sub(COLUMN_BASE)
                .and_then(|id| self.state.columns.record(id))
                .ok_or_else(|| not_implemented(idx("ValueTableRow"), m))?;
        self.write(ex, column, value)
    }
    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        match method_name(idx("ValueTableRow"), m) {
            Some("Owner") => self.state.table(ex),
            Some("Get") | Some("Set") => {
                let revision = self.state.revision.get();
                let schema = self.state.columns.revision();
                let column = self.numeric_column(ex, args.first())?;
                if method_name(idx("ValueTableRow"), m) == Some("Set") {
                    self.writable()?;
                    let live = self.state.live(ex)?;
                    let value = arg_value_cached(ex, args.get(1), live)?;
                    self.state.check_revision(revision, schema)?;
                    self.write(ex, column, value)?;
                    Ok(Value::Undefined)
                } else {
                    self.state.check_revision(revision, schema)?;
                    self.read(ex, column.id)
                }
            }
            _ => Err(not_implemented(idx("ValueTableRow"), m).into()),
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
        let column = self.state.columns.cell_column(ex, key)?;
        self.read(ex, column.id)
    }
    fn index_set(&self, ex: &mut Execution, key: &Value, value: Value) -> CallResult<()> {
        let column = self.state.columns.cell_column(ex, key)?;
        self.write(ex, column, value)
    }
    fn iter_len(&self, _: &mut Execution) -> Option<usize> {
        Some(self.state.columns.len())
    }
    fn iter_get(&self, ex: &mut Execution, i: usize) -> CallResult<Value> {
        self.index_get(ex, &Value::from(i as i64))
    }
    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<Str>> {
        Ok(Some(type_name(ex, idx("ValueTableRow"))))
    }
    fn boost_hash(&self) -> Option<i32> {
        Some(self.record.hash)
    }
    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        Some(
            other.as_any().downcast_ref::<Self>().is_some_and(|o| {
                Rc::ptr_eq(&self.state, &o.state) && self.record.id == o.record.id
            }),
        )
    }
}
