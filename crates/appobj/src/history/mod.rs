mod content;
mod dataid;
mod manager;
mod versions;

pub(crate) use manager::register;

use crate::Application;
use crate::model::{FieldRole, ObjDef, ObjId, ObjKind, Periodicity};
use crate::txn::CallTxn;
use bsl_vm::{CallResult, Execution, Value};
use content::{Body, Change, Field, Header, Tab, Val};
use dataid::KeyValue;
use ibschema::Numeric;
use ibschema::Row;
use ibschema::fields::FieldValue;
use ibschema::logical::{TypeCode, VAR};
use ibschema::names::{StdField, purpose};
use ibschema::storage::{FieldId, IndexId, TableId, TableInfo};
use ibschema::store::{Direction, ReadTxn, StoreResult, WriteTxn};
use ibschema::values::{RefId, datetime_to_ticks};
use md_model::Value as MdValue;
use std::cell::RefCell;
use std::ops::Bound;

const VERSION_INT_MAX: u64 = 0x3fff;

const KEY_INT_MAX: u64 = 0x7fff_ffff;

pub(crate) fn kept(app: &Application, obj: ObjId) -> bool {
    let model = app.model();
    model
        .config
        .object(model.obj(obj).md)
        .is_some_and(|md| matches!(md.prop("DataHistory"), Some(MdValue::Text(t)) if t == "Use"))
}

pub(crate) struct Stamp {
    user: [u8; 16],
    utc_ticks: u64,
    txn: [u8; 16],
}

impl Stamp {
    pub(crate) fn now(ex: &mut Execution) -> CallResult<Stamp> {
        let utc = ex.env().now_utc().trunc_seconds();
        let local = match ex.call_global("ТекущаяДата", &mut [])? {
            Value::Date(d) => d,
            _ => utc,
        };
        let session = crate::session::call_ctx(ex).map_or(0, |c| c.sess.number());
        let mut txn = [0u8; 16];
        let (head, tail) = txn.split_at_mut(8);
        head.copy_from_slice(&u64::try_from(local.ticks()).unwrap_or(0).to_le_bytes());
        tail.copy_from_slice(&u64::from(session).to_le_bytes());
        Ok(Stamp {
            user: RefId::from_uuid(evlog::UserRef::nobody().uuid).0,
            utc_ticks: u64::try_from(utc.ticks()).unwrap_or(0),
            txn,
        })
    }

    fn header(&self, key: Vec<u8>) -> Header {
        Header {
            version: RefId::from_uuid(random_v4()).0,
            key,
            user: self.user,
            user_name: String::new(),
            full_name: String::new(),
            utc_ticks: self.utc_ticks,
            txn: self.txn,
        }
    }
}

pub(crate) fn random_v4() -> crate::model::Uuid16 {
    let mut text = [0u8; 16];
    rand::RngCore::fill_bytes(&mut rand::rng(), &mut text);
    if let Some(b) = text.get_mut(6) {
        *b = (*b & 0x0f) | 0x40;
    }
    if let Some(b) = text.get_mut(8) {
        *b = (*b & 0x3f) | 0x80;
    }
    crate::model::Uuid16::from_bytes(text)
}

struct Stamped {
    app: std::sync::Weak<Application>,
    stamp: [u8; 16],
    serial: u64,
    open: std::sync::Weak<()>,
}

static STAMPED: std::sync::Mutex<Vec<Stamped>> = std::sync::Mutex::new(Vec::new());
const STAMPS: usize = 64;

fn stamped() -> std::sync::MutexGuard<'static, Vec<Stamped>> {
    STAMPED.lock().unwrap_or_else(|e| e.into_inner())
}

fn take_stamp(
    taken: &mut Vec<Stamped>,
    me: &std::sync::Weak<Application>,
    stamp: &mut [u8; 16],
    serial: u64,
    open: &std::sync::Arc<()>,
) -> Result<bool, &'static str> {
    taken.retain(|e| e.app.strong_count() > 0 || std::sync::Weak::ptr_eq(&e.app, me));
    let mine = |e: &Stamped| std::sync::Weak::ptr_eq(&e.app, me);
    if let Some(e) = taken.iter().find(|e| mine(e) && e.serial == serial) {
        *stamp = e.stamp;
        return Ok(false);
    }
    if taken.iter().any(|e| mine(e) && e.stamp == *stamp) {
        return Err("вторая транзакция сеанса в ту же секунду");
    }
    if taken.iter().filter(|e| mine(e)).count() >= STAMPS {
        let ended = taken
            .iter()
            .position(|e| mine(e) && e.open.strong_count() == 0)
            .ok_or("нет места для id транзакции истории: все отметки у открытых транзакций")?;
        taken.remove(ended);
    }
    taken.push(Stamped {
        app: me.clone(),
        stamp: *stamp,
        serial,
        open: std::sync::Arc::downgrade(open),
    });
    Ok(true)
}

fn transaction_stamp(
    app: &Application,
    txn: &CallTxn,
    stamp: &mut [u8; 16],
) -> Option<&'static str> {
    let serial = txn.serial()?;
    let me = app.arc().map(|a| std::sync::Arc::downgrade(&a))?;
    let open = std::sync::Arc::new(());
    match take_stamp(&mut stamped(), &me, stamp, serial, &open) {
        Err(why) => return Some(why),
        Ok(false) => return None,
        Ok(true) => {}
    }
    let stamp = *stamp;
    txn.on_rollback(move || {
        let _open = &open;
        stamped().retain(|e| {
            !(std::sync::Weak::ptr_eq(&e.app, &me) && e.stamp == stamp && e.serial == serial)
        });
    });
    None
}

struct Tables<'a> {
    queue: TableId,
    q_index: IndexId,
    m_index: IndexId,
    q: &'a TableInfo,
    q_md: usize,
    q_data: usize,
    q_pos: usize,
    q_content: usize,
    meta: TableId,
    m_md: usize,
    m_actual: usize,
    m_content: usize,
}

fn column(ti: &TableInfo, name: &str) -> Result<usize, String> {
    ti.field_by_name(name)
        .map(|f| usize::from(f.0))
        .ok_or_else(|| format!("нет поля {name} таблицы {}", ti.logical))
}

impl<'a> Tables<'a> {
    fn of(app: &'a Application) -> Result<Tables<'a>, String> {
        let map = app.store().storage();
        let table = |name: &str| {
            map.table_by_name(name)
                .and_then(|t| map.try_table(t).map(|ti| (t, ti)))
                .ok_or_else(|| format!("нет таблицы {name}"))
        };
        let (queue, q) = table(purpose::DATA_HISTORY_QUEUE0)?;
        let (meta, m) = table(purpose::DATA_HISTORY_METADATA)?;
        if !q.separators.is_empty() || !m.separators.is_empty() {
            return Err("разделённые таблицы истории".into());
        }
        let q_md = column(q, "MetadataId")?;
        let m_md = column(m, "MetadataId")?;
        Ok(Tables {
            queue,
            q_index: index_from(q, q_md)?,
            m_index: index_from(m, m_md)?,
            q,
            q_md,
            q_data: column(q, "DataId")?,
            q_pos: column(q, "Position")?,
            q_content: column(q, "Content")?,
            meta,
            m_md,
            m_actual: column(m, "IsActual")?,
            m_content: column(m, "Content")?,
        })
    }
}

fn index_from(ti: &TableInfo, first: usize) -> Result<IndexId, String> {
    let starts = |ix: &ibschema::storage::IndexInfo| {
        !ix.absent && ix.prefix == 0 && ix.fields.first().is_some_and(|f| usize::from(f.0) == first)
    };
    if let Some(at) = ti.indexes.iter().position(starts) {
        return u16::try_from(at)
            .map(IndexId::Named)
            .map_err(|_| "номер индекса".to_string());
    }
    if ti.key_index.as_ref().is_some_and(starts) {
        return Ok(IndexId::Id);
    }
    Err(format!("нет индекса таблицы {}", ti.logical))
}

fn cell_bytes(v: Option<&FieldValue>) -> Option<&[u8]> {
    match v {
        Some(FieldValue::Binary(b) | FieldValue::Storage(b)) => Some(b.as_slice()),
        _ => None,
    }
}

type Described = (Vec<Field>, Vec<Tab>);

pub(crate) struct Queue<'a> {
    app: &'a Application,
    def: &'a ObjDef,
    main: &'a TableInfo,
    tables: Tables<'a>,
    md: [u8; 16],
    md_cell: FieldValue,
    stamp: Stamp,
    number: Option<u32>,
    fields: RefCell<Option<Described>>,
    refusal: RefCell<Option<String>>,
}

fn refusal(def: &ObjDef, what: &str) -> String {
    let kind = def.kind.name(platform_catalog::messages::Lang::Ru);
    format!("история данных {kind}.{}: {what}", def.name)
}

pub(crate) fn refused_deletion(app: &Application, reg: ObjId) -> String {
    refusal(app.model().obj(reg), "удаление записей регистра")
}

impl<'a> Queue<'a> {
    pub(crate) fn of(
        app: &'a Application,
        txn: &CallTxn,
        obj: ObjId,
        stamp: Stamp,
    ) -> Option<Queue<'a>> {
        if !kept(app, obj) {
            return None;
        }
        let model = app.model();
        let def = model.obj(obj);
        let checked = (|| -> Result<Queue<'a>, String> {
            match def.kind {
                ObjKind::Catalog if def.tabular.is_empty() && !def.behavior.hierarchical => {}
                ObjKind::Catalog => return Err("иерархия или табличные части".into()),
                ObjKind::Document => {}
                ObjKind::InformationRegister => {
                    if def.behavior.write_mode != Some(crate::model::InfoRegWriteMode::Independent)
                    {
                        return Err("регистр, подчинённый регистратору".into());
                    }
                    if def.behavior.periodicity == Some(Periodicity::RecorderPosition) {
                        return Err("периодичность по позиции регистратора".into());
                    }
                }
                _ => return Err("вид объекта".into()),
            }
            let st = model.storage_of(obj).ok_or("нет таблицы объекта")?;
            let main = app
                .store()
                .storage()
                .try_table(st.main)
                .ok_or("нет таблицы объекта")?;
            if !main.separators.is_empty() {
                return Err("разделённый объект".into());
            }
            Ok(Queue {
                app,
                def,
                main,
                tables: Tables::of(app)?,
                md: RefId::from_uuid(def.md).0,
                md_cell: FieldValue::Uuid(def.md),
                stamp,
                number: st.ref_number,
                fields: RefCell::new(None),
                refusal: RefCell::new(None),
            })
        })();
        match checked {
            Ok(q) => Some(q),
            Err(what) => {
                txn.refuse_commit(refusal(def, &what));
                None
            }
        }
    }

    pub(crate) fn for_write(
        ex: &mut Execution,
        app: &'a Application,
        txn: &CallTxn,
        obj: ObjId,
    ) -> CallResult<Option<Queue<'a>>> {
        if !kept(app, obj) {
            return Ok(None);
        }
        let mut stamp = Stamp::now(ex)?;
        let refused = transaction_stamp(app, txn, &mut stamp.txn);
        let queue = Queue::of(app, txn, obj, stamp);
        if let (Some(what), Some(q)) = (refused, &queue) {
            q.refuse(what);
        }
        Ok(queue)
    }

    fn refuse(&self, what: &str) {
        let mut slot = self.refusal.borrow_mut();
        if slot.is_none() {
            *slot = Some(refusal(self.def, what));
        }
    }

    pub(crate) fn finish(self, txn: &CallTxn) {
        if let Some(what) = self.refusal.into_inner() {
            txn.refuse_commit(what);
        }
    }

    fn fields(&self, r: &dyn ReadTxn) -> StoreResult<Result<Described, String>> {
        if let Some(f) = self.fields.borrow().as_ref() {
            return Ok(Ok(f.clone()));
        }
        let t = &self.tables;
        let key = [self.md_cell.clone()];
        let mut found = None;
        for item in r.range(
            t.meta,
            t.m_index,
            Bound::Included(key.as_slice()),
            Bound::Included(key.as_slice()),
            Direction::Forward,
        )? {
            let (_, row) = item?;
            if row.get(t.m_md) != Some(&self.md_cell)
                || row.get(t.m_actual) != Some(&FieldValue::Bool(true))
            {
                continue;
            }
            if found.is_some() {
                return Ok(Err("два актуальных описания метаданных истории".into()));
            }
            found = Some(cell_bytes(row.get(t.m_content)).map(content::described));
        }
        let fields = match found {
            None => return Ok(Err("нет актуального описания метаданных истории".into())),
            Some(None | Some(None)) => {
                return Ok(Err("описание метаданных истории не разобрано".into()));
            }
            Some(Some(Some(f))) => f,
        };
        if !fields.1.is_empty() && self.def.kind != ObjKind::Document {
            return Ok(Err("табличные части в описании метаданных истории".into()));
        }
        *self.fields.borrow_mut() = Some(fields.clone());
        Ok(Ok(fields))
    }

    fn position(&self, w: &dyn WriteTxn, data: &[u8; 20]) -> StoreResult<Result<i64, String>> {
        let t = &self.tables;
        let key = [self.md_cell.clone(), FieldValue::Binary(data.to_vec())];
        let count = |r: &dyn ReadTxn| -> StoreResult<usize> {
            let mut n = 0usize;
            for item in r.range(
                t.queue,
                t.q_index,
                Bound::Included(key.as_slice()),
                Bound::Included(key.as_slice()),
                Direction::Forward,
            )? {
                item?;
                n += 1;
            }
            Ok(n)
        };
        let committed = self.app.store().read()?;
        if count(&*committed)? > 0 {
            return Ok(Err("в очереди истории уже есть строка этого ключа".into()));
        }
        let own = count(w)?;
        Ok(Ok(i64::try_from(own).unwrap_or(i64::MAX).saturating_add(1)))
    }

    fn put(
        &self,
        w: &mut dyn WriteTxn,
        data: [u8; 20],
        header_key: Vec<u8>,
        change: Change,
        body: &Body<'_>,
    ) -> StoreResult<()> {
        let pos = match self.position(&*w, &data)? {
            Ok(p) => p,
            Err(what) => {
                self.refuse(&what);
                return Ok(());
            }
        };
        let t = &self.tables;
        let bytes = content::encode(&self.stamp.header(header_key), &self.md, change, body);
        let mut row: Row = vec![FieldValue::Null; t.q.fields.len()];
        for (at, v) in [
            (t.q_md, self.md_cell.clone()),
            (t.q_data, FieldValue::Binary(data.to_vec())),
            (t.q_pos, FieldValue::Number(Numeric::from_i64(pos))),
            (t.q_content, FieldValue::Binary(bytes)),
        ] {
            if let Some(slot) = row.get_mut(at) {
                *slot = v;
            }
        }
        w.insert(t.queue, &row).map(drop)
    }

    pub(crate) fn object(
        &self,
        w: &mut dyn WriteTxn,
        id: RefId,
        row: Option<&Row>,
        is_new: bool,
    ) -> StoreResult<()> {
        let (fields, tabs) = match self.fields(&*w)? {
            Ok(f) => f,
            Err(what) => {
                self.refuse(&what);
                return Ok(());
            }
        };
        let prepared = match row {
            None => Ok((Change::Deleted, Vec::new(), Vec::new())),
            Some(row) => self.values(&fields, row).and_then(|v| {
                let lines = self.lines(&*w, id, &tabs)?;
                Ok((if is_new { Change::New } else { Change::Changed }, v, lines))
            }),
        };
        let (change, values, lines) = match prepared {
            Ok(x) => x,
            Err(what) => {
                self.refuse(&what);
                return Ok(());
            }
        };
        let body = Body {
            fields: &fields,
            tabs: &tabs,
            values: &values,
            lines: &lines,
        };
        self.put(w, dataid::object_id(id), id.0.to_vec(), change, &body)
    }

    fn lines(
        &self,
        r: &dyn ReadTxn,
        id: RefId,
        tabs: &[Tab],
    ) -> Result<Vec<Vec<Vec<Val>>>, String> {
        if tabs.is_empty() {
            return Ok(Vec::new());
        }
        let model = self.app.model();
        let st = model.storage_of(self.def.id).ok_or("нет таблицы объекта")?;
        let number = self.number.ok_or("нет таблицы ссылок")?;
        let map = self.app.store().storage();
        let mut out = Vec::with_capacity(tabs.len());
        for tab in tabs {
            let md = RefId(tab.md).to_uuid();
            let i = self
                .def
                .tabular
                .iter()
                .position(|t| t.md == md)
                .ok_or("табличная часть версии без табличной части объекта")?;
            let (Some(td), Some((table, ids))) = (self.def.tabular.get(i), st.tabular.get(i))
            else {
                return Err("табличная часть без таблицы".into());
            };
            let ti = map.try_table(*table).ok_or("табличная часть без таблицы")?;
            let line_no = td
                .fields
                .iter()
                .position(|f| f.std == Some(crate::model::StdAttr::LineNumber))
                .and_then(|k| ids.get(k).copied().flatten())
                .ok_or("табличная часть без номера строки")?;
            let mut columns = Vec::with_capacity(tab.fields.len());
            for f in &tab.fields {
                let Field::Item(u) = f else {
                    return Err("стандартное поле табличной части в версии".into());
                };
                let col = RefId(*u).to_uuid();
                let fid = td
                    .fields
                    .iter()
                    .position(|c| c.std.is_none() && c.md == Some(col))
                    .and_then(|k| ids.get(k).copied().flatten())
                    .ok_or("колонка версии без колонки табличной части")?;
                columns.push(fid);
            }
            let key = [FieldValue::Ref { table: number, id }];
            let mut rows: Vec<(u64, Row)> = Vec::new();
            for item in r
                .range(
                    *table,
                    IndexId::Id,
                    Bound::Included(key.as_slice()),
                    Bound::Included(key.as_slice()),
                    Direction::Forward,
                )
                .map_err(|e| format!("чтение табличной части: {e}"))?
            {
                let (_, row) = item.map_err(|e| format!("чтение табличной части: {e}"))?;
                let n = match row.get(usize::from(line_no.0)) {
                    Some(FieldValue::Number(n)) => whole(n).ok_or("номер строки")?,
                    _ => return Err("номер строки".into()),
                };
                rows.push((n, row));
            }
            if rows.is_empty() {
                return Err("пустая табличная часть".into());
            }
            rows.sort_by_key(|(n, _)| *n);
            let mut lines = Vec::with_capacity(rows.len());
            for (_, row) in &rows {
                let mut line = Vec::with_capacity(columns.len());
                for fid in &columns {
                    let at = usize::from(fid.0);
                    let cell = row.get(at).ok_or("короткая строка")?;
                    line.push(self.value(ti, at, cell)?);
                }
                lines.push(line);
            }
            out.push(lines);
        }
        Ok(out)
    }

    pub(crate) fn record(&self, w: &mut dyn WriteTxn, row: &Row) -> StoreResult<()> {
        let fields = match self.fields(&*w)? {
            Ok(f) => f,
            Err(what) => {
                self.refuse(&what);
                return Ok(());
            }
        };
        let prepared = self
            .key(row)
            .and_then(|k| self.values(&fields.0, row).map(|v| (k, v)));
        let (key, values) = match prepared {
            Ok(x) => x,
            Err(what) => {
                self.refuse(&what);
                return Ok(());
            }
        };
        let (fields, tabs) = fields;
        if !tabs.is_empty() {
            self.refuse("табличные части в описании регистра");
            return Ok(());
        }
        let data = dataid::record_id(&key);
        let body = Body {
            fields: &fields,
            tabs: &[],
            values: &values,
            lines: &[],
        };
        self.put(w, data, data.to_vec(), Change::New, &body)
    }

    pub(crate) fn replaced(&self) {
        self.refuse("замещение или удаление записей регистра");
    }

    fn column_of(&self, f: &Field) -> Result<usize, String> {
        let std = |s: StdField| self.main.field_by_std(s);
        let found = match (self.def.kind, f) {
            (ObjKind::Catalog, Field::Std(-13)) => std(StdField::PredefinedId),
            (ObjKind::Catalog, Field::Std(-7)) => std(StdField::Marked),
            (ObjKind::Catalog, Field::Std(-5)) => std(StdField::OwnerId),
            (ObjKind::Catalog, Field::Std(-3)) => std(StdField::Description),
            (ObjKind::Catalog, Field::Std(-2)) => std(StdField::Code),
            (ObjKind::InformationRegister, Field::Std(-2)) => std(StdField::Period),
            (ObjKind::Document, Field::Std(-7)) => std(StdField::Posted),
            (ObjKind::Document, Field::Std(-4)) => std(StdField::Marked),
            (ObjKind::Document, Field::Std(-3)) => std(StdField::DateTime),
            (ObjKind::Document, Field::Std(-2)) => std(StdField::Number),
            (_, Field::Std(n)) => return Err(format!("стандартное поле версии {n}")),
            (_, Field::Item(u)) => {
                let md = RefId(*u).to_uuid();
                self.main
                    .fields
                    .iter()
                    .position(|fi| fi.md == Some(md) && fi.std.is_none())
                    .and_then(|i| u16::try_from(i).ok().map(FieldId))
            }
        };
        found
            .map(|f| usize::from(f.0))
            .ok_or_else(|| "поле версии без колонки".to_string())
    }

    fn values(&self, fields: &[Field], row: &Row) -> Result<Vec<Val>, String> {
        fields
            .iter()
            .map(|f| {
                let at = self.column_of(f)?;
                let cell = row.get(at).ok_or("короткая строка")?;
                if matches!(f, Field::Std(-13)) {
                    return match cell {
                        FieldValue::Uuid(u) if u.is_nil() => Ok(Val::Str(String::new())),
                        FieldValue::Binary(b) if b.iter().all(|x| *x == 0) => {
                            Ok(Val::Str(String::new()))
                        }
                        _ => Err("предопределённый элемент".into()),
                    };
                }
                self.value(self.main, at, cell)
            })
            .collect()
    }

    fn value(&self, ti: &TableInfo, at: usize, cell: &FieldValue) -> Result<Val, String> {
        let document = self.def.kind == ObjKind::Document;
        let fraction = ti.fields.get(at).is_some_and(|fi| {
            fi.types
                .iter()
                .any(|t| t.code == TypeCode::N && t.precision > 0)
        });
        if !document && fraction && matches!(cell, FieldValue::Number(n) if whole(n).is_some()) {
            return Err("целое значение в поле с дробной частью".into());
        }
        if document
            && let FieldValue::Ref { table, id } = cell
            && id.is_nil()
        {
            let single = ti.fields.get(at).is_some_and(|fi| {
                fi.types.len() == 1 && fi.types.iter().all(|t| t.code == TypeCode::R)
            });
            if !single {
                return Err("пустая ссылка составного типа".into());
            }
            return Ok(Val::EmptyRef { table: *table });
        }
        value_of(cell)
    }

    fn key(&self, row: &Row) -> Result<Vec<KeyValue>, String> {
        let model = self.app.model();
        let st = model.storage_of(self.def.id).ok_or("нет таблицы объекта")?;
        let mut out = Vec::new();
        if !matches!(
            self.def.behavior.periodicity,
            None | Some(Periodicity::Nonperiodical)
        ) {
            let at = self
                .main
                .field_by_std(StdField::Period)
                .ok_or("нет периода")?;
            match row.get(usize::from(at.0)) {
                Some(FieldValue::Date(d)) if d.year != 0 => {
                    out.push(KeyValue::Date(datetime_to_ticks(d)))
                }
                _ => return Err("период в ключе".into()),
            }
        }
        for (f, fid) in self.def.fields.iter().zip(&st.fields) {
            if f.role != FieldRole::Dimension {
                continue;
            }
            let fid = fid.ok_or("измерение без колонки")?;
            let cell = row.get(usize::from(fid.0)).ok_or("короткая строка")?;
            let fixed = self.main.fields.get(usize::from(fid.0)).is_some_and(|fi| {
                fi.types
                    .iter()
                    .any(|t| t.code == TypeCode::S && t.length & VAR == 0)
            });
            if fixed && matches!(cell, FieldValue::String(_)) {
                return Err("строка фиксированной длины в ключе".into());
            }
            out.push(self.key_value(cell)?);
        }
        Ok(out)
    }

    fn key_value(&self, cell: &FieldValue) -> Result<KeyValue, String> {
        match cell {
            FieldValue::Ref { table, id } if !id.is_nil() => {
                let model = self.app.model();
                let obj = model
                    .obj_of_ref_number(*table)
                    .ok_or("ссылка неизвестной таблицы в ключе")?;
                let type_id = model
                    .obj(obj)
                    .generated
                    .iter()
                    .find(|g| g.category == "Ref")
                    .map(|g| g.type_id)
                    .ok_or("ссылка без типа в ключе")?;
                Ok(KeyValue::Ref {
                    type_id,
                    table: *table,
                    id: *id,
                })
            }
            other => plain_key(other),
        }
    }
}

fn plain_key(cell: &FieldValue) -> Result<KeyValue, String> {
    match cell {
        FieldValue::String(s) => Ok(KeyValue::Str(s.clone())),
        FieldValue::Number(n) => match whole(n) {
            Some(v) if v <= KEY_INT_MAX => Ok(KeyValue::Int(v)),
            _ => Err(format!("число {} в ключе", number_text(n))),
        },
        other => Err(format!("{} в ключе", type_name(other))),
    }
}

fn whole(n: &Numeric) -> Option<u64> {
    let (int, frac) = n
        .digits()
        .split_at(n.digits().len().saturating_sub(n.scale() as usize));
    let zero = n.digits().iter().all(|d| *d == 0);
    if (n.is_negative() && !zero) || frac.iter().any(|d| *d != 0) {
        return None;
    }
    int.iter().try_fold(0u64, |acc, d| {
        acc.checked_mul(10)?.checked_add(u64::from(*d))
    })
}

fn number_text(n: &Numeric) -> String {
    let (int, frac) = n
        .digits()
        .split_at(n.digits().len().saturating_sub(n.scale() as usize));
    let digits = |d: &[u8]| d.iter().map(|x| char::from(b'0' + x)).collect::<String>();
    let frac = digits(frac);
    let frac = frac.trim_end_matches('0');
    let sign = if n.is_negative() { "-" } else { "" };
    if frac.is_empty() {
        format!("{sign}{}", digits(int))
    } else {
        format!("{sign}{}.{frac}", digits(int))
    }
}

fn type_name(v: &FieldValue) -> &'static str {
    match v {
        FieldValue::Null => "Null",
        FieldValue::Undefined => "Неопределено",
        FieldValue::Bool(_) => "Булево",
        FieldValue::Number(_) => "Число",
        FieldValue::Date(_) => "Дата",
        FieldValue::String(_) => "Строка",
        FieldValue::Uuid(_) => "УникальныйИдентификатор",
        FieldValue::Ref { .. } => "пустая ссылка",
        FieldValue::Storage(_) => "ХранилищеЗначения",
        FieldValue::Binary(_) => "ДвоичныеДанные",
        FieldValue::RowVersion(_) => "ВерсияДанных",
    }
}

fn value_of(cell: &FieldValue) -> Result<Val, String> {
    match cell {
        FieldValue::Bool(b) => Ok(Val::Bool(*b)),
        FieldValue::String(s) => Ok(Val::Str(s.clone())),
        FieldValue::Number(n) if n.is_negative() && whole(n) != Some(0) => {
            Err(format!("отрицательное число {}", number_text(n)))
        }
        FieldValue::Number(n) => match whole(n) {
            Some(v) if v > VERSION_INT_MAX => Err(format!(
                "целое {v} в версии (наблюдены 0…{VERSION_INT_MAX})"
            )),
            Some(v) => Ok(Val::Int(v)),
            None => Ok(Val::Decimal(number_text(n))),
        },
        FieldValue::Date(d) if d.year != 0 => Ok(Val::Date(datetime_to_ticks(d))),
        FieldValue::Date(_) => Err("пустая дата".into()),
        FieldValue::Ref { table, id } if !id.is_nil() => Ok(Val::Ref {
            id: id.0,
            table: *table,
        }),
        FieldValue::Storage(b) if b.as_slice() == crate::values::field::EMPTY_STORAGE => {
            Ok(Val::Storage(b.clone()))
        }
        FieldValue::Storage(b) if !b.is_empty() => {
            Err("непустое ХранилищеЗначения в версии".into())
        }
        FieldValue::Storage(_) => Err("пустой блоб ХранилищеЗначения".into()),
        other => Err(type_name(other).to_string()),
    }
}
