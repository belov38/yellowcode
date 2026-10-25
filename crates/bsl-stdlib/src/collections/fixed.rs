use super::array::Array;
use super::map::{Map, copy_of_arg};
use super::reserve_values;
use super::structure::{Structure, fields_of_args};
use crate::bind::{arg_value, type_mismatch};
use bsl_vm::{Arg, CallResult, Exception, Execution, Value};
use std::collections::HashMap;
use std::rc::Rc;

pub(super) fn array_ctor(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    let src = arg_value(ex, args.first())?;
    if Array::of(&src).is_none() {
        return Err(type_mismatch(1).into());
    }
    Ok(freeze(ex, &src)?)
}

fn freeze(ex: &mut Execution, src: &Value) -> Result<Value, Exception> {
    let key = |v: &Value| v.as_object().map(|o| Rc::as_ptr(o) as *const () as usize);
    let mut copies: HashMap<usize, Rc<Array>> = HashMap::new();
    let mut sources = vec![src.clone()];
    let root = Array::new(Vec::new(), true);
    copies.extend(key(src).map(|k| (k, root.clone())));
    let mut work = vec![(root.clone(), src.clone())];
    while let Some((copy, from)) = work.pop() {
        let Some(items) = Array::of(&from).map(Array::items) else {
            continue;
        };
        reserve_values(ex, items.len() as u64)?;
        let mut out = Vec::with_capacity(items.len());
        for v in items {
            match (Array::of(&v), key(&v)) {
                (Some(a), Some(k)) if !a.is_fixed() => {
                    let c = match copies.get(&k) {
                        Some(c) => c.clone(),
                        None => {
                            let c = Array::new(Vec::new(), true);
                            copies.insert(k, c.clone());
                            sources.push(v.clone());
                            work.push((c.clone(), v.clone()));
                            c
                        }
                    };
                    out.push(Value::Object(c));
                }
                _ => out.push(v),
            }
        }
        copy.set_items(out);
    }
    drop(sources);
    Ok(Value::Object(root))
}

pub(super) fn structure_ctor(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    let fields = fields_of_args(ex, args, |_| true)?;
    Ok(Structure::new_value(fields, true))
}

pub(super) fn map_ctor(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    let src = arg_value(ex, args.first())?;
    if Map::of(&src).is_none() {
        return Err(type_mismatch(1).into());
    }
    let map = copy_of_arg(ex, args, |_| true)?;
    Ok(Map::new_value(map, true))
}
