use crate::Application;
use crate::errors::{self, keys};
use crate::locks::{Conflict, LockControl, LockMode};
use crate::meta::{arg_value, member_name, member_of, type_mismatch};
use crate::model::{AppModel, InfoRegWriteMode, ObjDef, ObjKind};
use crate::read_budget::slots;
use crate::txn::{CallTxn, TxnError};
use bsl_prim::fold_name;
use bsl_vm::{
    Arg, CallResult, Exception, Execution, HostError, HostObject, MemberId, Registry,
    RetainedMemory, TypeId, TypeIdx, TypeImpl, Value,
};
use ibschema::storage::TableId;
use ibschema::store::StoreError;
use md_model::DataLockControlMode;
use platform_catalog::{Catalog, catalog, messages::Lang};
use std::any::Any;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

const LOCK: &str = "БлокировкаДанных";
const ITEM: &str = "ЭлементБлокировкиДанных";

struct Ids {
    lock: TypeIdx,
    add: MemberId,
    lock_it: MemberId,
    count: MemberId,
    get: MemberId,
    item: TypeIdx,
    data_source: MemberId,
    region: MemberId,
    fields: MemberId,
    mode: MemberId,
    set_value: MemberId,
    use_from_source: MemberId,
    fields_type: TypeIdx,
    fields_count: MemberId,
    fields_get: MemberId,
    field_type: TypeIdx,
    field_field: MemberId,
    field_value: MemberId,
    field_column: MemberId,
    mode_enum: TypeIdx,
    control_enum: TypeIdx,
}

impl Ids {
    fn build(c: &Catalog) -> Option<Ids> {
        let lock = c.find_type(LOCK)?;
        let item = c.find_type(ITEM)?;
        let fields_type = c.find_type("ПоляЭлементаБлокировкиДанных")?;
        let field_type = c.find_type("ПолеЭлементаБлокировкиДанных")?;
        let member = |t: TypeIdx, name: &str| member_of(c, t, name).map(|(m, _)| m);
        Some(Ids {
            lock,
            add: member(lock, "Добавить")?,
            lock_it: member(lock, "Заблокировать")?,
            count: member(lock, "Количество")?,
            get: member(lock, "Получить")?,
            item,
            data_source: member(item, "ИсточникДанных")?,
            region: member(item, "Область")?,
            fields: member(item, "Поля")?,
            mode: member(item, "Режим")?,
            set_value: member(item, "УстановитьЗначение")?,
            use_from_source: member(item, "ИспользоватьИзИсточникаДанных")?,
            fields_type,
            fields_count: member(fields_type, "Количество")?,
            fields_get: member(fields_type, "Получить")?,
            field_type,
            field_field: member(field_type, "Поле")?,
            field_value: member(field_type, "Значение")?,
            field_column: member(field_type, "КолонкаИсточникаДанных")?,
            mode_enum: c.find_type("РежимБлокировкиДанных")?,
            control_enum: c.find_type("РежимУправленияБлокировкойДанных")?,
        })
    }
}

fn ids() -> Option<&'static Ids> {
    static IDS: OnceLock<Option<Ids>> = OnceLock::new();
    IDS.get_or_init(|| Ids::build(catalog())).as_ref()
}

fn l5_text(key: &'static str) -> HostError {
    HostError::text(
        &errors::render(key, &[], Lang::Ru),
        &errors::render(key, &[], Lang::En),
    )
}

fn bad_namespace(text: &str) -> HostError {
    let both = |lang: Lang| {
        format!(
            "{} ({}{})",
            errors::render(keys::PARAMETER_VALUE_MISMATCH, &["1"], lang),
            errors::render(keys::BAD_LOCK_NAMESPACE, &[], lang),
            text
        )
    };
    HostError::text(&both(Lang::Ru), &both(Lang::En))
}

pub(crate) fn conflict() -> HostError {
    TxnError::Store(StoreError::WriterBusy).host()
}

fn internal(text: &str) -> HostError {
    let t = format!("internal: {text}");
    HostError::text(&t, &t)
}

fn other(t: TypeIdx, ru_type: &str, m: MemberId) -> HostError {
    match member_name(catalog(), t, m) {
        Some((_, ru, _)) => HostError::not_implemented(ru_type, ru),
        None => internal("a member that the type does not have"),
    }
}

fn off_bound(by_method: bool) -> HostError {
    HostError::key(if by_method {
        bsl_stdlib::bind::msg::RANGE
    } else {
        bsl_stdlib::bind::msg::INDEX_OFFBOUND
    })
}

fn number_index(v: &Value) -> Option<usize> {
    match v {
        Value::Number(n) => usize::try_from(n.trunc_to_i64_saturating()).ok(),
        _ => None,
    }
}

fn lockable(kind: ObjKind) -> bool {
    !matches!(
        kind,
        ObjKind::DocumentJournal | ObjKind::Enum | ObjKind::DataProcessor | ObjKind::Report
    )
}

fn space_key(def: &ObjDef) -> String {
    format!("{}.{}", def.kind.name(Lang::Ru), def.name)
}

struct Space {
    text: String,
    key: String,
}

enum Refused {
    Name,
    Later(&'static str),
}

fn records_suffix(def: &ObjDef, word: &str) -> Option<&'static str> {
    let suffix = match fold_name(word).as_str() {
        "наборзаписей" | "recordset" => ".НаборЗаписей",
        "записи" | "records" if def.kind == ObjKind::Sequence => ".Записи",
        _ => return None,
    };
    let fits = match def.kind {
        ObjKind::InformationRegister => {
            def.behavior.write_mode == Some(InfoRegWriteMode::RecorderSubordinate)
        }
        ObjKind::AccumulationRegister
        | ObjKind::AccountingRegister
        | ObjKind::CalculationRegister
        | ObjKind::Sequence => true,
        _ => false,
    };
    fits.then_some(suffix)
}

fn parse_space(model: &AppModel, text: &str) -> Result<Space, Refused> {
    let mut parts = text.split('.');
    let word = parts.next().unwrap_or_default();
    match fold_name(word).as_str() {
        "перерасчет" | "recalculation" => return Err(Refused::Later("Перерасчет")),
        "внешнийисточникданных" | "externaldatasource" => {
            return Err(Refused::Later("ВнешнийИсточникДанных"));
        }
        _ => {}
    }
    let kind = ObjKind::from_word(word)
        .filter(|k| lockable(*k))
        .ok_or(Refused::Name)?;
    let name = parts.next().ok_or(Refused::Name)?;
    let def = model.obj(model.find(kind, name).ok_or(Refused::Name)?);
    let rest: Vec<&str> = parts.collect();
    let suffix = match rest.as_slice() {
        [] => "",
        [word] => records_suffix(def, word).ok_or(Refused::Name)?,
        _ => return Err(Refused::Name),
    };
    Ok(Space {
        text: text.to_owned(),
        key: format!("{}{suffix}", space_key(def)),
    })
}

fn mode_value(mode: LockMode) -> Option<Value> {
    let name = match mode {
        LockMode::Exclusive => "Exclusive",
        LockMode::Shared => "Shared",
    };
    bsl_stdlib::enums::enum_value(ids()?.mode_enum, name)
}

fn mode_of(v: &Value) -> Option<LockMode> {
    let want = ids()?.mode_enum;
    let e = bsl_stdlib::enums::as_enum_value(v).filter(|e| e.type_idx() == want)?;
    match e.name_en() {
        "Exclusive" => Some(LockMode::Exclusive),
        "Shared" => Some(LockMode::Shared),
        _ => None,
    }
}

pub(crate) fn control_of(v: &Value) -> Option<LockControl> {
    let want = ids()?.control_enum;
    let e = bsl_stdlib::enums::as_enum_value(v).filter(|e| e.type_idx() == want)?;
    match e.name_en() {
        "Automatic" => Some(LockControl::Automatic),
        "Managed" => Some(LockControl::Managed),
        _ => None,
    }
}

fn managed_locks(app: &Application, txn: &CallTxn) -> bool {
    match app.model().config.data_lock_control_mode() {
        DataLockControlMode::Managed => true,
        DataLockControlMode::Automatic => false,
        DataLockControlMode::AutomaticAndManaged => txn.control() == Some(LockControl::Managed),
    }
}

#[derive(Clone)]
struct Entry {
    field: Value,
    value: Option<Value>,
    column: Option<String>,
}

fn entry_bytes(e: &Entry, visit: &mut dyn RetainedMemory) -> Result<(), Exception> {
    visit.value(&e.field)?;
    if let Some(v) = &e.value {
        visit.value(v)?;
    }
    if let Some(c) = &e.column {
        visit.bytes(c.capacity() as u64)?;
    }
    Ok(())
}

fn entries_census(
    entries: &Rc<RefCell<Vec<Entry>>>,
    visit: &mut dyn RetainedMemory,
) -> Result<(), Exception> {
    let list = entries.borrow();
    let bytes = slots::<Entry>(list.capacity());
    if visit.allocation(Rc::as_ptr(entries) as usize, bytes)? {
        for e in list.iter() {
            entry_bytes(e, visit)?;
        }
    }
    Ok(())
}

struct Item {
    ids: &'static Ids,
    space: Space,
    mode: Cell<LockMode>,
    source: RefCell<Value>,
    entries: Rc<RefCell<Vec<Entry>>>,
}

impl Item {
    fn new(ids: &'static Ids, space: Space) -> Item {
        Item {
            ids,
            space,
            mode: Cell::new(LockMode::Exclusive),
            source: RefCell::new(Value::Undefined),
            entries: Rc::default(),
        }
    }

    fn put(&self, entry: Entry) {
        let Ok(mut entries) = self.entries.try_borrow_mut() else {
            return;
        };
        let name = |e: &Entry| match &e.field {
            Value::String(s) => Some(fold_name(&s.to_string_lossy())),
            _ => None,
        };
        let new = name(&entry);
        let at = new
            .as_ref()
            .and_then(|n| entries.iter().position(|e| name(e).as_ref() == Some(n)));
        match at.and_then(|i| entries.get_mut(i)) {
            Some(slot) => *slot = entry,
            None => entries.push(entry),
        }
    }

    fn other(&self, m: MemberId) -> HostError {
        other(self.ids.item, ITEM, m)
    }
}

fn field_arg(ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
    match arg_value(ex, args, 0)? {
        v @ (Value::String(_) | Value::Object(_)) => Ok(v),
        _ => Err(type_mismatch(1).into()),
    }
}

impl HostObject for Item {
    fn retained_memory(&self, visit: &mut dyn RetainedMemory) -> Result<bool, Exception> {
        visit.bytes((self.space.text.capacity() + self.space.key.capacity()) as u64)?;
        visit.value(&self.source.borrow())?;
        entries_census(&self.entries, visit)?;
        Ok(true)
    }

    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.ids.item)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        let ids = self.ids;
        if m == ids.data_source {
            Ok(self.source.borrow().clone())
        } else if m == ids.region {
            Ok(Value::from(self.space.text.as_str()))
        } else if m == ids.fields {
            Ok(Value::Object(Rc::new(Fields {
                ids,
                entries: self.entries.clone(),
            })))
        } else if m == ids.mode {
            mode_value(self.mode.get())
                .ok_or_else(|| internal("the mode of a lock has no value").into())
        } else {
            Err(self.other(m).into())
        }
    }

    fn set(&self, _: &mut Execution, m: MemberId, v: Value) -> CallResult<()> {
        if m == self.ids.mode {
            let mode = mode_of(&v).ok_or_else(crate::session::type_mismatch)?;
            self.mode.set(mode);
            Ok(())
        } else if m == self.ids.data_source {
            match v {
                Value::Object(_) | Value::Undefined => {
                    *self.source.borrow_mut() = v;
                    Ok(())
                }
                _ => Err(crate::session::type_mismatch().into()),
            }
        } else {
            Err(self.other(m).into())
        }
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        if m == self.ids.set_value {
            let field = field_arg(ex, args)?;
            let value = arg_value(ex, args, 1)?;
            self.put(Entry {
                field,
                value: Some(value),
                column: None,
            });
            Ok(Value::Undefined)
        } else if m == self.ids.use_from_source {
            let field = field_arg(ex, args)?;
            let Value::String(column) = arg_value(ex, args, 1)? else {
                return Err(type_mismatch(2).into());
            };
            self.put(Entry {
                field,
                value: None,
                column: Some(column.to_string_lossy()),
            });
            Ok(Value::Undefined)
        } else {
            Err(self.other(m).into())
        }
    }
}

struct Fields {
    ids: &'static Ids,
    entries: Rc<RefCell<Vec<Entry>>>,
}

impl Fields {
    fn view(&self, i: usize) -> Option<Value> {
        let entry = self.entries.borrow().get(i).cloned()?;
        Some(Value::Object(Rc::new(FieldView {
            ids: self.ids,
            entry,
        })))
    }
}

impl HostObject for Fields {
    fn retained_memory(&self, visit: &mut dyn RetainedMemory) -> Result<bool, Exception> {
        entries_census(&self.entries, visit)?;
        Ok(true)
    }

    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.ids.fields_type)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        Err(other(self.ids.fields_type, "ПоляЭлементаБлокировкиДанных", m).into())
    }

    fn set(&self, _: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        Err(other(self.ids.fields_type, "ПоляЭлементаБлокировкиДанных", m).into())
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        if m == self.ids.fields_count {
            Ok(Value::from(
                i64::try_from(self.entries.borrow().len()).unwrap_or(i64::MAX),
            ))
        } else if m == self.ids.fields_get {
            let at = number_index(&arg_value(ex, args, 0)?);
            at.and_then(|i| self.view(i))
                .ok_or_else(|| off_bound(true).into())
        } else {
            Err(other(self.ids.fields_type, "ПоляЭлементаБлокировкиДанных", m).into())
        }
    }

    fn iter_len(&self, _: &mut Execution) -> Option<usize> {
        Some(self.entries.borrow().len())
    }

    fn iter_get(&self, _: &mut Execution, i: usize) -> CallResult<Value> {
        self.view(i).ok_or_else(|| off_bound(true).into())
    }

    fn index_get(&self, _: &mut Execution, key: &Value) -> CallResult<Value> {
        number_index(key)
            .and_then(|i| self.view(i))
            .ok_or_else(|| off_bound(false).into())
    }
}

struct FieldView {
    ids: &'static Ids,
    entry: Entry,
}

impl HostObject for FieldView {
    fn retained_memory(&self, visit: &mut dyn RetainedMemory) -> Result<bool, Exception> {
        entry_bytes(&self.entry, visit)?;
        Ok(true)
    }

    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.ids.field_type)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        let ids = self.ids;
        if m == ids.field_field {
            Ok(self.entry.field.clone())
        } else if m == ids.field_value {
            Ok(self.entry.value.clone().unwrap_or(Value::Undefined))
        } else if m == ids.field_column {
            Ok(self
                .entry
                .column
                .as_deref()
                .map_or(Value::Undefined, Value::from))
        } else {
            Err(other(ids.field_type, "ПолеЭлементаБлокировкиДанных", m).into())
        }
    }

    fn set(&self, _: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        Err(other(self.ids.field_type, "ПолеЭлементаБлокировкиДанных", m).into())
    }

    fn call(&self, _: &mut Execution, m: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        Err(other(self.ids.field_type, "ПолеЭлементаБлокировкиДанных", m).into())
    }
}

struct DataLock {
    app: Arc<Application>,
    ids: &'static Ids,
    items: RefCell<Vec<Rc<Item>>>,
}

impl DataLock {
    fn item(&self, i: usize) -> Option<Value> {
        let item = self.items.borrow().get(i).cloned()?;
        Some(Value::Object(item))
    }

    fn add(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let Value::String(text) = arg_value(ex, args, 0)? else {
            return Err(type_mismatch(1).into());
        };
        let text = text.to_string_lossy();
        let space = match parse_space(self.app.model(), &text) {
            Ok(space) => space,
            Err(Refused::Name) => return Err(bad_namespace(&text).into()),
            Err(Refused::Later(what)) => {
                return Err(HostError::not_implemented(LOCK, &format!("Добавить: {what}")).into());
            }
        };
        let item = Rc::new(Item::new(self.ids, space));
        self.items.borrow_mut().push(item.clone());
        Ok(Value::Object(item))
    }

    fn lock(&self, ex: &mut Execution) -> CallResult<Value> {
        let txn = CallTxn::of_call(ex).ok_or_else(|| internal("a data lock outside of a call"))?;
        if !txn.active() || !managed_locks(&self.app, &txn) {
            return Err(l5_text(keys::DATALOCK_OUTSIDE_TRANSACTION).into());
        }
        txn.check().map_err(|e| e.host())?;
        let wait = self.app.options().lock_timeout;
        let items: Vec<Rc<Item>> = self.items.borrow().clone();
        for item in items {
            if txn
                .lock(self.app.locks(), &item.space.key, item.mode.get(), wait)
                .is_err()
            {
                txn.poison();
                return Err(conflict().into());
            }
        }
        Ok(Value::Undefined)
    }

    fn other(&self, m: MemberId) -> HostError {
        other(self.ids.lock, LOCK, m)
    }
}

impl HostObject for DataLock {
    fn retained_memory(&self, visit: &mut dyn RetainedMemory) -> Result<bool, Exception> {
        let items = self.items.borrow();
        visit.bytes(slots::<Rc<Item>>(items.capacity()))?;
        for item in items.iter() {
            visit.value(&Value::Object(item.clone()))?;
        }
        Ok(true)
    }

    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.ids.lock)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        Err(self.other(m).into())
    }

    fn set(&self, _: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        Err(self.other(m).into())
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        if m == self.ids.add {
            self.add(ex, args)
        } else if m == self.ids.lock_it {
            self.lock(ex)
        } else if m == self.ids.count {
            Ok(Value::from(
                i64::try_from(self.items.borrow().len()).unwrap_or(i64::MAX),
            ))
        } else if m == self.ids.get {
            let at = number_index(&arg_value(ex, args, 0)?);
            at.and_then(|i| self.item(i))
                .ok_or_else(|| off_bound(true).into())
        } else {
            Err(self.other(m).into())
        }
    }

    fn iter_len(&self, _: &mut Execution) -> Option<usize> {
        Some(self.items.borrow().len())
    }

    fn iter_get(&self, _: &mut Execution, i: usize) -> CallResult<Value> {
        self.item(i).ok_or_else(|| off_bound(true).into())
    }

    fn index_get(&self, _: &mut Execution, key: &Value) -> CallResult<Value> {
        number_index(key)
            .and_then(|i| self.item(i))
            .ok_or_else(|| off_bound(false).into())
    }
}

pub(crate) fn register(app: &Arc<Application>, reg: &mut Registry) {
    let Some(ids) = ids() else {
        return;
    };
    let app = app.clone();
    reg.type_impl(
        ids.lock,
        TypeImpl {
            ctor: Some(Arc::new(move |_, _| {
                Ok(Value::Object(Rc::new(DataLock {
                    app: app.clone(),
                    ids,
                    items: RefCell::default(),
                })))
            })),
        },
    );
}

pub(crate) fn lock_for_write(
    app: &Application,
    txn: &CallTxn,
    def: &ObjDef,
) -> Result<(), Conflict> {
    txn.lock(
        app.locks(),
        &space_key(def),
        LockMode::Exclusive,
        app.options().lock_timeout,
    )
}

fn object_of(app: &Application, table: TableId) -> Option<&ObjDef> {
    let map = app.store().storage();
    let mut info = map.try_table(table)?;
    while let Some(owner) = info.owner {
        info = map.try_table(owner)?;
    }
    let def = app.model().obj(app.model().by_uuid(info.md?)?);
    lockable(def.kind).then_some(def)
}

pub(crate) fn lock_tables(
    app: &Application,
    txn: &CallTxn,
    tables: &[TableId],
    for_update: bool,
) -> Result<(), Conflict> {
    if !txn.active() {
        return Ok(());
    }
    let wait = app.options().lock_timeout;
    let mode = if for_update && !managed_locks(app, txn) {
        LockMode::Exclusive
    } else {
        LockMode::Shared
    };
    for &table in tables {
        let Some(def) = object_of(app, table) else {
            continue;
        };
        txn.lock(app.locks(), &space_key(def), mode, wait)?;
    }
    Ok(())
}
