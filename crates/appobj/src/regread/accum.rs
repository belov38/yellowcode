use super::{AccumSet, FIELD_BASE, Movements, Reg, template_member, type_of};
use crate::Application;
use crate::events::{self, Event};
use crate::model::{
    AccumRegType, FieldDef, ObjId, ObjKind, Posting, RecordsDeletion, RecordsWriting, StdAttr,
};
use crate::objects::ObjectInstance;
use crate::read_budget as budget;
use crate::session::{call_ctx, internal};
use crate::txn::{CallTxn, Commit};
use crate::types::Category;
use crate::values::field::{default_of, to_field};
use crate::values::refs::{RefValue, as_ref};
use bsl_prim::{BslDate, Str, fold_name};
use bsl_vm::{
    Arg, CallResult, Exception, Execution, HostError, HostObject, MemberId, MemberKind,
    RetainedMemory, TypeId, Value,
};
use ibschema::Row;
use ibschema::fields::FieldValue;
use ibschema::names::role;
use ibschema::storage::{FieldId, IndexId, TableId};
use ibschema::store::{Direction, RowHandle, StoreResult};
use std::any::Any;
use std::cell::{Cell, RefCell};
use std::ops::Bound;
use std::rc::Rc;
use std::sync::Arc;

#[derive(Default)]
pub(crate) struct SetState {
    pub(super) modified: Cell<bool>,
    pub(super) by_document: Cell<bool>,
    pub(super) selected: Cell<bool>,
    pub(super) unposted: Cell<Unposted>,
    pub(super) after_rollback: Cell<AfterRollback>,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) enum Unposted {
    #[default]
    No,
    Clean,
    Refused,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) enum AfterRollback {
    #[default]
    No,
    NewWritten,
    Refused,
}

impl SetState {
    pub(super) fn changed(&self) {
        self.modified.set(true);
        self.by_document.set(false);
    }

    pub(super) fn read(&self) {
        self.modified.set(false);
        self.by_document.set(false);
    }
}

pub(crate) struct AccumRecord {
    app: Arc<Application>,
    obj: ObjId,
    ty: TypeId,
    cols: Rc<Vec<(usize, String)>>,
    cells: RefCell<Vec<Value>>,
    new: bool,
    recorder: Option<RefValue>,
    state: Rc<SetState>,
}

impl AccumRecord {
    pub(super) fn new(
        ex: &mut Execution,
        set: &AccumSet,
        values: Vec<Value>,
        new: bool,
    ) -> CallResult<Rc<AccumRecord>> {
        let bytes = values.iter().fold(
            budget::rc_size::<AccumRecord>().saturating_add(budget::slots::<Value>(values.len())),
            |n, v| n.saturating_add(budget::value_heap(v)),
        );
        budget::reserve(ex, bytes)?;
        Ok(Rc::new(AccumRecord {
            app: set.app.clone(),
            obj: set.obj,
            ty: type_of(&set.app, set.obj, Category::Record)?,
            cols: set.cols.clone(),
            cells: RefCell::new(values),
            new,
            recorder: if set.doc.is_some() {
                set.recorder.get()
            } else {
                as_ref(&set.filter.item.value.borrow()).filter(|r| !r.is_empty())
            },
            state: set.state.clone(),
        }))
    }

    pub(super) fn blank(ex: &mut Execution, set: &AccumSet) -> CallResult<Vec<Value>> {
        let def = set.app.model().obj(set.obj);
        let mut out = Vec::with_capacity(set.cols.len());
        for (i, _) in set.cols.iter() {
            ex.charge(1)?;
            let f = def
                .fields
                .get(*i)
                .ok_or_else(|| internal("a field of the register"))?;
            out.push(match f.std {
                Some(StdAttr::Period) => Value::Date(BslDate::EMPTY),
                Some(StdAttr::Active) => Value::Bool(true),
                Some(StdAttr::Recorder | StdAttr::LineNumber | StdAttr::RecordType) => {
                    Value::Undefined
                }
                _ => default_of(&set.app, f)?,
            });
        }
        Ok(out)
    }

    pub(super) fn is_new(&self) -> bool {
        self.new
    }

    pub(super) fn values(&self) -> Vec<Value> {
        self.cells.borrow().clone()
    }

    fn refuse(&self, member: &str) -> HostError {
        HostError::not_implemented("РегистрНакопленияЗапись", member)
    }

    fn field(&self, col: usize) -> Result<(&FieldDef, &str), HostError> {
        let (i, name) = self
            .cols
            .get(col)
            .ok_or_else(|| internal("a column of a record"))?;
        let f = self
            .app
            .model()
            .obj(self.obj)
            .fields
            .get(*i)
            .ok_or_else(|| internal("a field of the register"))?;
        Ok((f, name.as_str()))
    }

    fn column(&self, ex: &Execution, m: MemberId) -> Result<usize, HostError> {
        if m.0 >= FIELD_BASE {
            return usize::try_from(m.0 - FIELD_BASE)
                .ok()
                .filter(|i| *i < self.cols.len())
                .ok_or_else(|| internal("unknown field of a record"));
        }
        let Some((false, ru)) = template_member(ex, self.ty, m) else {
            let name = template_member(ex, self.ty, m)
                .map_or_else(|| m.0.to_string(), |n| n.1.to_string());
            return Err(self.refuse(&name));
        };
        self.cols
            .iter()
            .position(|(_, n)| fold_name(n) == fold_name(ru))
            .ok_or_else(|| crate::meta::no_field(ru))
    }

    fn read(&self, col: usize) -> CallResult<Value> {
        let (f, name) = self.field(col)?;
        let v = self
            .cells
            .borrow()
            .get(col)
            .cloned()
            .ok_or_else(|| internal("a cell of a record"))?;
        let unread = match f.std {
            Some(StdAttr::Recorder | StdAttr::LineNumber) => self.new,
            Some(StdAttr::RecordType) => matches!(v, Value::Undefined),
            _ => false,
        };
        if unread {
            return Err(self.refuse(&format!("{name}: новая запись")).into());
        }
        Ok(v)
    }

    fn assign(&self, ex: &mut Execution, col: usize, v: Value) -> CallResult<()> {
        let (f, name) = self.field(col)?;
        let catalog = platform_catalog::catalog();
        let cast = match f.std {
            Some(StdAttr::Recorder) if self.recorder.is_some() && as_ref(&v) == self.recorder => v,
            Some(StdAttr::Recorder) => {
                return Err(self
                    .refuse(&format!("{name}: значение не регистратор набора"))
                    .into());
            }
            Some(StdAttr::LineNumber) => {
                return Err(self.refuse(&format!("{name} (запись)")).into());
            }
            Some(StdAttr::RecordType) => {
                let kind = catalog.find_type("ВидДвиженияНакопления");
                match bsl_stdlib::enums::as_enum_value(&v) {
                    Some(e) if Some(e.type_idx()) == kind => v,
                    _ => {
                        return Err(self
                            .refuse(&format!("{name}: значение не ВидДвиженияНакопления"))
                            .into());
                    }
                }
            }
            Some(StdAttr::Period) => match v {
                Value::Date(_) => v,
                _ => return Err(self.refuse(&format!("{name}: значение не Дата")).into()),
            },
            Some(StdAttr::Active) => match v {
                Value::Bool(_) => v,
                _ => return Err(self.refuse(&format!("{name}: значение не Булево")).into()),
            },
            _ => crate::values::typedesc::cast_to_field(&self.app, ex, f, v)?,
        };
        budget::reserve(ex, budget::value_heap(&cast))?;
        if let Some(slot) = self.cells.borrow_mut().get_mut(col) {
            *slot = cast;
        }
        self.state.changed();
        Ok(())
    }
}

impl HostObject for AccumRecord {
    fn retained_memory(&self, visit: &mut dyn RetainedMemory) -> Result<bool, Exception> {
        visit.work(1)?;
        visit.mutable();
        let cells = self.cells.borrow();
        visit.bytes(budget::slots::<Value>(cells.capacity()))?;
        for v in cells.iter() {
            visit.value(v)?;
        }
        Ok(true)
    }

    fn type_id(&self) -> TypeId {
        self.ty
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let col = self.column(ex, m)?;
        self.read(col)
    }

    fn set(&self, ex: &mut Execution, m: MemberId, v: Value) -> CallResult<()> {
        let col = self.column(ex, m)?;
        self.assign(ex, col, v)
    }

    fn call(&self, ex: &mut Execution, m: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        let name =
            template_member(ex, self.ty, m).map_or_else(|| m.0.to_string(), |n| n.1.to_string());
        Err(self.refuse(&name).into())
    }

    fn find_member(&self, name: &str) -> Option<MemberId> {
        let wanted = fold_name(name);
        let i = self.cols.iter().position(|(_, n)| fold_name(n) == wanted)?;
        FIELD_BASE.checked_add(u32::try_from(i).ok()?).map(MemberId)
    }

    fn member_kind(&self, m: MemberId) -> Option<MemberKind> {
        let i = usize::try_from(m.0.checked_sub(FIELD_BASE)?).ok()?;
        let (f, _) = self.field(i).ok()?;
        Some(MemberKind::Property {
            readable: true,
            writable: f.std != Some(StdAttr::LineNumber),
        })
    }

    fn property_names(&self, ex: &mut Execution, retained: u64) -> CallResult<Option<Vec<Str>>> {
        let bytes = self
            .cols
            .iter()
            .fold(budget::slots::<Str>(self.cols.len()), |n, (_, name)| {
                n.saturating_add((name.len() as u64).saturating_mul(2).saturating_add(64))
            });
        budget::reserve(ex, retained.saturating_add(bytes))?;
        Ok(Some(
            self.cols
                .iter()
                .map(|(_, name)| Str::from(name.as_str()))
                .collect(),
        ))
    }
}

struct Layout<'a> {
    app: &'a Application,
    obj: ObjId,
    table: TableId,
    ti: &'a ibschema::storage::TableInfo,
    fields: &'a [Option<FieldId>],
    by_recorder: IndexId,
}

fn refuse_write(what: &str) -> HostError {
    HostError::not_implemented(
        "РегистрНакопленияНаборЗаписей",
        &format!("Записать: {what}"),
    )
}

fn layout<'a>(
    ex: &mut Execution,
    app: &'a Arc<Application>,
    obj: ObjId,
    posting: bool,
    recorder: RefValue,
) -> CallResult<Layout<'a>> {
    Reg::new(app, obj, "Записать").check_shape()?;
    let def = app.model().obj(obj);
    if def.behavior.register_type == Some(AccumRegType::Turnovers)
        && (!posting
            || def
                .fields
                .iter()
                .any(|f| f.std == Some(StdAttr::RecordType)))
    {
        return Err(refuse_write("регистр оборотов").into());
    }
    if app
        .modules()
        .of_object(obj, md_model::ModuleKind::RecordSetModule)
        .is_some()
    {
        return Err(refuse_write("модуль набора записей").into());
    }
    if events::has_write_subscriptions(app, def, "RecordSet") {
        return Err(refuse_write("подписка на события набора").into());
    }
    if let Some(why) = crate::extensions::write_refusal(app, def, "RecordSet") {
        return Err(refuse_write(&why).into());
    }
    if crate::history::kept(app, obj) {
        return Err(refuse_write("история данных регистра").into());
    }
    if app.options().read_only_scope.is_some() {
        return Err(refuse_write("приложение только для чтения (read_scope)").into());
    }
    let privileged_posting = posting
        && app
            .model()
            .obj(recorder.obj)
            .behavior
            .post_in_privileged_mode;
    let in_call = call_ctx(ex).map(|c| c.sess.user().no_users);
    if !(privileged_posting && in_call.is_some()) && in_call != Some(true) {
        return Err(HostError::not_implemented("P6", "application write security provider").into());
    }
    let st = app
        .model()
        .storage_of(obj)
        .ok_or_else(|| internal("the register has no storage"))?;
    if st.service.iter().any(|(p, _)| p == role::CHANGE_RECORDS) {
        return Err(refuse_write("регистрация изменений регистра").into());
    }
    let ti = app
        .store()
        .storage()
        .try_table(st.main)
        .ok_or_else(|| internal("no main table of the register"))?;
    if !ti.separators.is_empty() {
        return Err(refuse_write("разделённые данные").into());
    }
    let recorder = def
        .fields
        .iter()
        .position(|f| f.std == Some(StdAttr::Recorder))
        .and_then(|i| st.fields.get(i).copied().flatten())
        .ok_or_else(|| internal("a register without a recorder"))?;
    let pos = ti
        .indexes
        .iter()
        .position(|ix| ix.unique && ix.prefix == 0 && ix.fields.first() == Some(&recorder))
        .ok_or_else(|| refuse_write("нет уникального индекса регистратора"))?;
    Ok(Layout {
        app,
        obj,
        table: st.main,
        ti,
        fields: &st.fields,
        by_recorder: IndexId::Named(u16::try_from(pos).map_err(|_| internal("index number"))?),
    })
}

fn rows_of(
    ex: &mut Execution,
    lay: &Layout<'_>,
    set: &AccumSet,
    r: RefValue,
) -> CallResult<Vec<Row>> {
    let app = lay.app;
    let def = app.model().obj(lay.obj);
    let mut hints: Vec<Option<&FieldDef>> = vec![None; lay.ti.fields.len()];
    for (f, fid) in def.fields.iter().zip(lay.fields) {
        if let Some(slot) = fid.and_then(|fid| hints.get_mut(usize::from(fid.0))) {
            *slot = Some(f);
        }
    }
    let recorder = crate::values::refs::ref_value(app, r)?;
    let receipt = bsl_stdlib::api::system_enum_value("ВидДвиженияНакопления", "Приход");
    let expense = bsl_stdlib::api::system_enum_value("ВидДвиженияНакопления", "Расход");
    let records: Vec<Rc<AccumRecord>> = set.records.borrow().clone();
    let mut rows = Vec::with_capacity(records.len());
    for (n, record) in records.iter().enumerate() {
        ex.charge(1)?;
        let values = record.values();
        let mut row = crate::predefined::blank_row(app, lay.ti, &hints);
        for ((i, name), v) in set.cols.iter().zip(values) {
            let f = def
                .fields
                .get(*i)
                .ok_or_else(|| internal("a field of the register"))?;
            let stored = match f.std {
                Some(StdAttr::Recorder) => to_field(app, f, &recorder)?,
                Some(StdAttr::LineNumber) => {
                    let line = i64::try_from(n + 1).map_err(|_| internal("a line number"))?;
                    to_field(app, f, &Value::from(line))?
                }
                Some(StdAttr::Period) => match v {
                    Value::Date(d) if d != BslDate::EMPTY => to_field(app, f, &v)?,
                    _ => return Err(refuse_write("пустой период записи").into()),
                },
                Some(StdAttr::Active) => match v {
                    Value::Bool(true) => FieldValue::Bool(true),
                    _ => return Err(refuse_write("неактивная запись").into()),
                },
                Some(StdAttr::RecordType) => {
                    let kind = if receipt.as_ref().is_some_and(|x| same_enum(x, &v)) {
                        0
                    } else if expense.as_ref().is_some_and(|x| same_enum(x, &v)) {
                        1
                    } else {
                        return Err(refuse_write(&format!("{name}: вид движения не задан")).into());
                    };
                    to_field(app, f, &Value::from(kind))?
                }
                _ => to_field(app, f, &v)?,
            };
            let fid = lay
                .fields
                .get(*i)
                .copied()
                .flatten()
                .ok_or_else(|| internal("a column without storage"))?;
            if let Some(slot) = row.get_mut(usize::from(fid.0)) {
                *slot = stored;
            }
        }
        rows.push(row);
    }
    Ok(rows)
}

fn same_enum(a: &Value, b: &Value) -> bool {
    match (
        bsl_stdlib::enums::as_enum_value(a),
        bsl_stdlib::enums::as_enum_value(b),
    ) {
        (Some(x), Some(y)) => x.type_idx() == y.type_idx() && x.name_en() == y.name_en(),
        _ => false,
    }
}

fn store(
    ex: &mut Execution,
    app: &Arc<Application>,
    txn: &Rc<CallTxn>,
    set: &AccumSet,
    r: RefValue,
    posting: bool,
) -> CallResult<()> {
    let lay = layout(ex, app, set.obj, posting, r)?;
    let rows = rows_of(ex, &lay, set, r)?;
    let kept = match (posting, reposting(r)) {
        (true, true) => Kept::Repost,
        (true, false) => Kept::New,
        (false, _) => Kept::Prefix,
    };
    replace(ex, app, txn, &lay, rows, r, kept)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kept {
    Nothing,
    Prefix,
    New,
    Repost,
}

thread_local! {
    static REPOSTING: RefCell<Vec<RefValue>> = const { RefCell::new(Vec::new()) };
}

fn reposting(r: RefValue) -> bool {
    REPOSTING.with(|v| v.borrow().contains(&r))
}

fn same_row(a: &Row, b: &Row) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| match (x, y) {
            (FieldValue::Number(m), FieldValue::Number(n)) => m.eq_value(n),
            _ => x == y,
        })
}

pub(crate) fn remove_records(
    ex: &mut Execution,
    app: &Arc<Application>,
    txn: &Rc<CallTxn>,
    reg: ObjId,
    r: RefValue,
) -> CallResult<()> {
    let lay = layout(ex, app, reg, true, r)?;
    replace(ex, app, txn, &lay, Vec::new(), r, Kept::Nothing)
}

fn replace(
    ex: &mut Execution,
    app: &Arc<Application>,
    txn: &Rc<CallTxn>,
    lay: &Layout<'_>,
    rows: Vec<Row>,
    r: RefValue,
    kept: Kept,
) -> CallResult<()> {
    let reg = lay.obj;
    let def = app.model().obj(reg);
    let recorder_field = def
        .fields
        .iter()
        .find(|f| f.std == Some(StdAttr::Recorder))
        .ok_or_else(|| internal("a register without a recorder"))?;
    let key = vec![to_field(
        app,
        recorder_field,
        &crate::values::refs::ref_value(app, r)?,
    )?];
    let bytes = rows
        .iter()
        .fold(0u64, |n, row| n.saturating_add(budget::stored_row(row)));
    budget::reserve(ex, bytes.saturating_mul(2))?;
    if crate::data_lock::lock_for_write(app, txn, def).is_err() {
        return Err(crate::data_lock::conflict().into());
    }
    let written = txn.with_write(&**app, |w| -> StoreResult<Option<Rewrite>> {
        let found = w
            .range(
                lay.table,
                lay.by_recorder,
                Bound::Included(key.as_slice()),
                Bound::Included(key.as_slice()),
                Direction::Forward,
            )?
            .collect::<StoreResult<Vec<(RowHandle, Row)>>>()?;
        if kept == Kept::New && !found.is_empty() {
            return Ok(None);
        }
        let set: &[Row] = if kept == Kept::Nothing { &[] } else { &rows };
        Ok(Some(rewrite(w, lay.table, found, set)?))
    });
    let Some(done) = written.map_err(|e| e.call_error(ex))? else {
        return Err(refuse_write("проведение документа с записанными движениями").into());
    };
    totals(ex, app, txn, reg, &done.removed, &done.added)?;
    crate::commit_guard::guard(app, txn, reg);
    Ok(())
}

struct Rewrite {
    removed: Vec<Row>,
    added: Vec<Row>,
}

fn rewrite(
    w: &mut dyn ibschema::store::WriteTxn,
    table: TableId,
    found: Vec<(RowHandle, Row)>,
    set: &[Row],
) -> StoreResult<Rewrite> {
    let mut out = Rewrite {
        removed: Vec::new(),
        added: Vec::new(),
    };
    let mut same = Vec::with_capacity(found.len());
    for (i, (h, old)) in found.into_iter().enumerate() {
        let kept = set.get(i).is_some_and(|new| same_row(&old, new));
        same.push(kept);
        if !kept {
            w.delete(table, h)?;
            out.removed.push(old);
        }
    }
    for (i, new) in set.iter().enumerate() {
        if same.get(i).copied().unwrap_or(false) {
            continue;
        }
        w.insert(table, new)?;
        out.added.push(new.clone());
    }
    Ok(out)
}

fn totals(
    ex: &mut Execution,
    app: &Application,
    txn: &CallTxn,
    reg: ObjId,
    removed: &[Row],
    added: &[Row],
) -> CallResult<()> {
    if removed.is_empty() && added.is_empty() {
        return Ok(());
    }
    let of = |rows: &[Row]| {
        rows.iter()
            .map(|row| crate::totals::Movement::of_row(app, reg, row))
            .collect::<Result<Vec<_>, HostError>>()
    };
    let (removed, added) = (of(removed)?, of(added)?);
    let ch = crate::totals::Change {
        reg,
        removed: &removed,
        added: &added,
    };
    let plan = txn
        .with_write(app, |w| Ok(crate::totals::plan(app, &*w, &ch)))
        .map_err(|e| e.call_error(ex))??;
    crate::totals::apply(txn, app, &plan).map_err(|e| e.call_error(ex))?;
    Ok(())
}

fn movements_recorder(set: &AccumSet) -> CallResult<RefValue> {
    if set.posting.get() {
        return Err(refuse_write("набор в ОбработкаПроведения").into());
    }
    let Some(r) = set.recorder.get() else {
        return Err(refuse_write("набор нового документа").into());
    };
    let item = &set.filter.item;
    if !item.used.get() || as_ref(&item.value.borrow()) != Some(r) {
        return Err(refuse_write("отбор изменён").into());
    }
    if !set.read.get() {
        return Err(refuse_write("набор не прочитан").into());
    }
    if set.read_for.get() != Some(r) {
        return Err(refuse_write("набор прочитан по другому отбору").into());
    }
    Ok(r)
}

fn manager_recorder(ex: &mut Execution, set: &AccumSet) -> CallResult<RefValue> {
    if !set.read.get() {
        return Err(refuse_write("набор не прочитан").into());
    }
    let Some(r) = set.read_for.get() else {
        return Err(refuse_write("отбор по Неопределено").into());
    };
    let item = &set.filter.item;
    if !item.used.get() || as_ref(&item.value.borrow()) != Some(r) {
        return Err(refuse_write("отбор изменён").into());
    }
    if !CallTxn::of_call(ex).is_some_and(|t| t.innermost_explicit()) {
        return Err(refuse_write("вне транзакции программы").into());
    }
    if reposting(r) {
        return Err(refuse_write("набор документа, который сейчас проводится").into());
    }
    Ok(r)
}

pub(super) fn write_set(ex: &mut Execution, set: &AccumSet, args: &[Arg]) -> CallResult<()> {
    if !args.is_empty() {
        return Err(refuse_write("параметр Замещать").into());
    }
    let r = if set.doc.is_some() {
        movements_recorder(set)?
    } else {
        manager_recorder(ex, set)?
    };
    let app = set.app.clone();
    let txn =
        CallTxn::of_call(ex).ok_or_else(|| internal("the write of a record set outside a call"))?;
    txn.check().map_err(|e| e.host())?;
    let modified = set.state.modified.get();
    txn.begin();
    let ended = match store(ex, &app, &txn, set, r, false) {
        Ok(()) => crate::fo_version::commit(ex, &app, &txn),
        Err(e) => {
            let _ = txn.rollback(&*app);
            Err(e)
        }
    };
    if matches!(ended, Ok(Commit::Committed | Commit::Inner)) {
        set.state.read();
        if set.doc.is_none() {
            let state = set.state.clone();
            txn.on_rollback(move || state.after_rollback.set(AfterRollback::Refused));
        }
    } else {
        set.state.modified.set(modified);
    }
    ended.map(|_| ())
}

pub(crate) fn document_movements(inst: &ObjectInstance, is_new: bool) -> CallResult<Vec<Value>> {
    let Some(movements) = inst.state.borrow().movements.clone() else {
        return Ok(Vec::new());
    };
    let Some(m) = movements
        .as_object()
        .and_then(|o| o.as_any().downcast_ref::<Movements>())
    else {
        return Err(internal("the movements of a document").into());
    };
    let sets: Vec<Value> = m
        .sets
        .borrow()
        .iter()
        .flatten()
        .filter(|v| set_of(v).is_some_and(|s| s.state.modified.get()))
        .cloned()
        .collect();
    if sets.is_empty() {
        return Ok(sets);
    }
    let def = inst.app.model().obj(inst.obj);
    let refuse = |what: &str| -> bsl_vm::CallError {
        HostError::not_implemented(
            &inst.type_name(),
            &format!("Записать: движения документа: {what}"),
        )
        .into()
    };
    if def.kind != ObjKind::Document
        || def.behavior.posting != Posting::Deny
        || def.behavior.register_records_writing != RecordsWriting::WriteModified
    {
        return Err(refuse("документ с проведением"));
    }
    if def.behavior.register_records_deletion != RecordsDeletion::AutoDelete {
        return Err(refuse("удаление движений не автоматическое"));
    }
    if !is_new {
        return Err(refuse("записанный документ"));
    }
    if events::handled(inst, Event::OnWrite) {
        return Err(refuse("обработчик ПриЗаписи"));
    }
    Ok(sets)
}

pub(crate) fn sets_touched(inst: &ObjectInstance) -> CallResult<bool> {
    let Some(movements) = inst.state.borrow().movements.clone() else {
        return Ok(false);
    };
    let m = movements_of(&movements)?;
    Ok(m.sets.borrow().iter().flatten().any(|v| {
        set_of(v).is_some_and(|s| s.read.get() || s.state.modified.get() || s.state.selected.get())
    }))
}

pub(crate) fn movements_unposted(
    ex: &mut Execution,
    inst: &ObjectInstance,
    txn: &CallTxn,
    unpost: bool,
) -> CallResult<()> {
    let movements = inst.movements_of(ex)?;
    let m = movements_of(&movements)?;
    let later = if unpost {
        Unposted::Clean
    } else {
        Unposted::Refused
    };
    m.unposted.set(later);
    let mut states = Vec::new();
    for v in m.sets.borrow().iter().flatten() {
        let Some(set) = set_of(v) else { continue };
        let held = set.state.by_document.get() || !set.records.borrow().is_empty();
        set.state
            .unposted
            .set(if held { Unposted::Refused } else { later });
        states.push(set.state.clone());
    }
    let flag = m.unposted.clone();
    txn.on_rollback(move || {
        flag.set(Unposted::No);
        for s in states {
            s.unposted.set(Unposted::Refused);
        }
    });
    Ok(())
}

pub(crate) fn any_set_changed(inst: &ObjectInstance) -> CallResult<bool> {
    let Some(movements) = inst.state.borrow().movements.clone() else {
        return Ok(false);
    };
    let m = movements_of(&movements)?;
    Ok(m.sets
        .borrow()
        .iter()
        .flatten()
        .any(|v| set_of(v).is_some_and(|s| s.state.modified.get() || s.state.selected.get())))
}

fn set_of(v: &Value) -> Option<&AccumSet> {
    v.as_object()?.as_any().downcast_ref::<AccumSet>()
}

pub(crate) fn write_movements(
    ex: &mut Execution,
    app: &Arc<Application>,
    txn: &Rc<CallTxn>,
    sets: &[Value],
    r: RefValue,
) -> CallResult<()> {
    for v in sets {
        let set = set_of(v).ok_or_else(|| internal("a record set"))?;
        store(ex, app, txn, set, r, false)?;
    }
    Ok(())
}

pub(crate) fn movements_written(
    inst: &ObjectInstance,
    _txn: &CallTxn,
    sets: &[Value],
    r: RefValue,
) -> CallResult<()> {
    let Some(movements) = inst.state.borrow().movements.clone() else {
        return Ok(());
    };
    let Some(m) = movements
        .as_object()
        .and_then(|o| o.as_any().downcast_ref::<Movements>())
    else {
        return Err(internal("the movements of a document").into());
    };
    m.r.set(Some(r));
    let value = crate::values::refs::ref_value(&inst.app, r)?;
    for v in m.sets.borrow().iter().flatten() {
        let Some(set) = set_of(v) else { continue };
        set.recorder.set(Some(r));
        *set.filter.item.value.borrow_mut() = value.clone();
    }
    for v in sets {
        if let Some(set) = set_of(v) {
            set.state.modified.set(false);
            set.state.by_document.set(true);
        }
    }
    Ok(())
}

pub(crate) fn movements_after_rollback(
    inst: &ObjectInstance,
    txn: &CallTxn,
    sets: &[Value],
    not_write: bool,
) -> CallResult<()> {
    let was_new = inst
        .state
        .borrow()
        .written
        .as_ref()
        .is_some_and(|w| w.was_new);
    let Some(movements) = inst.state.borrow().movements.clone() else {
        return Ok(());
    };
    let m = movements_of(&movements)?;
    let mut states = Vec::new();
    for v in m.sets.borrow().iter().flatten() {
        let Some(set) = set_of(v) else { continue };
        let written = sets
            .iter()
            .any(|w| set_of(w).is_some_and(|s| std::ptr::eq(s, set)));
        let after = if written && was_new && !not_write {
            AfterRollback::NewWritten
        } else {
            AfterRollback::Refused
        };
        states.push((set.state.clone(), after));
    }
    let later = was_new.then(|| m.after_rollback.clone());
    txn.on_rollback(move || {
        for (state, after) in states {
            state.by_document.set(false);
            if state.after_rollback.get() != AfterRollback::Refused {
                state.after_rollback.set(after);
            }
        }
        if let Some(later) = later {
            later.set(true);
        }
    });
    Ok(())
}

pub(crate) fn movements_failed_new(inst: &ObjectInstance, txn: &CallTxn) -> CallResult<()> {
    let Some(movements) = inst.state.borrow().movements.clone() else {
        return Ok(());
    };
    let m = movements_of(&movements)?;
    let states: Vec<Rc<SetState>> = m
        .sets
        .borrow()
        .iter()
        .flatten()
        .filter_map(|v| set_of(v).map(|s| s.state.clone()))
        .collect();
    let later = m.after_rollback.clone();
    txn.on_rollback(move || {
        for state in states {
            state.after_rollback.set(AfterRollback::Refused);
        }
        later.set(true);
    });
    Ok(())
}

fn movements_of(v: &Value) -> CallResult<&Movements> {
    v.as_object()
        .and_then(|o| o.as_any().downcast_ref::<Movements>())
        .ok_or_else(|| internal("the movements of a document").into())
}

pub(crate) fn posting_begin(
    inst: &ObjectInstance,
    movements: &Value,
    r: RefValue,
    repost: bool,
) -> CallResult<()> {
    let app = &*inst.app;
    let m = movements_of(movements)?;
    if repost
        && m.sets.borrow().iter().flatten().any(|v| {
            set_of(v).is_some_and(|s| !s.state.by_document.get() && !s.records.borrow().is_empty())
        })
    {
        return Err(HostError::not_implemented(
            &inst.type_name(),
            "Записать: перепроведение объекта с движениями в памяти",
        )
        .into());
    }
    m.r.set(Some(r));
    if repost {
        REPOSTING.with(|v| v.borrow_mut().push(r));
    }
    let value = crate::values::refs::ref_value(app, r)?;
    for v in m.sets.borrow().iter().flatten() {
        let Some(set) = set_of(v) else { continue };
        set.recorder.set(Some(r));
        *set.filter.item.value.borrow_mut() = value.clone();
        set.state.selected.set(false);
    }
    m.posting.set(true);
    Ok(())
}

pub(crate) fn posting_end(movements: &Value, last: bool) {
    if let Ok(m) = movements_of(movements) {
        m.posting.set(false);
        if let Some(r) = m.r.get().filter(|_| last) {
            REPOSTING.with(|v| {
                let mut v = v.borrow_mut();
                if let Some(i) = v.iter().position(|x| *x == r) {
                    v.remove(i);
                }
            });
        }
    }
}

pub(crate) fn write_selected(ex: &mut Execution, m: &Movements) -> CallResult<()> {
    let Some(r) = m.r.get() else {
        return Err(internal("the posting of a document without its reference").into());
    };
    let txn = CallTxn::of_call(ex).ok_or_else(|| internal("the posting outside a call"))?;
    let sets: Vec<Value> = m.sets.borrow().iter().flatten().cloned().collect();
    for v in sets {
        let Some(set) = set_of(&v) else { continue };
        if !set.state.selected.get() {
            continue;
        }
        if m.written
            .borrow()
            .iter()
            .any(|w| set_of(w).is_some_and(|s| std::ptr::eq(s, set)))
        {
            return Err(refuse_write("набор, уже записанный этим проведением").into());
        }
        store(ex, &m.app, &txn, set, r, true)?;
        set.state.selected.set(false);
        set.state.modified.set(false);
        set.state.by_document.set(false);
        m.written.borrow_mut().push(v.clone());
    }
    Ok(())
}

pub(crate) fn posting_finish(
    ex: &mut Execution,
    inst: &ObjectInstance,
    movements: &Value,
) -> CallResult<Vec<Value>> {
    let m = movements_of(movements)?;
    for v in m.sets.borrow().iter().flatten() {
        let Some(set) = set_of(v) else { continue };
        if set.state.modified.get() && !set.state.selected.get() {
            let name = &inst.app.model().obj(set.obj).name;
            return Err(HostError::not_implemented(
                &inst.type_name(),
                &format!(
                    "Записать: набор {name} изменён в ОбработкаПроведения без Записывать = Истина"
                ),
            )
            .into());
        }
    }
    write_selected(ex, m)?;
    if let Some(r) = m.r.get().filter(|r| reposting(*r)) {
        let app = inst.app.clone();
        let regs: Vec<ObjId> = {
            let model = app.model();
            model
                .obj(inst.obj)
                .behavior
                .register_records
                .iter()
                .filter_map(|md| model.by_uuid(*md))
                .collect()
        };
        let txn = CallTxn::of_call(ex).ok_or_else(|| internal("the posting outside a call"))?;
        for reg in regs {
            let wrote = m
                .written
                .borrow()
                .iter()
                .any(|w| set_of(w).is_some_and(|s| s.obj == reg));
            if !wrote && has_records(ex, &app, &txn, reg, r)? {
                let name = &app.model().obj(reg).name;
                return Err(HostError::not_implemented(
                    &inst.type_name(),
                    &format!("Записать: перепроведение без записи набора {name}"),
                )
                .into());
            }
        }
    }
    Ok(std::mem::take(&mut *m.written.borrow_mut()))
}

fn has_records(
    ex: &mut Execution,
    app: &Arc<Application>,
    txn: &Rc<CallTxn>,
    reg: ObjId,
    r: RefValue,
) -> CallResult<bool> {
    let lay = layout(ex, app, reg, true, r)?;
    let def = app.model().obj(reg);
    let recorder_field = def
        .fields
        .iter()
        .find(|f| f.std == Some(StdAttr::Recorder))
        .ok_or_else(|| internal("a register without a recorder"))?;
    let key = vec![to_field(
        app,
        recorder_field,
        &crate::values::refs::ref_value(app, r)?,
    )?];
    let mut view = txn.view(&**app).map_err(|e| e.call_error(ex))?;
    let found = view
        .txn()
        .range(
            lay.table,
            lay.by_recorder,
            Bound::Included(key.as_slice()),
            Bound::Included(key.as_slice()),
            Direction::Forward,
        )
        .and_then(|mut rows| rows.next().transpose().map(|r| r.is_some()));
    found.map_err(|e| crate::read::store_error(&e).into())
}
