use crate::Application;
use crate::errors::{self, keys};
use crate::model::{ObjDef, ObjKind, ObjStorage, StdAttr};
use crate::objects::{ObjectInstance, WriteStamp};
use crate::predefined::{AreaKey, blank_row};
use crate::read::read_fresh;
use crate::read_budget as budget;
use crate::txn::{CallTxn, Commit, TxnError};
use crate::values::field::to_field;
use crate::values::refs::RefValue;
use crate::write_hooks::{self, WriteCtx};
use bsl_vm::{CallResult, Category, CategorySet, Execution, HostError};
use ibschema::fields::{FieldValue, encode};
use ibschema::names::StdField;
use ibschema::storage::{IndexId, TableId, TableInfo};
use ibschema::store::{Direction, RowHandle, StoreError, StoreResult, WriteTxn};
use ibschema::values::RefId;
use platform_catalog::messages::Lang;
use std::ops::Bound;
use std::rc::Rc;

fn internal(text: &str) -> HostError {
    HostError::text(&format!("internal: {text}"), &format!("internal: {text}"))
}

fn l5(key: &str) -> HostError {
    match errors::msg(key) {
        Some(m) => HostError::text(m.ru, m.en),
        None => internal(key),
    }
}

pub(crate) fn use_error(key: &str) -> HostError {
    let mut e = l5(key);
    e.categories = Some(CategorySet::of(&[
        Category::AllErrors,
        Category::ScriptRuntimeError,
        Category::ScriptUseError,
    ]));
    e
}

fn other_error(mut e: HostError) -> HostError {
    e.categories = Some(CategorySet::of(&[
        Category::AllErrors,
        Category::ScriptRuntimeError,
        Category::OtherError,
    ]));
    e
}

fn ref_not_unique(table: &str) -> HostError {
    let text = |lang| {
        format!(
            "{}\n{}",
            errors::render(keys::REF_NOT_UNIQUE, &[], lang),
            errors::render(keys::REF_NOT_UNIQUE_TABLE, &[table], lang)
        )
    };
    other_error(HostError::text(&text(Lang::Ru), &text(Lang::En)))
}

struct Plan<'a> {
    app: &'a Application,
    def: &'a ObjDef,
    st: &'a ObjStorage,
    main: &'a TableInfo,
    number: u32,
}

impl<'a> Plan<'a> {
    fn new(app: &'a Application, inst: &ObjectInstance) -> Result<Plan<'a>, HostError> {
        let def = app.model().obj(inst.obj);
        let refuse = |what: &str| {
            HostError::not_implemented(&inst.type_name(), &format!("Записать: {what}"))
        };
        if def.kind != ObjKind::Catalog {
            return Err(refuse("вид объекта"));
        }
        if app.options().read_only_scope.is_some() {
            return Err(refuse("приложение только для чтения (read_scope)"));
        }
        let st = app
            .model()
            .storage_of(inst.obj)
            .ok_or_else(|| internal("the object has no storage"))?;
        let number = st
            .ref_number
            .ok_or_else(|| internal("the object has no reference table"))?;
        let map = app.store().storage();
        let main = map
            .try_table(st.main)
            .ok_or_else(|| internal("no main table"))?;
        let separated = !main.separators.is_empty()
            || st.tabular.iter().any(|(t, _)| {
                map.try_table(*t)
                    .is_some_and(|ti| !ti.separators.is_empty())
            });
        if separated && app.options().area().is_none() {
            return Err(refuse("данные разделены, область данных не задана"));
        }
        Ok(Plan {
            app,
            def,
            st,
            main,
            number,
        })
    }

    fn key(&self, ti: &TableInfo, id: RefId) -> Result<Vec<FieldValue>, HostError> {
        let mut key = if ti.key_index.as_ref().is_some_and(|k| k.prefix != 0) {
            let scope = self
                .app
                .options()
                .area()
                .ok_or_else(|| internal("separated table without an area"))?;
            scope
                .area(ti)
                .map_err(|_| internal("invalid bound write area"))?
        } else {
            Vec::new()
        };
        key.push(FieldValue::Ref {
            table: self.number,
            id,
        });
        Ok(key)
    }
}

struct TabJob {
    table: TableId,
    key: Vec<FieldValue>,
    rows: Vec<Vec<FieldValue>>,
}

struct Edits {
    main: Vec<(usize, FieldValue)>,
    tabs: Vec<TabJob>,
}

fn set_std(row: &mut [FieldValue], ti: &TableInfo, std: StdField, v: FieldValue) {
    if let Some(slot) = ti
        .field_by_std(std)
        .and_then(|fid| row.get_mut(usize::from(fid.0)))
    {
        *slot = v;
    }
}

fn edits(
    ex: &mut Execution,
    plan: &Plan<'_>,
    inst: &ObjectInstance,
    id: RefId,
    is_new: bool,
) -> CallResult<Edits> {
    let map = plan.app.store().storage();
    let mut main = Vec::new();
    let group = inst.folder_flag().unwrap_or(false);
    {
        let st = inst.state.borrow();
        for (i, f) in plan.def.fields.iter().enumerate() {
            let Some(fid) = plan.st.fields.get(i).copied().flatten() else {
                continue;
            };
            let col = usize::from(fid.0);
            match f.std {
                Some(StdAttr::PredefinedDataName) => {
                    if crate::write_checks::clears_predefined_name(inst, i) {
                        main.push((col, crate::write_checks::cleared_id()));
                    }
                    continue;
                }
                Some(StdAttr::Ref | StdAttr::Predefined) => continue,
                Some(StdAttr::IsFolder) => {
                    if is_new {
                        main.push((col, FieldValue::Bool(!group)));
                    }
                    continue;
                }
                _ => {}
            }
            if is_new && inst.hidden(i) && encode(plan.main, fid, &FieldValue::Null).is_ok() {
                main.push((col, FieldValue::Null));
                continue;
            }
            if let Some(Some(v)) = st.attributes.get(i) {
                main.push((col, to_field(plan.app, f, v)?));
            }
        }
    }
    let tabs = inst.state.borrow().tabular.clone();
    let mut jobs = Vec::new();
    for (t, tab) in tabs.iter().enumerate() {
        let lines = tab.lines();
        if if is_new {
            lines.is_empty()
        } else {
            !tab.touched()
        } {
            continue;
        }
        let (Some((table, ids)), Some(td)) = (plan.st.tabular.get(t), plan.def.tabular.get(t))
        else {
            return Err(internal("no table of the section").into());
        };
        let ti = map
            .try_table(*table)
            .ok_or_else(|| internal("no table of the section"))?;
        let mut hints = vec![None; ti.fields.len()];
        for (f, fid) in td.fields.iter().zip(ids) {
            if let Some(slot) = fid.and_then(|fid| hints.get_mut(usize::from(fid.0))) {
                *slot = Some(f);
            }
        }
        let mut rows = Vec::with_capacity(lines.len());
        for (j, line) in lines.iter().enumerate() {
            ex.charge(1)?;
            let number = FieldValue::Number(ibschema::Numeric::from_i64(
                i64::try_from(j + 1).unwrap_or(i64::MAX),
            ));
            let mut row = blank_row(plan.app, ti, &hints);
            set_std(
                &mut row,
                ti,
                StdField::Id,
                FieldValue::Ref {
                    table: plan.number,
                    id,
                },
            );
            let key = FieldValue::Number(ibschema::Numeric::from_i64(
                i64::try_from(j).unwrap_or(i64::MAX),
            ));
            set_std(&mut row, ti, StdField::KeyField, key);
            for ((c, f), fid) in td.fields.iter().enumerate().zip(ids) {
                let Some(slot) = fid.and_then(|fid| row.get_mut(usize::from(fid.0))) else {
                    continue;
                };
                *slot = if f.std == Some(StdAttr::LineNumber) {
                    number.clone()
                } else {
                    match line.get(c) {
                        Some(v) => to_field(plan.app, f, v)?,
                        None => continue,
                    }
                };
            }
            rows.push(row);
        }
        jobs.push(TabJob {
            table: *table,
            key: plan.key(ti, id)?,
            rows,
        });
    }
    Ok(Edits { main, tabs: jobs })
}

fn apply(row: &mut [FieldValue], edits: &[(usize, FieldValue)]) {
    for (col, v) in edits {
        if let Some(slot) = row.get_mut(*col) {
            *slot = v.clone();
        }
    }
}

fn row_version(row: &[FieldValue]) -> Option<String> {
    row.iter().find_map(|v| match v {
        FieldValue::RowVersion(w) => Some(query_ir::convert::data_version(w)),
        _ => None,
    })
}

enum Stored {
    Done,
    Conflict,
}

fn outside_call() -> HostError {
    internal("the write of an object outside a call")
}

pub(crate) fn write(ex: &mut Execution, inst: &Rc<ObjectInstance>) -> CallResult<()> {
    let app = inst.app.clone();
    inst.live()?;
    let plan = Plan::new(&app, inst)?;
    if !crate::session::call_ctx(ex).is_some_and(|c| c.sess.user().no_users) {
        return Err(HostError::not_implemented("P6", "application write security provider").into());
    }
    let txn = CallTxn::of_call(ex).ok_or_else(outside_call)?;
    txn.check().map_err(|e| e.host())?;
    let loading = inst.loading();
    app.predefined()
        .ensure(&app, inst.obj, &AreaKey::default())?;
    let is_new = inst.state.borrow().is_new;
    let snapshot = inst.snapshot();
    txn.begin();
    let ended = match steps(ex, &plan, &txn, inst, is_new, loading) {
        Ok(()) => txn.commit(&*app).map_err(|e| e.call_error(ex)),
        Err(e) => {
            let _ = txn.rollback(&*app);
            Err(e)
        }
    };
    if !matches!(ended, Ok(Commit::Committed | Commit::Inner)) {
        inst.restore(snapshot);
    }
    ended.map(|_| ())
}

fn steps(
    ex: &mut Execution,
    plan: &Plan<'_>,
    txn: &Rc<CallTxn>,
    inst: &Rc<ObjectInstance>,
    is_new: bool,
    loading: bool,
) -> CallResult<()> {
    let app = plan.app;
    let ctx = WriteCtx {
        inst,
        is_new,
        loading,
    };
    write_hooks::before_write(ex, &ctx)?;
    write_hooks::assign_code(ex, &ctx)?;
    write_hooks::check_write(ex, &ctx)?;
    let (id, expected) = {
        let st = inst.state.borrow();
        let id = match st.r {
            Some(r) if !is_new => r.id,
            _ => st.new_ref.map_or_else(|| app.uuid1().next_ref(), |r| r.id),
        };
        (id, st.data_version.clone())
    };
    let edits = edits(ex, plan, inst, id, is_new)?;
    let hints: Vec<Option<&crate::model::FieldDef>> = {
        let mut hints = vec![None; plan.main.fields.len()];
        for (f, fid) in plan.def.fields.iter().zip(&plan.st.fields) {
            if let Some(slot) = fid.and_then(|fid| hints.get_mut(usize::from(fid.0))) {
                *slot = Some(f);
            }
        }
        hints
    };
    let mut fresh = blank_row(app, plan.main, &hints);
    set_std(
        &mut fresh,
        plan.main,
        StdField::Id,
        FieldValue::Ref {
            table: plan.number,
            id,
        },
    );
    apply(&mut fresh, &edits.main);
    let key = plan.key(plan.main, id)?;
    let main_table = plan.st.main;
    let bytes = edits
        .tabs
        .iter()
        .flat_map(|j| j.rows.iter())
        .fold(budget::stored_row(&fresh), |n, r| {
            n.saturating_add(budget::stored_row(r))
        });
    budget::reserve(ex, bytes.saturating_mul(2))?;
    if crate::data_lock::lock_for_write(app, txn, plan.def).is_err() {
        return Err(crate::data_lock::conflict().into());
    }
    let stored = txn.with_write(app, |w: &mut dyn WriteTxn| -> StoreResult<Stored> {
        if is_new {
            w.insert(main_table, &fresh)?;
        } else {
            let found = {
                let mut rows = w.range(
                    main_table,
                    IndexId::Id,
                    Bound::Included(key.as_slice()),
                    Bound::Included(key.as_slice()),
                    Direction::Forward,
                )?;
                rows.next().transpose()?
            };
            let Some((handle, mut row)) = found else {
                return Ok(Stored::Conflict);
            };
            if row_version(&row).as_deref() != Some(expected.as_str()) {
                return Ok(Stored::Conflict);
            }
            apply(&mut row, &edits.main);
            w.update(main_table, handle, &row)?;
        }
        for job in &edits.tabs {
            if !is_new {
                let handles = w
                    .range(
                        job.table,
                        IndexId::Id,
                        Bound::Included(job.key.as_slice()),
                        Bound::Included(job.key.as_slice()),
                        Direction::Forward,
                    )?
                    .map(|item| item.map(|(h, _)| h))
                    .collect::<StoreResult<Vec<RowHandle>>>()?;
                for h in handles {
                    w.delete(job.table, h)?;
                }
            }
            for row in &job.rows {
                w.insert(job.table, row)?;
            }
        }
        Ok(Stored::Done)
    });
    match stored {
        Ok(Stored::Done) => {}
        Ok(Stored::Conflict) => {
            return Err(other_error(l5(keys::OBJECT_VERSION_MISMATCH)).into());
        }
        Err(TxnError::Store(StoreError::UniqueViolation { table_name, .. })) if is_new => {
            return Err(ref_not_unique(&table_name).into());
        }
        Err(e) => return Err(e.call_error(ex)),
    }
    let r = RefValue { obj: inst.obj, id };
    let row = read_fresh(ex, app, r)?;
    if !row.found {
        return Err(internal("the written row is not found").into());
    }
    {
        let mut st = inst.state.borrow_mut();
        st.r = Some(r);
        st.is_new = false;
        st.data_version = row.version.clone();
        st.row = Rc::new(row);
        for slot in st.attributes.iter_mut() {
            *slot = None;
        }
        for tab in &st.tabular {
            tab.clear_touched();
        }
        st.modified.set(false);
        st.new_ref = None;
        let id = txn.id().unwrap_or(0);
        let was_new = is_new || st.written.as_ref().is_some_and(|w| w.was_new && w.id == id);
        st.written = Some(WriteStamp {
            txn: Rc::downgrade(txn),
            id,
            was_new,
        });
    }
    write_hooks::on_write(ex, &ctx)?;
    Ok(())
}
