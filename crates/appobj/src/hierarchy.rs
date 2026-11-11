use crate::Application;
use crate::meta::{arg_value, type_mismatch};
use crate::model::{ObjId, ObjKind, StdAttr};
use crate::objects::not_selected;
use crate::read::{ReadRow, read_object_row};
use crate::values::refs::{RefValue, as_ref};
use bsl_vm::{Arg, CallResult, Execution, HostError, Value};
use std::rc::Rc;

const MAX_DEPTH: usize = 1024;

type Link = (RefValue, Rc<ReadRow>);

pub(crate) fn call(
    ex: &mut Execution,
    app: &Application,
    r: RefValue,
    member: &str,
    args: &[Arg],
) -> CallResult<Value> {
    let refused = || HostError::not_implemented("Ссылка", member);
    match member {
        "Уровень" => {
            selected(app, r)?;
            let chain = chain(ex, app, r)?.ok_or_else(refused)?;
            let level = i64::try_from(chain.len().saturating_sub(1)).unwrap_or(i64::MAX);
            Ok(Value::from(level))
        }
        "ПолныйКод" => {
            if r.is_empty() && std_field(app, r.obj, StdAttr::Code).is_none() {
                return Err(refused().into());
            }
            selected(app, r)?;
            let chain = chain(ex, app, r)?.ok_or_else(refused)?;
            let mut parts = Vec::with_capacity(chain.len());
            match std_field(app, r.obj, StdAttr::Code) {
                Some(code) => {
                    for (at, row) in chain.iter().rev() {
                        let value = row.value(ex, app, at.obj, code)?;
                        parts.push(code_text(&value).ok_or_else(refused)?);
                    }
                }
                None if chain.len() == 1 => {}
                None => return Err(refused().into()),
            }
            Ok(Value::from(parts.join("/").as_str()))
        }
        "ПринадлежитЭлементу" => {
            let group = as_ref(&arg_value(ex, args, 0)?)
                .filter(|g| g.obj == r.obj)
                .ok_or_else(|| type_mismatch(1))?;
            selected(app, r)?;
            if group.is_empty() && !hierarchical_catalog(app, r.obj) {
                return Err(refused().into());
            }
            let chain = chain(ex, app, r)?.ok_or_else(refused)?;
            Ok(Value::Bool(
                chain.iter().skip(1).any(|(at, _)| *at == group),
            ))
        }
        _ => Err(refused().into()),
    }
}

fn hierarchical_catalog(app: &Application, obj: ObjId) -> bool {
    app.model().obj(obj).kind == ObjKind::Catalog
        && std_field(app, obj, StdAttr::Parent).is_some()
        && std_field(app, obj, StdAttr::IsFolder).is_some()
}

fn selected(app: &Application, r: RefValue) -> CallResult<()> {
    if r.is_empty() && hierarchical_catalog(app, r.obj) {
        return Err(not_selected().into());
    }
    Ok(())
}

fn chain(ex: &mut Execution, app: &Application, r: RefValue) -> CallResult<Option<Vec<Link>>> {
    let Some(parent) = std_field(app, r.obj, StdAttr::Parent) else {
        return flat_item(ex, app, r);
    };
    let mut out: Vec<Link> = Vec::new();
    let mut at = r;
    loop {
        ex.charge(1)?;
        let row = read_object_row(ex, app, at)?;
        if !row.found || out.len() >= MAX_DEPTH || out.iter().any(|(seen, _)| *seen == at) {
            return Ok(None);
        }
        let up = as_ref(&row.value(ex, app, at.obj, parent)?);
        out.push((at, row));
        match up {
            Some(p) if p.is_empty() => return Ok(Some(out)),
            Some(p) => at = p,
            None => return Ok(None),
        }
    }
}

fn flat_item(ex: &mut Execution, app: &Application, r: RefValue) -> CallResult<Option<Vec<Link>>> {
    let code = std_field(app, r.obj, StdAttr::Code);
    let def = app.model().obj(r.obj);
    let plain =
        def.kind == ObjKind::Catalog && def.behavior.owners.is_empty() && def.tabular.is_empty();
    let Some(code) = code.filter(|_| plain) else {
        return Ok(None);
    };
    ex.charge(1)?;
    let row = read_object_row(ex, app, r)?;
    let string_code = row.found && matches!(row.value(ex, app, r.obj, code)?, Value::String(_));
    Ok(string_code.then(|| vec![(r, row)]))
}

fn std_field(app: &Application, obj: ObjId, attr: StdAttr) -> Option<usize> {
    app.model()
        .obj(obj)
        .fields
        .iter()
        .position(|f| f.std == Some(attr))
}

fn code_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => {
            Some(s.to_string_lossy()).filter(|t| !t.is_empty() && !t.ends_with(' '))
        }
        Value::Number(n) => n
            .to_i64()
            .filter(|n| (0..1000).contains(n))
            .map(|n| n.to_string()),
        _ => None,
    }
}
