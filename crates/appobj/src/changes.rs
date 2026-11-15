use crate::Application;
use crate::exchange_params::ExchangeParams;
use crate::model::{ObjId, ObjKind};
use crate::txn::CallTxn;
use bsl_vm::{Arg, CallError, CallResult, Execution, HostError, Value};
use ibschema::Row;
use ibschema::fields::FieldValue;
use ibschema::names::{StdField, role};
use ibschema::storage::{FieldId, IndexId, TableId, TableInfo};
use ibschema::store::{Direction, ReadTxn, StoreResult, WriteTxn};
use ibschema::values::RefId;
use std::ops::Bound;
use std::sync::Arc;

fn internal(text: &str) -> HostError {
    HostError::text(&format!("internal: {text}"), &format!("internal: {text}"))
}

pub(crate) fn refusal(what: &str) -> HostError {
    HostError::not_implemented("Регистрация изменений", what)
}

pub(crate) fn plans_of(app: &Application, obj: ObjId) -> Vec<(ObjId, bool)> {
    let model = app.model();
    let md = model.obj(obj).md;
    model
        .objects
        .iter()
        .filter(|o| o.kind == ObjKind::ExchangePlan)
        .filter_map(|plan| {
            let items = model.config.object(plan.md)?.exchange_content()?;
            let item = items.iter().find(|i| i.object.id == Some(md))?;
            Some((plan.id, item.auto_record))
        })
        .collect()
}

pub(crate) fn auto_plans(app: &Application, obj: ObjId) -> Vec<ObjId> {
    plans_of(app, obj)
        .into_iter()
        .filter_map(|(p, auto)| auto.then_some(p))
        .collect()
}

fn is_this_node(cell: Option<&FieldValue>) -> bool {
    matches!(cell, Some(FieldValue::Uuid(u)) if !u.is_nil())
}

struct NodeTable {
    table: TableId,
    number: u32,
    id: usize,
    predefined: usize,
}

impl NodeTable {
    fn of(app: &Application, plan: ObjId) -> Result<NodeTable, HostError> {
        let st = app
            .model()
            .storage_of(plan)
            .ok_or_else(|| internal("an exchange plan without storage"))?;
        let number = st
            .ref_number
            .ok_or_else(|| internal("an exchange plan without a reference table"))?;
        let ti = app
            .store()
            .storage()
            .try_table(st.main)
            .ok_or_else(|| internal("no table of the nodes"))?;
        if !ti.separators.is_empty() {
            return Err(refusal("разделённый план обмена"));
        }
        let col = |f: StdField| {
            ti.field_by_std(f)
                .map(|id| usize::from(id.0))
                .ok_or_else(|| internal("a column of the table of the nodes"))
        };
        Ok(NodeTable {
            table: st.main,
            number,
            id: col(StdField::Id)?,
            predefined: col(StdField::PredefinedId)?,
        })
    }

    fn nodes(&self, r: &dyn ReadTxn) -> StoreResult<Vec<(u32, RefId)>> {
        let mut out = Vec::new();
        for item in r.scan(self.table)? {
            let (_, row) = item?;
            if is_this_node(row.get(self.predefined)) {
                continue;
            }
            if let Some(FieldValue::Ref { id, .. }) = row.get(self.id) {
                out.push((self.number, *id));
            }
        }
        Ok(out)
    }
}

pub(crate) struct ChangeTable<'a> {
    table: TableId,
    ti: &'a TableInfo,
    node: usize,
    msg: usize,
    by_key: IndexId,
    key_fields: &'a [FieldId],
}

impl<'a> ChangeTable<'a> {
    pub(crate) fn of(
        app: &'a Application,
        obj: ObjId,
    ) -> Result<Option<ChangeTable<'a>>, HostError> {
        let Some(st) = app.model().storage_of(obj) else {
            return Ok(None);
        };
        let Some(table) = st
            .service
            .iter()
            .find(|(p, _)| p == role::CHANGE_RECORDS || p == role::CONSTANT_CHANGE_RECORDS)
            .map(|(_, t)| *t)
        else {
            return Ok(None);
        };
        let ti = app
            .store()
            .storage()
            .try_table(table)
            .ok_or_else(|| internal("no table of changes"))?;
        if !ti.separators.is_empty() {
            return Err(refusal("разделённая таблица изменений"));
        }
        let col = |f: StdField| {
            ti.field_by_std(f)
                .ok_or_else(|| internal("a column of the table of changes"))
        };
        let node = col(StdField::Node)?;
        let msg = col(StdField::MessageNo)?;
        let (pos, ix) = ti
            .indexes
            .iter()
            .enumerate()
            .find(|(_, ix)| ix.unique && ix.prefix == 0 && ix.fields.first() != Some(&node))
            .ok_or_else(|| refusal("таблица изменений без ключа данных"))?;
        Ok(Some(ChangeTable {
            table,
            ti,
            node: usize::from(node.0),
            msg: usize::from(msg.0),
            by_key: IndexId::Named(u16::try_from(pos).map_err(|_| internal("index number"))?),
            key_fields: &ix.fields,
        }))
    }

    fn key_columns(&self) -> impl Iterator<Item = (usize, &'a ibschema::storage::FieldInfo)> + '_ {
        self.ti
            .fields
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != self.node && *i != self.msg)
    }

    pub(crate) fn key_ref(
        &self,
        number: u32,
        id: RefId,
    ) -> Result<Vec<(usize, FieldValue)>, HostError> {
        let mut key = Vec::new();
        for (i, f) in self.key_columns() {
            if f.std != Some(StdField::Id) {
                return Err(internal("a key of changes that is not a reference"));
            }
            key.push((i, FieldValue::Ref { table: number, id }));
        }
        Ok(key)
    }

    pub(crate) fn key_constant(
        &self,
        md: crate::model::Uuid16,
    ) -> Result<Vec<(usize, FieldValue)>, HostError> {
        let mut key = Vec::new();
        for (i, f) in self.key_columns() {
            if f.std != Some(StdField::ConstId) {
                return Err(internal("a key of changes that is not a constant"));
            }
            key.push((i, FieldValue::Uuid(md)));
        }
        Ok(key)
    }

    pub(crate) fn key_of_row(
        &self,
        main: &TableInfo,
        row: &Row,
    ) -> Result<Vec<(usize, FieldValue)>, HostError> {
        let mut key = Vec::new();
        for (i, f) in self.key_columns() {
            let src =
                source_of(main, f).ok_or_else(|| refusal(&format!("поле ключа {}", f.name)))?;
            let v = row
                .get(usize::from(src.0))
                .cloned()
                .ok_or_else(|| internal("a short row"))?;
            key.push((i, v));
        }
        Ok(key)
    }

    pub(crate) fn key_sources(&self, main: &TableInfo) -> Result<Vec<FieldId>, HostError> {
        self.key_columns()
            .map(|(_, f)| {
                source_of(main, f).ok_or_else(|| refusal(&format!("поле ключа {}", f.name)))
            })
            .collect()
    }

    fn row(&self, node: (u32, RefId), key: &[(usize, FieldValue)]) -> Row {
        let mut row: Row = vec![FieldValue::Null; self.ti.fields.len()];
        if let Some(slot) = row.get_mut(self.node) {
            *slot = FieldValue::Ref {
                table: node.0,
                id: node.1,
            };
        }
        for (i, v) in key {
            if let Some(slot) = row.get_mut(*i) {
                *slot = v.clone();
            }
        }
        row
    }

    fn index_key(&self, row: &Row) -> Vec<FieldValue> {
        self.key_fields
            .iter()
            .map(|f| {
                row.get(usize::from(f.0))
                    .cloned()
                    .unwrap_or(FieldValue::Null)
            })
            .collect()
    }

    pub(crate) fn register(
        &self,
        w: &mut dyn WriteTxn,
        node: (u32, RefId),
        key: &[(usize, FieldValue)],
    ) -> StoreResult<()> {
        let row = self.row(node, key);
        let ikey = self.index_key(&row);
        let found = {
            let mut rows = w.range(
                self.table,
                self.by_key,
                Bound::Included(ikey.as_slice()),
                Bound::Included(ikey.as_slice()),
                Direction::Forward,
            )?;
            rows.next().transpose()?
        };
        match found {
            Some((h, mut old)) => {
                if let Some(slot) = old.get_mut(self.msg)
                    && *slot != FieldValue::Null
                {
                    *slot = FieldValue::Null;
                    w.update(self.table, h, &old)?;
                }
                Ok(())
            }
            None => w.insert(self.table, &row).map(drop),
        }
    }

    pub(crate) fn unregister(
        &self,
        w: &mut dyn WriteTxn,
        node: (u32, RefId),
        key: &[(usize, FieldValue)],
    ) -> StoreResult<bool> {
        let row = self.row(node, key);
        let ikey = self.index_key(&row);
        let found = {
            let mut rows = w.range(
                self.table,
                self.by_key,
                Bound::Included(ikey.as_slice()),
                Bound::Included(ikey.as_slice()),
                Direction::Forward,
            )?;
            rows.next().transpose()?
        };
        match found {
            Some((h, _)) => {
                w.delete(self.table, h)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }
}

fn source_of(main: &TableInfo, f: &ibschema::storage::FieldInfo) -> Option<FieldId> {
    match (f.std, f.md) {
        (Some(StdField::Period), _) => main.field_by_std(StdField::Period),
        (None, Some(md)) => main
            .fields
            .iter()
            .position(|m| m.md == Some(md) && !m.implicit && m.std.is_none())
            .and_then(|p| u16::try_from(p).ok())
            .map(FieldId),
        _ => None,
    }
}

enum Recipients {
    Auto {
        plans: Vec<NodeTable>,
        sender: Option<(u32, RefId)>,
    },
    Listed(Vec<(u32, RefId)>),
}

pub(crate) struct Registration<'a> {
    pub(crate) changes: ChangeTable<'a>,
    recipients: Recipients,
    params: std::rc::Rc<ExchangeParams>,
}

impl<'a> Registration<'a> {
    pub(crate) fn for_write(
        app: &'a Application,
        obj: ObjId,
        params: &std::rc::Rc<ExchangeParams>,
        auto_before: bool,
    ) -> Result<Option<Registration<'a>>, HostError> {
        if auto_before != params.auto_fill() {
            return Err(refusal("Получатели.АвтоЗаполнение изменено в обработчике"));
        }
        let sender = match params.sender_ref() {
            None => None,
            Some(r) => Some((
                app.model()
                    .storage_of(r.obj)
                    .and_then(|s| s.ref_number)
                    .ok_or_else(|| internal("a sender without a reference table"))?,
                r.id,
            )),
        };
        let recipients = if params.auto_fill() {
            let plans = auto_plans(app, obj);
            if plans.is_empty() {
                return Ok(None);
            }
            Recipients::Auto {
                plans: plans
                    .into_iter()
                    .map(|p| NodeTable::of(app, p))
                    .collect::<Result<_, _>>()?,
                sender,
            }
        } else {
            let kept: Vec<(u32, RefId)> = params
                .kept()
                .into_iter()
                .filter(|n| Some(*n) != sender)
                .collect();
            if kept.is_empty() {
                return Ok(None);
            }
            Recipients::Listed(kept)
        };
        let Some(changes) = ChangeTable::of(app, obj)? else {
            return Err(refusal("получатели у объекта вне состава планов обмена"));
        };
        Ok(Some(Registration {
            changes,
            recipients,
            params: params.clone(),
        }))
    }

    pub(crate) fn nodes(&self, r: &dyn ReadTxn) -> StoreResult<Vec<(u32, RefId)>> {
        match &self.recipients {
            Recipients::Listed(nodes) => Ok(nodes.clone()),
            Recipients::Auto { plans, sender } => {
                let mut out = Vec::new();
                for p in plans {
                    out.extend(p.nodes(r)?.into_iter().filter(|n| Some(*n) != *sender));
                }
                Ok(out)
            }
        }
    }

    pub(crate) fn apply(
        &self,
        w: &mut dyn WriteTxn,
        nodes: &[(u32, RefId)],
        key: &[(usize, FieldValue)],
    ) -> StoreResult<()> {
        for node in nodes {
            self.changes.register(w, *node, key)?;
        }
        Ok(())
    }

    pub(crate) fn done(&self, nodes: &[(u32, RefId)]) {
        if matches!(self.recipients, Recipients::Auto { .. }) {
            self.params.keep(nodes.to_vec());
        }
    }
}

fn node_arg(
    ex: &mut Execution,
    app: &Application,
    method: &str,
    v: &Value,
) -> CallResult<(ObjId, (u32, RefId))> {
    if bsl_stdlib::array_items(v).is_some() {
        return Err(refusal(&format!("{method}: массив узлов")).into());
    }
    let Some(r) = crate::values::refs::as_ref(v) else {
        return Err(crate::meta::type_mismatch(1).into());
    };
    if app.model().obj(r.obj).kind != ObjKind::ExchangePlan {
        return Err(crate::meta::type_mismatch(1).into());
    }
    if r.id.is_nil() {
        return Err(crate::select::value_mismatch(1).into());
    }
    let nodes = NodeTable::of(app, r.obj)?;
    let txn = CallTxn::of_call(ex);
    let this = {
        let mut view = crate::read::read_view(ex, txn.as_deref(), app)?;
        let rd = view.txn();
        let key = [FieldValue::Ref {
            table: nodes.number,
            id: r.id,
        }];
        let found = rd
            .range(
                nodes.table,
                IndexId::Id,
                Bound::Included(&key[..]),
                Bound::Included(&key[..]),
                Direction::Forward,
            )
            .and_then(|mut it| it.next().transpose())
            .map_err(|e| crate::read::store_call_error(ex, e))?;
        match found {
            None => return Err(refusal(&format!("{method}: узел, которого нет в базе")).into()),
            Some((_, row)) => is_this_node(row.get(nodes.predefined)),
        }
    };
    if this {
        return Err(crate::select::value_mismatch(1).into());
    }
    Ok((r.obj, (nodes.number, r.id)))
}

fn data_arg(
    app: &Application,
    method: &str,
    plan: ObjId,
    v: &Value,
) -> Result<(ObjId, Key), CallError> {
    let in_content = |obj: ObjId| plans_of(app, obj).iter().any(|(p, _)| *p == plan);
    if let Some(r) = crate::values::refs::as_ref(v) {
        let kind = app.model().obj(r.obj).kind;
        if kind != ObjKind::Catalog {
            return Err(refusal(&format!(
                "{method}: ссылка вида {}",
                kind.name(platform_catalog::messages::Lang::Ru)
            ))
            .into());
        }
        if r.id.is_nil() || !in_content(r.obj) {
            return Err(crate::select::value_mismatch(2).into());
        }
        let number = app
            .model()
            .storage_of(r.obj)
            .and_then(|s| s.ref_number)
            .ok_or_else(|| internal("a catalog without a reference table"))?;
        return Ok((r.obj, Key::Ref(number, r.id)));
    }
    if let Some(crate::meta::MdKey::Path(crate::model::MdPath::Object(obj))) =
        crate::meta::key_of(v)
        && app.model().obj(obj).kind == ObjKind::Constant
    {
        if !in_content(obj) {
            return Err(crate::select::value_mismatch(2).into());
        }
        return Ok((obj, Key::Constant(app.model().obj(obj).md)));
    }
    Err(refusal(&format!("{method}: данные этого вида")).into())
}

enum Key {
    Ref(u32, RefId),
    Constant(crate::model::Uuid16),
}

pub(crate) fn manager_call(
    ex: &mut Execution,
    app: &Arc<Application>,
    register: bool,
    args: &[Arg],
) -> CallResult<Value> {
    let method = if register {
        "ЗарегистрироватьИзменения"
    } else {
        "УдалитьРегистрациюИзменений"
    };
    if app.options().read_only_scope.is_some() {
        return Err(refusal(&format!(
            "{method}: приложение только для чтения (read_scope)"
        ))
        .into());
    }
    if !crate::session::call_ctx(ex).is_some_and(|c| c.sess.user().no_users) {
        return Err(HostError::not_implemented("P6", "application write security provider").into());
    }
    let node_v = crate::meta::arg_value(ex, args, 0)?;
    let (plan, node) = node_arg(ex, app, method, &node_v)?;
    let data = crate::meta::arg_value(ex, args, 1)?;
    if matches!(data, Value::Undefined) {
        return Err(refusal(&format!("{method}: без данных")).into());
    }
    if !register && matches!(data, Value::Number(_)) {
        return Err(refusal(&format!("{method}: номер сообщения")).into());
    }
    let (obj, key) = data_arg(app, method, plan, &data)?;
    let changes = ChangeTable::of(app, obj)?
        .ok_or_else(|| internal("an object of the content of a plan without a table of changes"))?;
    let key = match key {
        Key::Ref(number, id) => changes.key_ref(number, id)?,
        Key::Constant(md) => changes.key_constant(md)?,
    };
    let txn = CallTxn::of_call(ex).ok_or_else(|| internal("a registration outside a call"))?;
    txn.check().map_err(|e| e.host())?;
    txn.begin();
    let done = txn.with_write(&**app, |w| {
        if register {
            changes.register(w, node, &key)
        } else {
            changes.unregister(w, node, &key).map(drop)
        }
    });
    let ended = match done {
        Ok(()) => txn.commit(&**app).map_err(|e| e.call_error(ex)),
        Err(e) => {
            let _ = txn.rollback(&**app);
            Err(e.call_error(ex))
        }
    };
    ended.map(|_| Value::Undefined)
}
