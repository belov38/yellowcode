use crate::Application;
use crate::model::{FieldDef, ObjDef, ObjId, StdAttr, Usage};
use crate::predefined::AreaKey;
use crate::session::call_ctx;
use crate::values::field::{default_of, from_field};
use crate::values::refs::{RefValue, ref_value};
use bsl_vm::{CallResult, Cause, Execution, HostError, Value};
use ibschema::fields::FieldValue;
use ibschema::storage::{FieldId, IndexId, TableId};
use ibschema::store::{Direction, StoreError};
use ibschema::values::RefId;
use std::ops::Bound;
use std::rc::Rc;

pub(crate) struct ReadRow {
    pub fields: Vec<Value>,
    pub found: bool,
    pub version: String,
    raw: Vec<FieldValue>,
    faulty: Vec<bool>,
}

impl ReadRow {
    pub fn value(&self, app: &Application, obj: ObjId, i: usize) -> Result<Value, HostError> {
        if self.faulty.get(i).copied().unwrap_or(false)
            && let (Some(f), Some(raw)) = (app.model().obj(obj).fields.get(i), self.raw.get(i))
        {
            return convert(app, f, raw);
        }
        self.fields.get(i).cloned().ok_or_else(|| {
            HostError::text(
                "internal: the field is not in the row",
                "internal: the field is not in the row",
            )
        })
    }
}

pub(crate) fn store_error(e: &StoreError) -> HostError {
    let text = e.to_string();
    HostError::text(
        "Ошибка при выполнении операции с информационной базой",
        "Error performing an infobase operation",
    )
    .caused_by(Cause::Host(Box::new(HostError::text(&text, &text))))
}

fn internal(text: &str) -> HostError {
    HostError::text(&format!("internal: {text}"), &format!("internal: {text}"))
}

pub(crate) fn read_fresh(app: &Application, r: RefValue) -> Result<ReadRow, HostError> {
    load_row(app, r)
}

pub(crate) fn read_object_row(
    ex: &mut Execution,
    app: &Application,
    r: RefValue,
) -> CallResult<Rc<ReadRow>> {
    let epoch = app.write_epoch();
    if let Some((e, row)) = call_ctx(ex).and_then(|c| c.read_cache.get(&r))
        && *e == epoch
    {
        return Ok(row.clone());
    }
    let row = Rc::new(load_row(app, r)?);
    if let Some(c) = call_ctx(ex) {
        if c.read_cache.len() >= crate::session::READ_CACHE_CAP {
            c.read_cache.clear();
        }
        c.read_cache.insert(r, (epoch, row.clone()));
    }
    Ok(row)
}

pub(crate) fn read_tabular(
    _ex: &mut Execution,
    app: &Application,
    r: RefValue,
    tab: usize,
) -> CallResult<Vec<Vec<Value>>> {
    let def = app.model().obj(r.obj);
    let (Some(st), Some(td)) = (app.model().storage_of(r.obj), def.tabular.get(tab)) else {
        return Ok(Vec::new());
    };
    let (Some((table, ids)), Some(number)) = (st.tabular.get(tab), st.ref_number) else {
        return Ok(Vec::new());
    };
    if r.is_empty() {
        return Ok(Vec::new());
    }
    app.predefined().ensure(app, r.obj, &AreaKey::default())?;
    let rows = fetch(app, *table, number, r, usize::MAX)?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let mut line = Vec::with_capacity(td.fields.len());
        for (i, f) in td.fields.iter().enumerate() {
            let fid = ids.get(i).copied().flatten();
            line.push(match fid {
                Some(fid) => convert(app, f, cell(row, fid)?)?,
                None => default_of(app, f)?,
            });
        }
        out.push(line);
    }
    Ok(out)
}

fn cell(row: &[FieldValue], fid: FieldId) -> Result<&FieldValue, HostError> {
    row.get(usize::from(fid.0))
        .ok_or_else(|| internal("the row is shorter than its table"))
}

fn fetch(
    app: &Application,
    table: TableId,
    number: u32,
    r: RefValue,
    limit: usize,
) -> Result<Vec<Vec<FieldValue>>, HostError> {
    let ti = app
        .store()
        .storage()
        .try_table(table)
        .ok_or_else(|| internal("no table"))?;
    if ti.key_index.as_ref().is_some_and(|k| k.prefix != 0) {
        return Err(HostError::not_implemented("Ссылка", "Разделение данных"));
    }
    let key = [FieldValue::Ref {
        table: number,
        id: r.id,
    }];
    let txn = app.store().read().map_err(|e| store_error(&e))?;
    let it = txn
        .range(
            table,
            IndexId::Id,
            Bound::Included(&key),
            Bound::Included(&key),
            Direction::Forward,
        )
        .map_err(|e| store_error(&e))?;
    let mut rows = Vec::new();
    for item in it.take(limit) {
        rows.push(item.map_err(|e| store_error(&e))?.1);
    }
    Ok(rows)
}

pub(crate) fn field_index(def: &ObjDef, name: &str) -> Option<usize> {
    let n = bsl_prim::fold_name(name);
    def.fields.iter().position(|f| {
        bsl_prim::fold_name(&f.name) == n
            || f.std.is_some_and(|a| {
                let (ru, en) = a.names();
                bsl_prim::fold_name(ru) == n || bsl_prim::fold_name(en) == n
            })
    })
}

pub(crate) fn tabular_index(def: &ObjDef, name: &str) -> Option<usize> {
    let n = bsl_prim::fold_name(name);
    def.tabular.iter().position(|t| {
        bsl_prim::fold_name(&t.name) == n
            || t.std_list.is_some_and(|list| {
                use ibschema::storage::StdList::*;
                let (ru, en) = match list {
                    LeadingCalculationTypes => ("ВедущиеВидыРасчета", "LeadingCalculationTypes"),
                    DisplacingCalculationTypes => {
                        ("ВытесняющиеВидыРасчета", "DisplacingCalculationTypes")
                    }
                    BaseCalculationTypes => ("БазовыеВидыРасчета", "BaseCalculationTypes"),
                    ExtDimensionTypes => ("ВидыСубконто", "ExtDimensionTypes"),
                };
                n == bsl_prim::fold_name(ru) || n == bsl_prim::fold_name(en)
            })
    })
}

pub(crate) fn is_std_name(name: &str) -> bool {
    let n = bsl_prim::fold_name(name);
    StdAttr::ALL.iter().any(|a| {
        let (ru, en) = a.names();
        bsl_prim::fold_name(ru) == n || bsl_prim::fold_name(en) == n
    })
}

fn convert(app: &Application, f: &FieldDef, v: &FieldValue) -> Result<Value, HostError> {
    let out = from_field(app, f, v)?;
    Ok(match (f.std, out) {
        (Some(StdAttr::IsFolder), Value::Bool(b)) => Value::Bool(!b),
        (_, out) => out,
    })
}

fn load_row(app: &Application, r: RefValue) -> Result<ReadRow, HostError> {
    let model = app.model();
    let def = model.obj(r.obj);
    if !r.is_empty() {
        app.predefined().ensure(app, r.obj, &AreaKey::default())?;
    }
    let found = match (model.storage_of(r.obj), r.is_empty()) {
        (Some(st), false) => match st.ref_number {
            Some(number) => fetch(app, st.main, number, r, 1)?
                .into_iter()
                .next()
                .map(|row| (st, row)),
            None => None,
        },
        _ => None,
    };
    let Some((st, row)) = found else {
        return defaults(app, def, r);
    };
    let mut fields = Vec::with_capacity(def.fields.len());
    let mut faulty = Vec::with_capacity(def.fields.len());
    let mut raw = Vec::with_capacity(def.fields.len());
    for (i, f) in def.fields.iter().enumerate() {
        let fid = st.fields.get(i).copied().flatten();
        let stored = match fid {
            Some(fid) => cell(&row, fid)?.clone(),
            None => FieldValue::Null,
        };
        let built = match (f.std, fid) {
            (Some(StdAttr::Predefined), _) => Ok(Value::Bool(is_predefined(def, st, &row))),
            (Some(StdAttr::PredefinedDataName), _) => Ok(Value::from(
                predefined_name(app, r.obj, &stored)
                    .unwrap_or_default()
                    .as_str(),
            )),
            (Some(StdAttr::Ref), _) => ref_value(app, r),
            (_, Some(_)) => convert(app, f, &stored),
            (_, None) => default_of(app, f),
        };
        match built {
            Ok(v) => {
                fields.push(v);
                faulty.push(false);
            }
            Err(_) => {
                fields.push(Value::Undefined);
                faulty.push(true);
            }
        }
        raw.push(stored);
    }
    if !faulty.iter().any(|b| *b) {
        raw = Vec::new();
    }
    apply_usage(def, &mut fields);
    let version = row
        .iter()
        .find_map(|v| match v {
            FieldValue::RowVersion(w) => Some(query_ir::convert::data_version(w)),
            _ => None,
        })
        .unwrap_or_default();
    Ok(ReadRow {
        fields,
        found: true,
        version,
        raw,
        faulty,
    })
}

fn predefined_name(app: &Application, obj: ObjId, stored: &FieldValue) -> Option<String> {
    let pid = match stored {
        FieldValue::Uuid(u) => RefId::from_uuid(*u).0,
        FieldValue::Ref { id, .. } => id.0,
        _ => return None,
    };
    app.predefined().name_of(app, obj, pid)
}

fn is_predefined(def: &ObjDef, st: &crate::model::ObjStorage, row: &[FieldValue]) -> bool {
    let Some(i) = def
        .fields
        .iter()
        .position(|f| f.std == Some(StdAttr::PredefinedDataName))
    else {
        return false;
    };
    match st
        .fields
        .get(i)
        .copied()
        .flatten()
        .and_then(|fid| row.get(usize::from(fid.0)))
    {
        Some(FieldValue::Uuid(u)) => u.as_bytes().iter().any(|b| *b != 0),
        Some(FieldValue::Ref { id, .. }) => !id.is_nil(),
        _ => false,
    }
}

fn apply_usage(def: &ObjDef, fields: &mut [Value]) {
    if !def.behavior.hierarchy_folders {
        return;
    }
    let Some(folder) = def
        .fields
        .iter()
        .position(|f| f.std == Some(StdAttr::IsFolder))
        .and_then(|i| fields.get(i))
        .map(|v| matches!(v, Value::Bool(true)))
    else {
        return;
    };
    for (f, v) in def.fields.iter().zip(fields.iter_mut()) {
        if f.std.is_some() {
            continue;
        }
        let hidden = match f.usage {
            Usage::ForItem => folder,
            Usage::ForFolder => !folder,
            Usage::ForFolderAndItem => false,
        };
        if hidden {
            *v = Value::Null;
        }
    }
}

fn defaults(app: &Application, def: &ObjDef, r: RefValue) -> Result<ReadRow, HostError> {
    let mut fields = Vec::with_capacity(def.fields.len());
    for f in &def.fields {
        fields.push(match f.std {
            Some(StdAttr::Ref) => ref_value(app, r)?,
            _ => default_of(app, f)?,
        });
    }
    Ok(ReadRow {
        faulty: vec![false; fields.len()],
        raw: Vec::new(),
        fields,
        found: false,
        version: String::new(),
    })
}
