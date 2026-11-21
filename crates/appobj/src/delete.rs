use crate::Application;
use crate::codes::{Layout, open_read};
use crate::data_lock;
use crate::events;
use crate::exchange_params::ExchangeParams;
use crate::meta::arg_value;
use crate::model::{
    FieldDef, FieldRole, ObjDef, ObjId, ObjKind, Posting, RecordsDeletion, StdAttr, TypeAtom,
};
use crate::objects::{ObjectInstance, not_selected, stored_instance};
use crate::predefined::AreaKey;
use crate::read::{read_fresh, read_view, store_call_error};
use crate::txn::{CallTxn, Commit};
use crate::values::field::to_field;
use crate::values::refs::RefValue;
use bsl_prim::fold_name;
use bsl_vm::{Arg, CallResult, Execution, HostError, Value};
use ibschema::fields::FieldValue;
use ibschema::names::StdField;
use ibschema::storage::{FieldId, IndexId, TableId, TableInfo};
use ibschema::store::{Direction, ReadTxn, RowHandle, StoreResult, WriteTxn};
use ibschema::values::RefId;
use platform_catalog::messages::Lang;
use std::collections::HashMap;
use std::ops::Bound;
use std::rc::Rc;
use std::sync::Arc;

fn internal(text: &str) -> HostError {
    HostError::text(&format!("internal: {text}"), &format!("internal: {text}"))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Op {
    Delete,
    Mark(bool),
}

impl Op {
    fn method(self) -> &'static str {
        match self {
            Op::Delete => "Удалить",
            Op::Mark(_) => "УстановитьПометкуУдаления",
        }
    }
}

fn type_name(def: &ObjDef) -> String {
    format!("{}Объект", def.kind.name(Lang::Ru))
}

fn refuse(def: &ObjDef, op: Op, what: &str) -> HostError {
    HostError::not_implemented(&type_name(def), &format!("{}: {what}", op.method()))
}

#[derive(Clone, Debug)]
struct Node {
    r: RefValue,
    marked: bool,
    owner: Option<FieldValue>,
}

struct Master {
    reg: ObjId,
    table: TableId,
    field: FieldId,
    guarded: bool,
}

enum Probe {
    Index {
        index: IndexId,
        key: Vec<FieldValue>,
    },
    Area {
        index: IndexId,
        key: Vec<FieldValue>,
    },
    All,
}

enum Removed {
    Done(Node),
    Missing,
    Stale,
    Guarded(ObjId),
    Recorded(ObjId),
}

fn recorders(app: &Application, def: &ObjDef) -> Result<Vec<(ObjId, TableId, IndexId)>, HostError> {
    let model = app.model();
    let map = app.store().storage();
    let mut out = Vec::new();
    for md in &def.behavior.register_records {
        let reg = model
            .by_uuid(*md)
            .ok_or_else(|| internal("an unknown register of the records of a document"))?;
        let st = model
            .storage_of(reg)
            .ok_or_else(|| internal("a register without storage"))?;
        let ti = map
            .try_table(st.main)
            .ok_or_else(|| internal("no table of the register"))?;
        let index = ti
            .indexes
            .iter()
            .position(|ix| {
                ix.prefix == 0
                    && ix
                        .fields
                        .first()
                        .and_then(|f| ti.field(*f))
                        .is_some_and(|f| f.std == Some(StdField::Recorder))
            })
            .and_then(|k| u16::try_from(k).ok())
            .map(IndexId::Named)
            .ok_or_else(|| refuse(def, Op::Delete, "регистр без индекса по регистратору"))?;
        out.push((reg, st.main, index));
    }
    Ok(out)
}

pub(crate) fn delete_document(ex: &mut Execution, inst: &ObjectInstance) -> CallResult<Value> {
    inst.live()?;
    let def = inst.app.model().obj(inst.obj);
    let posted = match def
        .fields
        .iter()
        .position(|f| f.std == Some(StdAttr::Posted))
    {
        Some(i) => matches!(inst.attr(ex, i)?, Value::Bool(true)),
        None => false,
    };
    if posted {
        crate::posting::undo_check(inst, "Удалить", events::Event::BeforeDelete)?;
    }
    let unrecord = if !posted && def.behavior.posting == Posting::Deny {
        let txn = CallTxn::of_call(ex).ok_or_else(|| internal("a deletion outside a call"))?;
        Unrecord::Deny(deny_check(inst, inst.written_in(&txn)).err())
    } else {
        Unrecord::No
    };
    delete_with(ex, inst, posted, unrecord)
}

enum Unrecord {
    No,
    Deny(Option<bsl_vm::CallError>),
}

fn deny_check(inst: &ObjectInstance, written: bool) -> CallResult<()> {
    let app = inst.app.clone();
    let model = app.model();
    let def = model.obj(inst.obj);
    let refuse = |what: &str| -> bsl_vm::CallError {
        refuse(def, Op::Delete, &format!("документ с движениями: {what}")).into()
    };
    if def.behavior.register_records_deletion != RecordsDeletion::AutoDelete {
        return Err(refuse("удаление движений не AutoDelete"));
    }
    for md in &def.behavior.register_records {
        if model.by_uuid(*md).map(|o| model.obj(o).kind) != Some(ObjKind::AccumulationRegister) {
            return Err(refuse("движения не в регистре накопления"));
        }
    }
    if events::handled(inst, events::Event::BeforeDelete) {
        return Err(refuse("обработчик ПередУдалением"));
    }
    if inst.loading() {
        return Err(refuse("ОбменДанными.Загрузка документа"));
    }
    if inst.state.borrow().modified.get() {
        return Err(refuse("объект изменён"));
    }
    if crate::regread::sets_touched(inst)? {
        return Err(refuse("движения документа прочитаны или изменены"));
    }
    if written {
        return Err(refuse("документ, записанный в этой транзакции"));
    }
    Ok(())
}

struct Walk<'a> {
    app: &'a Arc<Application>,
    txn: Rc<CallTxn>,
    op: Op,
    def: &'a ObjDef,
    masters: HashMap<ObjId, Rc<Vec<Master>>>,
    top: Option<(Rc<ExchangeParams>, bool)>,
    posted: bool,
    unrecord: Unrecord,
}

fn admit(
    ex: &mut Execution,
    app: &Application,
    def: &ObjDef,
    op: Op,
    inst: &ObjectInstance,
) -> CallResult<()> {
    if app.options().read_only_scope.is_some() {
        return Err(refuse(def, op, "приложение только для чтения (read_scope)").into());
    }
    let separated = app.model().storage_of(def.id).is_some_and(|st| {
        let map = app.store().storage();
        std::iter::once(st.main)
            .chain(st.tabular.iter().map(|(t, _)| *t))
            .any(|t| map.try_table(t).is_some_and(|ti| !ti.separators.is_empty()))
    });
    if separated && app.options().area().is_none() {
        return Err(refuse(def, op, "данные разделены, область данных не задана").into());
    }
    admit_user(ex, app, def, op, inst)
}

fn admit_user(
    ex: &mut Execution,
    app: &Application,
    def: &ObjDef,
    op: Op,
    inst: &ObjectInstance,
) -> CallResult<()> {
    use crate::write::{DataWrite, data_write, user_refusal};
    let asked = match op {
        Op::Delete => data_write(ex, app, def.md, &["Delete"])?,
        Op::Mark(_) => data_write(ex, app, def.md, &[])?,
    };
    match asked {
        DataWrite::NoUsers => return Ok(()),
        DataWrite::Privileged => {
            return Err(user_refusal("удаление или пометка в привилегированном режиме").into());
        }
        DataWrite::Denied | DataWrite::Allowed => {}
    }
    if op != Op::Delete {
        return Err(user_refusal("пометка удаления").into());
    }
    if def.kind != ObjKind::Catalog {
        return Err(user_refusal(&format!(
            "удаление объекта вида {}",
            def.kind.name(Lang::Ru)
        ))
        .into());
    }
    if inst.loading() {
        return Err(user_refusal("удаление при ОбменДанными.Загрузка").into());
    }
    if reachable(app, def.id).len() > 1 {
        return Err(user_refusal("удаление справочника с подчинёнными справочниками").into());
    }
    if asked == DataWrite::Denied {
        return Err(crate::rights::access_violation().into());
    }
    Err(user_refusal("удаление с правом Удаление").into())
}

fn subordinates(app: &Application, obj: ObjId) -> Vec<ObjId> {
    let model = app.model();
    let md = model.obj(obj).md;
    model
        .objects
        .iter()
        .filter(|o| o.kind == ObjKind::Catalog && o.behavior.owners.contains(&md))
        .map(|o| o.id)
        .collect()
}

fn reachable(app: &Application, obj: ObjId) -> Vec<ObjId> {
    let mut out = vec![obj];
    let mut i = 0;
    while let Some(&o) = out.get(i) {
        i += 1;
        for s in subordinates(app, o) {
            if !out.contains(&s) {
                out.push(s);
            }
        }
    }
    out
}

fn ensure_predefined(app: &Application, objs: &[ObjId]) -> Result<(), HostError> {
    for &obj in objs {
        app.predefined().ensure(app, obj, &AreaKey::default())?;
    }
    Ok(())
}

fn holds(app: &Application, f: &FieldDef, obj: ObjId) -> bool {
    let kind = app.model().obj(obj).kind;
    f.types.atoms.iter().any(|a| match a {
        TypeAtom::Ref(o) => *o == obj,
        TypeAtom::AnyRef(k) => k.is_none_or(|k| k == kind),
        _ => false,
    })
}

fn guarded(app: &Application, reg: &ObjDef) -> bool {
    const SET: &str = "cfg:InformationRegisterRecordSet";
    reg.modules.record_set
        || app.model().subscriptions.iter().any(|s| {
            s.source.atoms.iter().any(|a| match a {
                TypeAtom::Platform(q) => {
                    q == SET
                        || q.strip_prefix(SET)
                            .and_then(|rest| rest.strip_prefix('.'))
                            .is_some_and(|name| fold_name(name) == fold_name(&reg.name))
                }
                _ => false,
            })
        })
}

fn masters_of(app: &Application, obj: ObjId) -> Result<Vec<Master>, HostError> {
    let model = app.model();
    let mut out = Vec::new();
    for reg in model
        .objects
        .iter()
        .filter(|o| o.kind == ObjKind::InformationRegister)
    {
        let Some(md) = model.config.object(reg.md) else {
            continue;
        };
        for (i, f) in reg.fields.iter().enumerate() {
            if f.role != FieldRole::Dimension {
                continue;
            }
            let Some(id) = f.md else {
                continue;
            };
            let master = md
                .children
                .iter()
                .find(|c| c.id == id)
                .and_then(|c| c.props.iter().find(|p| p.name == "Master"))
                .is_some_and(|p| match &p.value {
                    md_model::Value::Bool(b) => *b,
                    md_model::Value::Text(t) => t == "true",
                    _ => false,
                });
            if !master || !holds(app, f, obj) {
                continue;
            }
            let st = model
                .storage_of(reg.id)
                .ok_or_else(|| internal("a register without a table"))?;
            let field = st
                .fields
                .get(i)
                .copied()
                .flatten()
                .ok_or_else(|| internal("a dimension without a field"))?;
            out.push(Master {
                reg: reg.id,
                table: st.main,
                field,
                guarded: guarded(app, reg),
            });
        }
    }
    Ok(out)
}

fn area_of(app: &Application, ti: &TableInfo) -> Result<Vec<FieldValue>, HostError> {
    if ti.separators.is_empty() {
        return Ok(Vec::new());
    }
    let scope = app
        .options()
        .area()
        .ok_or_else(|| HostError::not_implemented("Удалить", "Разделение данных"))?;
    scope.area(ti).map_err(|_| internal("invalid bound area"))
}

fn begins_with_separators(ti: &TableInfo, ix: &ibschema::storage::IndexInfo) -> bool {
    ix.prefix == ti.separators.len()
        && ix
            .fields
            .iter()
            .take(ix.prefix)
            .zip(&ti.separators)
            .all(|(f, s)| *f == s.field)
}

fn probe(app: &Application, m: &Master, value: &FieldValue) -> Result<Probe, HostError> {
    let map = app.store().storage();
    let ti = map
        .try_table(m.table)
        .ok_or_else(|| internal("no table of the register"))?;
    let area = area_of(app, ti)?;
    let named = |k: usize| u16::try_from(k).ok().map(IndexId::Named);
    for (k, ix) in ti.indexes.iter().enumerate() {
        if begins_with_separators(ti, ix)
            && ix.fields.get(ix.prefix) == Some(&m.field)
            && let Some(index) = named(k)
        {
            let mut key = area.clone();
            key.push(value.clone());
            return Ok(Probe::Index { index, key });
        }
    }
    if area.is_empty() {
        return Ok(Probe::All);
    }
    for (k, ix) in ti.indexes.iter().enumerate() {
        if begins_with_separators(ti, ix)
            && let Some(index) = named(k)
        {
            return Ok(Probe::Area { index, key: area });
        }
    }
    let reg = app.model().obj(m.reg);
    Err(HostError::not_implemented(
        &format!("{}.{}", reg.kind.name(Lang::Ru), reg.name),
        "записи по ведущему измерению без индекса области",
    ))
}

fn handles_of<R: ReadTxn + ?Sized>(
    rd: &R,
    table: TableId,
    key: &[FieldValue],
) -> StoreResult<Vec<RowHandle>> {
    rd.range(
        table,
        IndexId::Id,
        Bound::Included(key),
        Bound::Included(key),
        Direction::Forward,
    )?
    .map(|item| item.map(|(h, _)| h))
    .collect()
}

fn matching<R: ReadTxn + ?Sized>(
    rd: &R,
    m: &Master,
    probe: &Probe,
    target: (u32, RefId),
) -> StoreResult<Vec<RowHandle>> {
    let hit = |row: &[FieldValue]| matches!(row.get(usize::from(m.field.0)), Some(FieldValue::Ref { table, id }) if (*table, *id) == target);
    let fields = [m.field];
    let rows = match probe {
        Probe::Index { index, key } | Probe::Area { index, key } => rd.range_fields(
            m.table,
            *index,
            Bound::Included(key.as_slice()),
            Bound::Included(key.as_slice()),
            Direction::Forward,
            &fields,
        )?,
        Probe::All => rd.scan_fields(m.table, &fields, None)?,
    };
    let mut out = Vec::new();
    for item in rows {
        let (h, row) = item?;
        if hit(&row) {
            out.push(h);
        }
    }
    Ok(out)
}

fn id_key(
    app: &Application,
    ti: &TableInfo,
    number: u32,
    id: RefId,
) -> Result<Vec<FieldValue>, HostError> {
    let mut key = if ti.key_index.as_ref().is_some_and(|k| k.prefix != 0) {
        area_of(app, ti)?
    } else {
        Vec::new()
    };
    key.push(FieldValue::Ref { table: number, id });
    Ok(key)
}

fn mark_field(def: &ObjDef) -> Result<usize, HostError> {
    def.fields
        .iter()
        .position(|f| f.std == Some(StdAttr::DeletionMark))
        .ok_or_else(|| internal("the object has no deletion mark"))
}

impl Walk<'_> {
    fn masters(&mut self, obj: ObjId) -> Result<Rc<Vec<Master>>, HostError> {
        if let Some(m) = self.masters.get(&obj) {
            return Ok(m.clone());
        }
        let m = Rc::new(masters_of(self.app, obj)?);
        self.masters.insert(obj, m.clone());
        Ok(m)
    }

    fn scan(
        &self,
        ex: &mut Execution,
        layout: &Layout<'_>,
        prefix: &[StdField],
        tail: Vec<FieldValue>,
    ) -> CallResult<Vec<Node>> {
        let mut want = prefix.to_vec();
        if prefix.contains(&StdField::ParentId) && layout.column(StdField::Folder).is_some() {
            want.push(StdField::Folder);
        }
        want.push(StdField::Code);
        let (index, ix) = layout
            .index(&want)
            .or_else(|| layout.index(prefix))
            .ok_or_else(|| refuse(self.def, self.op, "индекс подчинённых элементов"))?;
        let mut key = layout.area_prefix(ix)?;
        key.extend(tail);
        let id = layout
            .column(StdField::Id)
            .ok_or_else(|| internal("the table has no reference"))?;
        let marked = layout.column(StdField::Marked);
        let owner = layout.column(StdField::OwnerId);
        let fields: Vec<FieldId> = [Some(id), marked, owner].into_iter().flatten().collect();
        let cell = |row: &[FieldValue], f: Option<FieldId>| {
            f.and_then(|f| row.get(usize::from(f.0))).cloned()
        };
        let txn = self.txn.clone();
        let mut view = read_view(ex, Some(&*txn), self.app)?;
        let _memory = open_read(ex, &mut view)?;
        let rows = view
            .txn()
            .range_fields(
                layout.table,
                index,
                Bound::Included(key.as_slice()),
                Bound::Included(key.as_slice()),
                Direction::Forward,
                &fields,
            )
            .map_err(|e| store_call_error(ex, e))?;
        let mut out: Vec<Node> = Vec::new();
        for item in rows {
            ex.charge(1)?;
            let (_, row) = item.map_err(|e| store_call_error(ex, e))?;
            let Some(FieldValue::Ref { id: rid, .. }) = row.get(usize::from(id.0)) else {
                return Err(internal("a row without a reference").into());
            };
            out.push(Node {
                r: RefValue {
                    obj: layout.obj,
                    id: *rid,
                },
                marked: matches!(cell(&row, marked), Some(FieldValue::Bool(true))),
                owner: cell(&row, owner),
            });
            crate::read_budget::reserve(ex, crate::read_budget::slots::<Node>(out.capacity()))?;
        }
        Ok(out)
    }

    fn below(&self, ex: &mut Execution, node: &Node) -> CallResult<Vec<Node>> {
        let app = self.app;
        let def = app.model().obj(node.r.obj);
        let own = Layout::of(app, node.r.obj)?;
        let mut out = Vec::new();
        if def.behavior.hierarchical && own.parent.is_some() {
            let mut prefix = Vec::with_capacity(2);
            let mut tail = Vec::with_capacity(2);
            if own.owner.is_some() {
                prefix.push(StdField::OwnerId);
                tail.push(node.owner.clone().ok_or_else(|| {
                    internal("an item of a subordinate catalog without an owner")
                })?);
            }
            prefix.push(StdField::ParentId);
            tail.push(FieldValue::Ref {
                table: own.number,
                id: node.r.id,
            });
            out.extend(self.scan(ex, &own, &prefix, tail)?);
        }
        for sub in subordinates(app, node.r.obj) {
            let layout = Layout::of(app, sub)?;
            let mut prefix = vec![StdField::OwnerId];
            let mut tail = vec![FieldValue::Ref {
                table: own.number,
                id: node.r.id,
            }];
            if app.model().obj(sub).behavior.hierarchical && layout.parent.is_some() {
                prefix.push(StdField::ParentId);
                tail.push(FieldValue::Ref {
                    table: layout.number,
                    id: RefId::NIL,
                });
            }
            out.extend(self.scan(ex, &layout, &prefix, tail)?);
        }
        Ok(out)
    }

    fn push_below(&self, ex: &mut Execution, node: &Node, stack: &mut Vec<Node>) -> CallResult<()> {
        let below = self.below(ex, node)?;
        stack.reserve(below.len());
        crate::read_budget::reserve(ex, crate::read_budget::slots::<Node>(stack.capacity()))?;
        stack.extend(below.into_iter().rev());
        Ok(())
    }

    fn has_records(
        &self,
        ex: &mut Execution,
        recorded: &[(ObjId, TableId, IndexId)],
        value: &FieldValue,
    ) -> CallResult<bool> {
        let txn = self.txn.clone();
        let mut view = read_view(ex, Some(&*txn), self.app)?;
        let _memory = open_read(ex, &mut view)?;
        let key = [value.clone()];
        for (_, table, index) in recorded {
            let found = view
                .txn()
                .range(
                    *table,
                    *index,
                    Bound::Included(key.as_slice()),
                    Bound::Included(key.as_slice()),
                    Direction::Forward,
                )
                .and_then(|mut rows| rows.next().transpose())
                .map_err(|e| store_call_error(ex, e))?;
            if found.is_some() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn remove(
        &mut self,
        ex: &mut Execution,
        r: RefValue,
        version: Option<&str>,
    ) -> CallResult<Option<Node>> {
        let app = self.app;
        let layout = Layout::of(app, r.obj)?;
        let st = app
            .model()
            .storage_of(r.obj)
            .ok_or_else(|| internal("the object has no storage"))?;
        let masters = self.masters(r.obj)?;
        if data_lock::lock_for_write(app, &self.txn, layout.def).is_err() {
            return Err(data_lock::conflict().into());
        }
        for m in masters.iter() {
            if data_lock::lock_for_write(app, &self.txn, app.model().obj(m.reg)).is_err() {
                return Err(data_lock::conflict().into());
            }
        }
        let key = layout.id_key(r.id)?;
        let map = app.store().storage();
        let mut sections = Vec::with_capacity(st.tabular.len());
        for (table, _) in &st.tabular {
            let ti = map
                .try_table(*table)
                .ok_or_else(|| internal("no table of the section"))?;
            sections.push((*table, id_key(app, ti, layout.number, r.id)?));
        }
        let value = FieldValue::Ref {
            table: layout.number,
            id: r.id,
        };
        let mut probes = Vec::with_capacity(masters.len());
        for m in masters.iter() {
            probes.push(probe(app, m, &value)?);
        }
        let target = (layout.number, r.id);
        let (params, auto_before) = match self.top.take() {
            Some(top) => top,
            None => {
                let p = ExchangeParams::new()?;
                let auto = p.auto_fill();
                (p, auto)
            }
        };
        let registration =
            crate::changes::Registration::for_write(app, r.obj, &params, auto_before)?;
        let change_key = match &registration {
            Some(reg) => reg.changes.key_ref(layout.number, r.id)?,
            None => Vec::new(),
        };
        let mut registered = Vec::new();
        let (owner, marked) = (
            layout.column(StdField::OwnerId),
            layout.column(StdField::Marked),
        );
        let mut taken = 0u64;
        let document = layout.def.kind == ObjKind::Document;
        if document {
            app.numerator().note_deletion();
        }
        let journals = if document {
            Some(crate::journal::Journals::of(app, r.obj)?)
        } else {
            None
        };
        let recorded = if document {
            recorders(app, layout.def)?
        } else {
            Vec::new()
        };
        let queue = crate::history::Queue::for_write(ex, app, &self.txn, r.obj)?;
        let mut posted = std::mem::take(&mut self.posted);
        if let Unrecord::Deny(refusal) = std::mem::replace(&mut self.unrecord, Unrecord::No)
            && self.has_records(ex, &recorded, &value)?
        {
            if let Some(e) = refusal {
                return Err(e);
            }
            posted = true;
        }
        if posted {
            let regs: Vec<ObjId> = recorded.iter().map(|(reg, _, _)| *reg).collect();
            for reg in regs {
                crate::regread::remove_records(ex, self.app, &self.txn, reg, r)?;
            }
        }
        let mut history_master = None;
        let outcome = self
            .txn
            .with_write(&**app, |w: &mut dyn WriteTxn| -> StoreResult<Removed> {
                let found = {
                    let mut rows = w.range(
                        layout.table,
                        IndexId::Id,
                        Bound::Included(key.as_slice()),
                        Bound::Included(key.as_slice()),
                        Direction::Forward,
                    )?;
                    rows.next().transpose()?
                };
                let Some((handle, row)) = found else {
                    return Ok(Removed::Missing);
                };
                if let Some(v) = version
                    && crate::write::row_version(&row).as_deref() != Some(v)
                {
                    return Ok(Removed::Stale);
                }
                let cell = |f: Option<FieldId>| f.and_then(|f| row.get(usize::from(f.0))).cloned();
                let node = Node {
                    r,
                    marked: matches!(cell(marked), Some(FieldValue::Bool(true))),
                    owner: cell(owner),
                };
                let recorder = [value.clone()];
                for (reg, table, index) in &recorded {
                    let mut rows = w.range(
                        *table,
                        *index,
                        Bound::Included(recorder.as_slice()),
                        Bound::Included(recorder.as_slice()),
                        Direction::Forward,
                    )?;
                    if rows.next().transpose()?.is_some() {
                        return Ok(Removed::Recorded(*reg));
                    }
                }
                w.delete(layout.table, handle)?;
                if let Some(j) = &journals {
                    j.delete(w, r.id)?;
                }
                if let Some(q) = &queue {
                    q.object(w, r.id, None, false)?;
                }
                for (table, key) in &sections {
                    let handles = handles_of(&*w, *table, key)?;
                    taken = taken.saturating_add(handles.len() as u64);
                    for h in handles {
                        w.delete(*table, h)?;
                    }
                }
                for (m, p) in masters.iter().zip(&probes) {
                    let handles = matching(&*w, m, p, target)?;
                    if handles.is_empty() {
                        continue;
                    }
                    if m.guarded {
                        return Ok(Removed::Guarded(m.reg));
                    }
                    if crate::history::kept(app, m.reg) {
                        history_master = Some(m.reg);
                    }
                    taken = taken.saturating_add(handles.len() as u64);
                    for h in handles {
                        w.delete(m.table, h)?;
                    }
                }
                if let Some(reg) = &registration {
                    registered = reg.nodes(&*w)?;
                    reg.apply(w, &registered, &change_key)?;
                }
                Ok(Removed::Done(node))
            });
        ex.charge(taken)?;
        match outcome.map_err(|e| e.call_error(ex))? {
            Removed::Done(node) => {
                if let Some(reg) = &registration {
                    reg.done(&registered);
                }
                if let Some(q) = queue {
                    q.finish(&self.txn);
                }
                if let Some(reg) = history_master {
                    self.txn
                        .refuse_commit(crate::history::refused_deletion(app, reg));
                }
                crate::commit_guard::guard(app, &self.txn, r.obj);
                Ok(Some(node))
            }
            Removed::Missing => Ok(None),
            Removed::Stale => {
                Err(refuse(self.def, self.op, "версия объекта не та, что в базе").into())
            }
            Removed::Recorded(reg) => Err(refuse(
                self.def,
                self.op,
                &format!(
                    "документ с движениями регистра {}",
                    app.model().obj(reg).name
                ),
            )
            .into()),
            Removed::Guarded(reg) => {
                let reg = app.model().obj(reg);
                Err(refuse(
                    self.def,
                    self.op,
                    &format!(
                        "записи регистра сведений {} с ведущим измерением и обработчиками набора записей",
                        reg.name
                    ),
                )
                .into())
            }
        }
    }

    fn delete(
        &mut self,
        ex: &mut Execution,
        inst: &Rc<ObjectInstance>,
        r: RefValue,
        version: &str,
        loading: bool,
    ) -> CallResult<()> {
        events::before_delete(ex, inst)?;
        let Some(top) = self.remove(ex, r, Some(version))? else {
            return Err(refuse(self.def, self.op, "записи объекта нет в базе").into());
        };
        if loading {
            return Ok(());
        }
        let mut stack = Vec::new();
        self.push_below(ex, &top, &mut stack)?;
        while let Some(next) = stack.pop() {
            ex.charge(1)?;
            let Some(child) = stored_instance(ex, self.app, next.r)? else {
                continue;
            };
            events::before_delete(ex, &child)?;
            let blank = read_fresh(
                ex,
                self.app,
                RefValue {
                    obj: next.r.obj,
                    id: RefId::NIL,
                },
            )?;
            let Some(node) = self.remove(ex, next.r, None)? else {
                continue;
            };
            child.forget(blank);
            self.push_below(ex, &node, &mut stack)?;
        }
        Ok(())
    }

    fn mark_below(&mut self, ex: &mut Execution, top: &Node, mark: bool) -> CallResult<()> {
        let mut stack = Vec::new();
        self.push_below(ex, top, &mut stack)?;
        while let Some(node) = stack.pop() {
            ex.charge(1)?;
            if node.marked != mark {
                let Some(child) = stored_instance(ex, self.app, node.r)? else {
                    continue;
                };
                let i = mark_field(self.app.model().obj(node.r.obj))?;
                child.assign(ex, i, Value::Bool(mark))?;
                crate::write::write(ex, &child)?;
            }
            self.push_below(ex, &node, &mut stack)?;
        }
        Ok(())
    }
}

pub(crate) fn delete(ex: &mut Execution, inst: &ObjectInstance) -> CallResult<Value> {
    delete_with(ex, inst, false, Unrecord::No)
}

fn delete_with(
    ex: &mut Execution,
    inst: &ObjectInstance,
    posted: bool,
    unrecord: Unrecord,
) -> CallResult<Value> {
    let inst = inst.rc()?;
    let app = inst.app.clone();
    let def = app.model().obj(inst.obj);
    let op = Op::Delete;
    let r = {
        let st = inst.state.borrow();
        if st.is_new {
            return Err(not_selected().into());
        }
        st.r.ok_or_else(|| internal("a stored object without a reference"))?
    };
    admit(ex, &app, def, op, &inst)?;
    let loading = inst.loading();
    let objs = if loading {
        vec![inst.obj]
    } else {
        reachable(&app, inst.obj)
    };
    let params = inst.exchange()?;
    let auto_before = params.auto_fill();
    let txn = CallTxn::of_call(ex).ok_or_else(|| internal("a deletion outside a call"))?;
    txn.check().map_err(|e| e.host())?;
    ensure_predefined(&app, &objs)?;
    let version = inst.state.borrow().data_version.clone();
    let blank = read_fresh(
        ex,
        &app,
        RefValue {
            obj: inst.obj,
            id: RefId::NIL,
        },
    )?;
    let mut walk = Walk {
        app: &app,
        txn: txn.clone(),
        op,
        def,
        masters: HashMap::new(),
        top: Some((params, auto_before)),
        posted,
        unrecord,
    };
    txn.begin();
    let ended = match walk.delete(ex, &inst, r, &version, loading) {
        Ok(()) => crate::fo_version::commit(ex, &app, &txn),
        Err(e) => {
            let _ = txn.rollback(&*app);
            Err(e)
        }
    };
    if matches!(ended?, Commit::Committed | Commit::Inner) {
        inst.forget(blank);
    }
    Ok(Value::Undefined)
}

pub(crate) fn set_mark(
    ex: &mut Execution,
    inst: &ObjectInstance,
    args: &[Arg],
) -> CallResult<Value> {
    let inst = inst.rc()?;
    let app = inst.app.clone();
    let def = app.model().obj(inst.obj);
    let mark = match arg_value(ex, args, 0)? {
        Value::Bool(b) => b,
        _ => {
            return Err(refuse(def, Op::Mark(true), "пометка не Булево").into());
        }
    };
    let op = Op::Mark(mark);
    let cascade = match arg_value(ex, args, 1)? {
        Value::Undefined => true,
        Value::Bool(b) => b,
        _ => return Err(refuse(def, op, "ВключаяПодчиненные не Булево").into()),
    };
    let (is_new, modified, r) = {
        let st = inst.state.borrow();
        (st.is_new, st.modified.get(), st.r)
    };
    if is_new {
        return Err(not_selected().into());
    }
    if modified {
        return Err(refuse(def, op, "объект модифицирован").into());
    }
    let r = r.ok_or_else(|| internal("a stored object without a reference"))?;
    let i = mark_field(def)?;
    let current = matches!(inst.attr(ex, i)?, Value::Bool(true));
    let walk_below = cascade && !inst.loading();
    if current == mark && !walk_below {
        return Ok(Value::Undefined);
    }
    admit(ex, &app, def, op, &inst)?;
    let txn = CallTxn::of_call(ex).ok_or_else(|| internal("a mark outside a call"))?;
    txn.check().map_err(|e| e.host())?;
    if walk_below {
        ensure_predefined(&app, &reachable(&app, inst.obj))?;
    }
    let owner = match def
        .fields
        .iter()
        .position(|f| f.std == Some(StdAttr::Owner))
    {
        Some(oi) => {
            let field = def
                .fields
                .get(oi)
                .ok_or_else(|| internal("no owner field"))?;
            Some(to_field(&app, field, &inst.attr(ex, oi)?)?)
        }
        None => None,
    };
    let mut walk = Walk {
        app: &app,
        txn: txn.clone(),
        op,
        def,
        masters: HashMap::new(),
        top: None,
        posted: false,
        unrecord: Unrecord::No,
    };
    txn.begin();
    let mut assigned = None;
    let mut result = Ok(());
    if current != mark {
        result = inst.assign(ex, i, Value::Bool(mark));
        if result.is_ok() {
            assigned = Some(inst.snapshot());
            result = crate::write::write(ex, &inst);
        }
    }
    if result.is_ok() && walk_below {
        let top = Node {
            r,
            marked: mark,
            owner,
        };
        result = walk.mark_below(ex, &top, mark);
    }
    let ended = match result {
        Ok(()) => crate::fo_version::commit(ex, &app, &txn),
        Err(e) => {
            let _ = txn.rollback(&*app);
            Err(e)
        }
    };
    if !matches!(ended, Ok(Commit::Committed | Commit::Inner))
        && let Some(s) = assigned
    {
        inst.restore(s);
    }
    ended.map(|_| Value::Undefined)
}

pub(crate) fn set_mark_document(
    ex: &mut Execution,
    inst: &ObjectInstance,
    args: &[Arg],
) -> CallResult<Value> {
    use crate::write_hooks::{DocWrite, DocumentModes};
    inst.live()?;
    let inst = inst.rc()?;
    let app = inst.app.clone();
    let def = app.model().obj(inst.obj);
    let mark = match arg_value(ex, args, 0)? {
        Value::Bool(b) => b,
        _ => return Err(refuse(def, Op::Mark(true), "пометка не Булево").into()),
    };
    let op = Op::Mark(mark);
    if inst.state.borrow().is_new {
        return Err(refuse(def, op, "новый документ").into());
    }
    if inst.loading() {
        return Err(refuse(def, op, "ОбменДанными.Загрузка документа").into());
    }
    if inst.state.borrow().modified.get() {
        return Err(refuse(def, op, "объект модифицирован").into());
    }
    let flag = |ex: &mut Execution, std: StdAttr| -> CallResult<bool> {
        match def.fields.iter().position(|f| f.std == Some(std)) {
            Some(i) => Ok(matches!(inst.attr(ex, i)?, Value::Bool(true))),
            None => Ok(false),
        }
    };
    let posted = flag(ex, StdAttr::Posted)?;
    let marked = flag(ex, StdAttr::DeletionMark)?;
    let txn = CallTxn::of_call(ex).ok_or_else(|| internal("a mark outside a call"))?;
    let written = inst.written_in(&txn);
    let write = match (mark, posted, marked) {
        (true, true, false) => DocWrite::Mark(true),
        (false, false, true) => DocWrite::Mark(false),
        (true, false, false) if def.behavior.posting == Posting::Deny => DocWrite::Mark(false),
        (true, false, false) => {
            return Err(refuse(def, op, "пометка непроведённого документа").into());
        }
        (true, false, true) if !written => return Ok(Value::Undefined),
        (true, false, true) => return Err(refuse(def, op, "повторная пометка документа").into()),
        (true, true, true) => {
            return Err(refuse(def, op, "повторная пометка проведённого документа").into());
        }
        (false, true, _) => {
            return Err(refuse(def, op, "снятие пометки проведённого документа").into());
        }
        (false, false, false) => {
            return Err(refuse(def, op, "снятие пометки непомеченного документа").into());
        }
    };
    let method = op.method();
    if write == DocWrite::Mark(true) {
        crate::posting::undo_check(&inst, method, events::Event::OnWrite)?;
        crate::posting::before_write_check(&inst, method, "пометке удаления")?;
    } else {
        if events::handled(&inst, events::Event::OnWrite) {
            return Err(refuse(def, op, "обработчик ПриЗаписи").into());
        }
        crate::posting::before_write_check(&inst, method, "пометке удаления")?;
        crate::posting::untouched_sets(
            &inst,
            if mark {
                "до пометки"
            } else {
                "до снятия пометки"
            },
        )?;
    }
    if written && !(posted && write == DocWrite::Mark(true)) {
        return Err(refuse(def, op, "документ, записанный в этой транзакции").into());
    }
    let i = mark_field(def)?;
    let before = inst.snapshot();
    let result = inst.assign(ex, i, Value::Bool(mark)).and_then(|()| {
        let modes = DocumentModes::undoing(write)?;
        crate::write::write_with(ex, &inst, Some(modes))
    });
    if result.is_err() {
        inst.restore(before);
    }
    result.map(|()| Value::Undefined)
}

fn refuse_list(what: &str) -> HostError {
    HostError::not_implemented("Глобальный контекст", &format!("УдалитьОбъекты: {what}"))
}

fn not_exclusive() -> HostError {
    let mut e = match crate::errors::msg(crate::errors::keys::NOT_EXCLUSIVE_MODE) {
        Some(m) => HostError::text(m.ru, m.en),
        None => internal(crate::errors::keys::NOT_EXCLUSIVE_MODE),
    };
    e.categories = Some(bsl_vm::CategorySet::of(&[
        bsl_vm::Category::AllErrors,
        bsl_vm::Category::ScriptRuntimeError,
        bsl_vm::Category::ScriptUseError,
    ]));
    e
}

fn delete_objects(
    app: &Arc<Application>,
    ex: &mut Execution,
    args: &mut [Arg],
) -> CallResult<Value> {
    let list = arg_value(ex, args, 0)?;
    let check = match arg_value(ex, args, 1)? {
        Value::Undefined => false,
        Value::Bool(b) => b,
        _ => return Err(refuse_list("Проверять не Булево").into()),
    };
    if check {
        if CallTxn::of_call(ex).is_some_and(|t| t.active()) {
            return Err(not_exclusive().into());
        }
        return Err(refuse_list("контроль ссылок вне транзакции").into());
    }
    for i in 3..6 {
        if !matches!(arg_value(ex, args, i)?, Value::Undefined) {
            return Err(refuse_list("область поиска").into());
        }
    }
    let items =
        bsl_stdlib::array_items(&list).ok_or_else(|| refuse_list("СписокСсылок не Массив"))?;
    let mut refs = Vec::with_capacity(items.len());
    for item in &items {
        ex.charge(1)?;
        let r = crate::values::refs::as_ref(item)
            .filter(|r| !r.is_empty())
            .filter(|r| app.model().obj(r.obj).kind == ObjKind::Catalog)
            .ok_or_else(|| refuse_list("элемент списка не ссылка справочника"))?;
        refs.push(r);
    }
    let txn = CallTxn::of_call(ex).ok_or_else(|| internal("a deletion outside a call"))?;
    if !txn.active() {
        return Err(refuse_list("без контроля ссылок вне транзакции").into());
    }
    txn.check().map_err(|e| e.host())?;
    txn.begin();
    let mut result = Ok(());
    for r in refs {
        result = match stored_instance(ex, app, r) {
            Ok(Some(inst)) => delete(ex, &inst).map(|_| ()),
            Ok(None) => Err(refuse_list("объекта нет в базе").into()),
            Err(e) => Err(e),
        };
        if result.is_err() {
            break;
        }
    }
    match result {
        Ok(()) => {
            crate::fo_version::commit(ex, app, &txn)?;
            Ok(Value::Undefined)
        }
        Err(e) => {
            let _ = txn.rollback(&**app);
            Err(e)
        }
    }
}

pub(crate) fn register(app: &Arc<Application>, reg: &mut bsl_vm::Registry) {
    let app = app.clone();
    reg.global_fn(
        "УдалитьОбъекты",
        "DeleteObjects",
        Arc::new(move |ex, args| delete_objects(&app, ex, args)),
    );
}
