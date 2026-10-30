use super::*;
use crate::ObjRef;

struct Frame {
    object: ObjRef,
    next: usize,
    name: Option<Str>,
}

fn leaf_work(value: &Value) -> u64 {
    1 + match value {
        Value::Number(number) => number.size_hint_limbs() / 64,
        Value::String(text) => text.len() as u64 / 64,
        _ => 0,
    }
}

fn object(ex: &mut Execution, root: ObjRef, method: &str, param: usize) -> Result<(), Exception> {
    let mut stack: Vec<Frame> = Vec::new();
    let mut next = Some((root, None));
    loop {
        if let Some((object, name)) = next.take() {
            ex.charge(stack.len() as u64)?;
            if stack.iter().any(|frame| Rc::ptr_eq(&frame.object, &object)) {
                return Err(invalid(
                    ex,
                    method,
                    param,
                    stack
                        .iter()
                        .filter_map(|frame| frame.name.as_ref())
                        .chain(name.as_ref()),
                ));
            }
            reserve(
                ex,
                (stack.capacity().saturating_mul(2).saturating_add(1) * size_of::<Frame>()) as u64,
            )?;
            stack.try_reserve_exact(1).map_err(|_| ex.out_of_memory())?;
            stack.push(Frame {
                object,
                next: 0,
                name,
            });
        }
        let Some(frame) = stack.last() else {
            return Ok(());
        };
        let start = frame.next;
        let mut ordinal = 0;
        let mut resume = start;
        let supported = frame.object.reuse_fields(&mut |name, value| {
            ex.charge(leaf_work(value))?;
            let index = ordinal;
            ordinal += 1;
            if index < start || next.is_some() {
                return Ok(());
            }
            match value {
                Value::Type(_) => {
                    return Err(invalid(
                        ex,
                        method,
                        param,
                        stack
                            .iter()
                            .filter_map(|frame| frame.name.as_ref())
                            .chain(Some(name)),
                    ));
                }
                Value::Object(child) => {
                    next = Some((child.clone(), Some(name.clone())));
                    resume = index + 1;
                }
                _ => (),
            }
            Ok(())
        })?;
        if !supported {
            return Err(invalid(
                ex,
                method,
                param,
                stack.iter().filter_map(|frame| frame.name.as_ref()),
            ));
        }
        if next.is_some() {
            if let Some(frame) = stack.last_mut() {
                frame.next = resume;
            }
        } else {
            stack.pop();
        }
    }
}

pub(super) fn arguments(
    ex: &mut Execution,
    module: &CompiledModule,
    method: u32,
    argc: usize,
) -> Result<(), Exception> {
    let code = &module.methods[method as usize];
    let base = ex.exec.stack.len().saturating_sub(argc);
    for index in 0..code.params.len() {
        if index >= argc || crate::exec::is_omitted(&ex.exec.stack[base + index]) {
            ex.charge(1)?;
            continue;
        }
        let value = crate::exec::arg_initial(&ex.exec.stack[base + index]);
        let work = leaf_work(value);
        let child = match value {
            Value::Object(object) => Some(object.clone()),
            Value::Type(_) => {
                return Err(invalid(ex, &code.name.name, index + 1, std::iter::empty()));
            }
            _ => None,
        };
        ex.charge(work)?;
        if let Some(child) = child {
            object(ex, child, &code.name.name, index + 1)?;
        }
    }
    Ok(())
}
