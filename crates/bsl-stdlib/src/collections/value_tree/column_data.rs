use super::search::Query;
use super::*;
use crate::collections::array::Array;

impl TreeRows {
    pub(super) fn unload_column(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let mut q = Query::new(self, ex, args, 1, false)?;
        q.extract(ex, 0)?;
        let column = q.selected_column(ex, &q.cached()[0])?;
        q.check()?;
        let count = self.len();
        ex.charge(count as u64)?;
        q.add_scratch(
            (count as u64)
                .saturating_mul(std::mem::size_of::<Value>() as u64)
                .saturating_add(
                    (std::mem::size_of::<Array>() + 2 * std::mem::size_of::<usize>()) as u64,
                ),
        );
        q.reserve(ex, 0)?;
        let mut values = vec_with_capacity(ex, count as u64)?;
        for index in 0..count {
            let node = self
                .with_ids(|ids| ids.get(index).and_then(|id| self.state.node(*id)))
                .ok_or_else(changed)?;
            let value = q.cell(ex, &node, column.id)?;
            q.add_scratch(value_bytes(&value));
            q.reserve(ex, 0)?;
            values.push(value);
        }
        q.check()?;
        q.reserve(ex, 0)?;
        Ok(Array::new_value(values, false))
    }
    pub(super) fn load_column(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        self.writable()?;
        let mut q = Query::for_array(self, ex, args)?;
        q.extract(ex, 0)?;
        let source = Array::of(&q.cached()[0])
            .filter(|a| !a.is_fixed())
            .ok_or_else(|| type_mismatch(1))?;
        let base = q.reserve(ex, 0)?;
        let (values, snapshot_live) = source.guarded_items(ex, base)?;
        q.add_scratch(snapshot_live.saturating_sub(base));
        q.extract(ex, 1)?;
        let column = q.selected_column(ex, &q.cached()[1])?;
        q.check()?;
        self.writable()?;
        let count = values.len().min(self.len());
        ex.charge(count as u64)?;
        q.add_scratch((count as u64).saturating_mul(
            (std::mem::size_of::<Rc<Node>>() + std::mem::size_of::<(Rc<Node>, Cells)>()) as u64,
        ));
        q.reserve(ex, 0)?;
        let mut nodes = vec_with_capacity(ex, count as u64)?;
        self.with_ids(|ids| {
            for id in ids.iter().take(count) {
                nodes.push(self.state.node(*id).ok_or_else(changed)?);
            }
            Ok::<_, bsl_vm::CallError>(())
        })?;
        let mut prepared = vec_with_capacity(ex, count as u64)?;
        for (index, node) in nodes.iter().enumerate() {
            q.check()?;
            let cell_count = node.cells.borrow().0.len();
            ex.charge((cell_count as u64).saturating_mul(2))?;
            q.add_scratch(
                (cell_count as u64).saturating_mul(std::mem::size_of::<(u32, Value)>() as u64),
            );
            q.reserve(ex, 0)?;
            let mut cells = Cells(vec_with_capacity(ex, cell_count as u64)?);
            for cell_index in 0..cell_count {
                let id = node.cells.borrow().0.get(cell_index).ok_or_else(changed)?.0;
                let value = if id == column.id {
                    let live = q.reserve(ex, value_bytes(&values[index]))?;
                    let input = clone_cell(ex, &values[index], live)?;
                    let adjusted = adjust(ex, &column.value_type(), input, live)?;
                    q.check()?;
                    q.reserve(ex, value_bytes(&adjusted))?;
                    adjusted
                } else {
                    let live = q.reserve(ex, 0)?;
                    let old = node.cells.borrow();
                    let value = &old.0.get(cell_index).ok_or_else(changed)?.1;
                    clone_cell(ex, value, live.saturating_add(value_bytes(value)))?
                };
                q.add_scratch(value_bytes(&value));
                q.reserve(ex, 0)?;
                cells.0.push((id, value));
            }
            prepared.push((node.clone(), cells));
        }
        let live = q.reserve(ex, 0)?;
        let mut retired = super::super::retire::prepare(ex, live, |visit| {
            for node in &nodes {
                for (_, value) in &node.cells.borrow().0 {
                    visit(value)?;
                }
            }
            for value in &values {
                visit(value)?;
            }
            for value in q.cached() {
                visit(value)?;
            }
            Ok(())
        })?;
        q.add_scratch(
            (retired.capacity() as u64).saturating_mul(std::mem::size_of::<Value>() as u64),
        );
        q.check()?;
        self.writable()?;
        q.reserve(ex, 0)?;
        for value in q.take_values() {
            super::super::retire::push(&mut retired, value);
        }
        for value in values {
            super::super::retire::push(&mut retired, value);
        }
        for (node, cells) in &mut prepared {
            std::mem::swap(&mut *node.cells.borrow_mut(), cells);
            self.state.update_bytes(node);
            cells.drain_into(&mut retired, true);
        }
        if count != 0 {
            self.state.bump();
        }
        super::super::retire::run(retired);
        Ok(Value::Undefined)
    }
}
