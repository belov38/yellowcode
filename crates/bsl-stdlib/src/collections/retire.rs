use super::*;
use bsl_vm::{HostObject, ObjRef};
use std::collections::HashSet;

pub(super) type Visitor<'a> = dyn FnMut(&Value) -> Result<(), Exception> + 'a;
fn internal_work(object: &dyn HostObject) -> u64 {
    let any = object.as_any();
    if let Some(work) = value_tree::retirement_work(object) {
        work
    } else if let Some(v) = any.downcast_ref::<map::Map>() {
        v.retirement_work()
    } else if let Some(v) = any.downcast_ref::<structure::Structure>() {
        v.retirement_work()
    } else if let Some(v) = any.downcast_ref::<value_table::ValueTable>() {
        v.retirement_work()
    } else if let Some(v) = any.downcast_ref::<value_table::row::TableRow>() {
        v.retirement_work()
    } else {
        0
    }
}
fn children(object: &dyn HostObject, visit: &mut Visitor<'_>) -> Result<(), Exception> {
    let any = object.as_any();
    if let Some(result) = value_tree::visit_values(object, visit) {
        result
    } else if let Some(v) = any.downcast_ref::<array::Array>() {
        v.visit_values(visit)
    } else if let Some(v) = any.downcast_ref::<structure::Structure>() {
        v.visit_values(visit)
    } else if let Some(v) = any.downcast_ref::<map::Map>() {
        v.visit_values(visit)
    } else if let Some(v) = any.downcast_ref::<value_list::ValueList>() {
        v.visit_values(visit)
    } else if let Some(v) = any.downcast_ref::<value_list::ValueListItem>() {
        v.visit_values(visit)
    } else if let Some(v) = any.downcast_ref::<value_table::ValueTable>() {
        v.visit_values(visit)
    } else if let Some(v) = any.downcast_ref::<value_table::row::TableRow>() {
        v.visit_values(visit)
    } else if let Some(v) = any.downcast_ref::<KeyValue>() {
        v.visit_values(visit)
    } else {
        Ok(())
    }
}
struct Census<'a> {
    ex: &'a mut Execution,
    live: u64,
    edges: u64,
    seen: HashSet<usize>,
    todo: Vec<ObjRef>,
}
impl Census<'_> {
    fn visit(&mut self, value: &Value) -> Result<(), Exception> {
        self.edges = self.edges.saturating_add(1);
        self.ex.charge(1)?;
        reserve_values(self.ex, self.edges)?;
        let nodes = self.seen.len() as u64 + 1;
        self.ex.reserve(
            self.live
                .saturating_add(self.edges.saturating_mul(VALUE_BYTES))
                .saturating_add(nodes.saturating_mul(256)),
        )?;
        if let Value::Object(object) = value {
            let key = Rc::as_ptr(object) as *const () as usize;
            if !self.seen.contains(&key) {
                self.seen
                    .try_reserve(1)
                    .map_err(|_| self.ex.out_of_memory())?;
                self.todo
                    .try_reserve(1)
                    .map_err(|_| self.ex.out_of_memory())?;
                self.seen.insert(key);
                self.todo.push(object.clone());
            }
        }
        Ok(())
    }
}
pub(super) fn prepare(
    ex: &mut Execution,
    live: u64,
    roots: impl FnOnce(&mut Visitor<'_>) -> Result<(), Exception>,
) -> Result<Vec<Value>, Exception> {
    let mut census = Census {
        ex,
        live,
        edges: 0,
        seen: HashSet::new(),
        todo: Vec::new(),
    };
    roots(&mut |value| census.visit(value))?;
    while let Some(object) = census.todo.pop() {
        census.ex.charge(internal_work(&*object))?;
        children(&*object, &mut |value| census.visit(value))?;
    }
    census.ex.reserve(
        live.saturating_add(census.edges.saturating_mul(VALUE_BYTES))
            .saturating_add((census.seen.len() as u64).saturating_mul(256)),
    )?;
    vec_with_capacity(census.ex, census.edges)
}
pub(super) fn push(out: &mut Vec<Value>, value: Value) {
    assert!(out.len() < out.capacity(), "retirement census capacity");
    out.push(value);
}
pub(super) fn run(mut stack: Vec<Value>) {
    while let Some(value) = stack.pop() {
        let Value::Object(object) = value else {
            continue;
        };
        if Rc::strong_count(&object) != 1 {
            continue;
        }
        let any = object.as_any();
        if value_tree::drain(&*object, &mut stack, true) {
        } else if let Some(v) = any.downcast_ref::<array::Array>() {
            v.retire_into(&mut stack);
        } else if let Some(v) = any.downcast_ref::<structure::Structure>() {
            v.retire_into(&mut stack);
        } else if let Some(v) = any.downcast_ref::<map::Map>() {
            v.retire_into(&mut stack);
        } else if let Some(v) = any.downcast_ref::<value_list::ValueList>() {
            v.retire_into(&mut stack);
        } else if let Some(v) = any.downcast_ref::<value_list::ValueListItem>() {
            v.retire_into(&mut stack);
        } else if let Some(v) = any.downcast_ref::<value_table::ValueTable>() {
            v.retire_into(&mut stack);
        } else if let Some(v) = any.downcast_ref::<value_table::row::TableRow>() {
            v.retire_into(&mut stack);
        } else if let Some(v) = any.downcast_ref::<KeyValue>() {
            v.retire_into(&mut stack);
        }
    }
}
