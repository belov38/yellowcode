mod content;
mod dataid;
mod decode;
mod manager;
mod versions;

pub(crate) use manager::register;

use crate::Application;
use crate::model::{FieldRole, ObjDef, ObjId, ObjKind, Periodicity, Usage};
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

pub(crate) fn std_field(kind: ObjKind, n: i64) -> Option<StdField> {
    Some(match (kind, n) {
        (ObjKind::Catalog, -13) => StdField::PredefinedId,
        (ObjKind::Catalog, -7) => StdField::Marked,
        (ObjKind::Catalog, -6) => StdField::Folder,
        (ObjKind::Catalog, -5) => StdField::OwnerId,
        (ObjKind::Catalog, -4) => StdField::ParentId,
        (ObjKind::Catalog, -3) => StdField::Description,
        (ObjKind::Catalog, -2) => StdField::Code,
        (ObjKind::InformationRegister, -2) => StdField::Period,
        (ObjKind::Document, -7) => StdField::Posted,
        (ObjKind::Document, -4) => StdField::Marked,
        (ObjKind::Document, -3) => StdField::DateTime,
        (ObjKind::Document, -2) => StdField::Number,
        _ => return None,
    })
}

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

    fn header(&self, key: Vec<u8>, changed_by_extension: bool) -> Header {
        Header {
            version: RefId::from_uuid(random_v4()).0,
            key,
            user: self.user,
            user_name: String::new(),
            full_name: String::new(),
            utc_ticks: self.utc_ticks,
            txn: self.txn,
            changed_by_extension,
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
    m_ext: Option<usize>,
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
            m_ext: m.field_by_name("IsExtensions").map(|f| usize::from(f.0)),
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

#[derive(Clone)]
struct Described {
    fields: Vec<Field>,
    tabs: Vec<Tab>,
    changed_by_extension: bool,
}

fn pick_description<'a>(
    actual: &[(bool, Option<&'a [u8]>)],
) -> Result<(bool, Option<&'a [u8]>), String> {
    let of = |ext: bool| actual.iter().filter(move |(e, _)| *e == ext);
    if of(true).count() > 1 || of(false).count() > 1 {
        return Err("два актуальных описания метаданных истории".into());
    }
    match (of(true).next(), of(false).next()) {
        (None, None) => Err("нет актуального описания метаданных истории".into()),
        (Some(e), Some(p)) if e.1 != p.1 => {
            Err("актуальные описания с расширениями и без них различаются".into())
        }
        (Some(e), _) => Ok(*e),
        (None, Some(p)) => Ok(*p),
    }
}

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
                ObjKind::Catalog
                    if def.behavior.hierarchical && !def.behavior.hierarchy_folders =>
                {
                    return Err("иерархия элементов".into());
                }
                ObjKind::Catalog => {}
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
        let mut rows = Vec::new();
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
            let ext = t
                .m_ext
                .is_some_and(|at| row.get(at) == Some(&FieldValue::Bool(true)));
            rows.push((ext, row));
        }
        let actual: Vec<(bool, Option<&[u8]>)> = rows
            .iter()
            .map(|(ext, row)| (*ext, cell_bytes(row.get(t.m_content))))
            .collect();
        let (with_extensions, content) = match pick_description(&actual) {
            Ok(d) => d,
            Err(what) => return Ok(Err(what)),
        };
        let changed_by_extension = match self.changed_by_extension(with_extensions) {
            Ok(c) => c,
            Err(what) => return Ok(Err(what)),
        };
        let Some((fields, tabs)) = content.and_then(content::described) else {
            return Ok(Err("описание метаданных истории не разобрано".into()));
        };
        if !tabs.is_empty() && !matches!(self.def.kind, ObjKind::Document | ObjKind::Catalog) {
            return Ok(Err("табличные части в описании метаданных истории".into()));
        }
        let described = Described {
            fields,
            tabs,
            changed_by_extension,
        };
        *self.fields.borrow_mut() = Some(described.clone());
        Ok(Ok(described))
    }

    fn changed_by_extension(&self, with_extensions: bool) -> Result<bool, String> {
        let md = self
            .app
            .model()
            .config
            .object(self.def.md)
            .ok_or("нет объекта метаданных")?;
        match crate::extensions::changed_object(self.app, md) {
            Err(why) => Err(format!("расширения: {why}")),
            Ok(None) => Ok(false),
            Ok(Some(true)) if self.def.kind == ObjKind::InformationRegister => {
                Err("регистр сведений, изменённый расширением".into())
            }
            Ok(Some(true)) => Ok(true),
            Ok(Some(false)) if with_extensions => Err(
                "описание метаданных истории с расширениями у объекта, не изменённого расширением"
                    .into(),
            ),
            Ok(Some(false)) => Ok(false),
        }
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
        changed_by_extension: bool,
    ) -> StoreResult<()> {
        let pos = match self.position(&*w, &data)? {
            Ok(p) => p,
            Err(what) => {
                self.refuse(&what);
                return Ok(());
            }
        };
        let t = &self.tables;
        let bytes = content::encode(
            &self.stamp.header(header_key, changed_by_extension),
            &self.md,
            change,
            body,
        );
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
        let Described {
            fields,
            tabs,
            changed_by_extension,
        } = match self.fields(&*w)? {
            Ok(f) => f,
            Err(what) => {
                self.refuse(&what);
                return Ok(());
            }
        };
        let prepared = match row {
            None if self.def.kind == ObjKind::Catalog && self.def.behavior.hierarchical => {
                Err("удаление в иерархическом справочнике".to_string())
            }
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
        self.put(
            w,
            dataid::object_id(id),
            id.0.to_vec(),
            change,
            &body,
            changed_by_extension,
        )
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
        if several_with_an_empty(&out) {
            return Err("несколько табличных частей, среди них без строк".into());
        }
        Ok(out)
    }

    pub(crate) fn record(&self, w: &mut dyn WriteTxn, row: &Row) -> StoreResult<()> {
        let described = match self.fields(&*w)? {
            Ok(f) => f,
            Err(what) => {
                self.refuse(&what);
                return Ok(());
            }
        };
        let prepared = self
            .key(row)
            .and_then(|k| self.values(&described.fields, row).map(|v| (k, v)));
        let (key, values) = match prepared {
            Ok(x) => x,
            Err(what) => {
                self.refuse(&what);
                return Ok(());
            }
        };
        let Described {
            fields,
            tabs,
            changed_by_extension,
        } = described;
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
        self.put(
            w,
            data,
            data.to_vec(),
            Change::New,
            &body,
            changed_by_extension,
        )
    }

    pub(crate) fn replaced(&self) {
        self.refuse("замещение или удаление записей регистра");
    }

    fn column_of(&self, f: &Field) -> Result<usize, String> {
        let found = match f {
            Field::Std(n) => match std_field(self.def.kind, *n) {
                Some(s) => self.main.field_by_std(s),
                None => return Err(format!("стандартное поле версии {n}")),
            },
            Field::Item(u) => {
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
        let catalog = self.def.kind == ObjKind::Catalog;
        let group = match self.main.field_by_std(StdField::Folder) {
            Some(f) if catalog => match row.get(usize::from(f.0)) {
                Some(FieldValue::Bool(b)) => Some(!*b),
                _ => None,
            },
            _ => None,
        };
        fields
            .iter()
            .map(|f| {
                if catalog
                    && matches!(f, Field::Std(-2))
                    && self.def.behavior.code_length == 0
                    && self.main.field_by_std(StdField::Code).is_none()
                {
                    return Ok(Val::Str(String::new()));
                }
                let at = self.column_of(f)?;
                let cell = row.get(at).ok_or("короткая строка")?;
                if catalog && matches!(f, Field::Std(-6)) {
                    return group.map(Val::Bool).ok_or_else(|| "ЭтоГруппа".to_string());
                }
                if let (FieldValue::Null, Field::Item(u)) = (cell, f) {
                    let md = RefId(*u).to_uuid();
                    let usage = self
                        .def
                        .fields
                        .iter()
                        .find(|d| d.md == Some(md))
                        .map(|d| d.usage);
                    return unused_null(usage, group);
                }
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
        let catalog = self.def.kind == ObjKind::Catalog;
        let fraction = ti.fields.get(at).is_some_and(|fi| {
            fi.types
                .iter()
                .any(|t| t.code == TypeCode::N && t.precision > 0)
        });
        if !document && fraction && matches!(cell, FieldValue::Number(n) if whole(n).is_some()) {
            return Err("целое значение в поле с дробной частью".into());
        }
        if (document || catalog)
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

fn several_with_an_empty(lines: &[Vec<Vec<Val>>]) -> bool {
    lines.len() > 1 && lines.iter().any(|l| l.is_empty())
}

fn unused_null(usage: Option<Usage>, group: Option<bool>) -> Result<Val, String> {
    match (usage, group) {
        (Some(Usage::ForItem), Some(true)) => Ok(Val::Null),
        _ => Err(type_name(&FieldValue::Null).to_string()),
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

const COMPRESSED_STORAGE: [u8; 18] = [
    0x02, 0x01, 0x53, 0x4B, 0x6F, 0xF4, 0x88, 0x8D, 0xC1, 0x4E, 0xA0, 0xD5, 0xEB, 0xB6, 0xBD, 0xA0,
    0xA7, 0x0D,
];

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
        FieldValue::Storage(b)
            if b.starts_with(&COMPRESSED_STORAGE) && b.len() > COMPRESSED_STORAGE.len() =>
        {
            Ok(Val::Storage(b.clone()))
        }
        FieldValue::Storage(b) if !b.is_empty() => {
            Err("непустое ХранилищеЗначения в версии".into())
        }
        FieldValue::Storage(_) => Err("пустой блоб ХранилищеЗначения".into()),
        other => Err(type_name(other).to_string()),
    }
}
