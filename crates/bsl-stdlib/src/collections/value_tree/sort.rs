use super::*;
use crate::sort::{SortKey, sort_by_keys, sort_key};
use std::mem::size_of;

struct Group {
    parent: Option<Rc<Node>>,
    start: usize,
    len: usize,
}

impl TreeRows {
    fn sort_live(
        &self,
        ex: &mut Execution,
        args: &[Arg],
        cached: &[Value; 2],
        scratch: u64,
    ) -> CallResult<u64> {
        let scratch = args.iter().fold(scratch, |n, arg| {
            n.saturating_add(match arg {
                Arg::Place(place) => place.numeric_backing_bytes(ex),
                _ => 0,
            })
        });
        let unsupported = ex.out_of_memory();
        let retained = ex.measure_retained(scratch, |visit| {
            if !crate::collections::accounting::project(self, visit)? {
                return Err(unsupported);
            }
            for arg in args {
                if let Arg::Val(value) = arg {
                    visit.value(value)?;
                }
            }
            for value in cached {
                visit.value(value)?;
            }
            Ok(())
        })?;
        let live = retained.saturating_add(scratch);
        ex.reserve(live)?;
        Ok(live)
    }

    fn sort_columns(
        &self,
        ex: &mut Execution,
        selector: &Str,
        live: u64,
    ) -> CallResult<Vec<(Rc<ColumnRecord>, bool)>> {
        ex.charge(selector.len() as u64 / 32 + 1)?;
        if selector.is_empty() {
            return Err(value_mismatch(1).into());
        }
        let count = selector
            .units()
            .iter()
            .filter(|u| **u == u16::from(b','))
            .count() as u64
            + 1;
        let live =
            live.saturating_add(count.saturating_mul(size_of::<(Rc<ColumnRecord>, bool)>() as u64));
        ex.reserve(live)?;
        let mut columns = vec_with_capacity(ex, count)?;
        for clause in selector.units().split(|u| *u == u16::from(b',')) {
            ex.reserve(
                live.saturating_add((clause.len() as u64).saturating_mul(42))
                    .saturating_add(2048),
            )?;
            let mut words = clause
                .split(|u| matches!(*u, 9 | 10 | 13 | 32))
                .filter(|part| !part.is_empty());
            let name = words.next().ok_or_else(|| value_mismatch(1))?;
            let descending = words.next().is_some_and(|word| {
                matches!(
                    bsl_prim::fold_name(&Str::from_units(word).to_string_lossy()).as_str(),
                    "убыв" | "desc"
                )
            });
            let name = Str::from_units(name);
            let column = self
                .state
                .columns
                .named_record(
                    ex,
                    &name,
                    live.saturating_add((name.len() as u64).saturating_mul(2)),
                )?
                .ok_or_else(|| HostError::key("core85:IDS_VT_COLUMNCODE_ERROR"))?;
            columns.push((column, descending));
        }
        Ok(columns)
    }

    pub(super) fn sort(&self, ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
        self.writable()?;
        let revision = self.state.revision.get();
        let schema = self.state.columns.revision();
        let check = || -> CallResult<()> {
            self.state.check_source(revision, schema)?;
            self.writable()
        };
        let mut cached = [Value::Undefined, Value::Undefined];
        let mut source = self.sort_live(ex, args, &cached, 0)?;
        for index in 0..2 {
            let value = arg_value_cached(ex, args.get(index), source);
            check()?;
            cached[index] = value?;
            source = self.sort_live(ex, args, &cached, 0)?;
        }
        let Value::String(selector) = &cached[0] else {
            return Err(type_mismatch(1).into());
        };
        let recursive = match &cached[1] {
            Value::Undefined => false,
            value @ (Value::Bool(_) | Value::Number(_) | Value::String(_)) => {
                if let Value::String(text) = value {
                    ex.charge(text.len() as u64 / 64 + 1)?;
                }
                bsl_vm::ops::to_bool(ex, value).map_err(|_| type_mismatch(2))?
            }
            _ => return Err(type_mismatch(2).into()),
        };
        check()?;
        let columns = self.sort_columns(ex, selector, source)?;
        let width = columns.len() as u64;
        let count = if recursive {
            self.state.arena.borrow().len()
        } else {
            self.len()
        };
        let group_count = if recursive {
            count.checked_add(1).ok_or_else(|| ex.out_of_memory())?
        } else {
            1
        };
        let scratch = (columns.capacity() as u64)
            .saturating_mul(size_of::<(Rc<ColumnRecord>, bool)>() as u64)
            .saturating_add(width.saturating_mul(size_of::<bool>() as u64))
            .saturating_add((count as u64).saturating_mul(size_of::<usize>() as u64))
            .saturating_add((group_count as u64).saturating_mul(size_of::<Group>() as u64));
        ex.reserve(source.saturating_add(scratch))?;
        let mut groups = vec_with_capacity(ex, group_count as u64)?;
        let mut order = vec_with_capacity(ex, count as u64)?;
        let mut desc = vec_with_capacity(ex, width)?;
        desc.extend(columns.iter().map(|(_, descending)| *descending));
        self.with_ids(|ids| order.extend(ids.iter().copied()));
        groups.push(Group {
            parent: self.parent.clone(),
            start: 0,
            len: order.len(),
        });
        let mut next = 0;
        while next < groups.len() {
            let start = groups[next].start;
            let len = groups[next].len;
            ex.charge(len as u64 + 1)?;
            if recursive {
                for index in start..start + len {
                    let node = self.state.node(order[index]).ok_or_else(changed)?;
                    let children = node.children.borrow();
                    ex.charge(children.len() as u64)?;
                    if !children.is_empty() {
                        let start = order.len();
                        order.extend(children.iter().copied());
                        groups.push(Group {
                            parent: Some(node.clone()),
                            start,
                            len: children.len(),
                        });
                    }
                }
            }
            next += 1;
        }
        for group in &groups {
            let key_scratch = (group.len as u64).saturating_mul(
                (size_of::<Vec<SortKey>>() + 2 * size_of::<usize>()) as u64
                    + width.saturating_mul(size_of::<SortKey>() as u64),
            );
            let scratch = scratch.saturating_add(key_scratch);
            let mut payload = 0u64;
            let mut live = source.saturating_add(scratch);
            ex.reserve(live)?;
            let mut keys = vec_with_capacity(ex, group.len as u64)?;
            for id in &order[group.start..group.start + group.len] {
                let node = self.state.node(*id).ok_or_else(changed)?;
                let mut row_keys = vec_with_capacity(ex, width)?;
                for (column, _) in &columns {
                    let value = {
                        let cells = node.cells.borrow();
                        ex.charge(cells.0.len() as u64)?;
                        match cells.0.iter().find(|(id, _)| *id == column.id) {
                            Some((_, value)) => clone_cell(ex, value, live)?,
                            None => Value::Undefined,
                        }
                    };
                    let before = live.saturating_add(scalar_clone_bytes(&value));
                    let mut during = before;
                    let key = sort_key(ex, &value, &mut during);
                    check()?;
                    let key = key?;
                    payload = payload.saturating_add(during.saturating_sub(before));
                    if matches!(value, Value::Object(_)) {
                        let retained =
                            self.sort_live(ex, args, &cached, scratch.saturating_add(payload))?;
                        source = retained.saturating_sub(scratch.saturating_add(payload));
                    }
                    live = source.saturating_add(scratch).saturating_add(payload);
                    ex.reserve(live)?;
                    row_keys.push(key);
                }
                keys.push(row_keys);
            }
            sort_by_keys(
                ex,
                &mut order[group.start..group.start + group.len],
                &keys,
                &desc,
            )?;
            check()?;
        }
        ex.charge((groups.len() as u64).saturating_add(order.len() as u64))?;
        check()?;
        for group in &groups {
            let sorted = &order[group.start..group.start + group.len];
            match &group.parent {
                Some(parent) => parent.children.borrow_mut().copy_from_slice(sorted),
                None => self.state.roots.borrow_mut().copy_from_slice(sorted),
            }
        }
        self.state.bump();
        Ok(Value::Undefined)
    }
}
