use super::filter::Filter;
use super::record::{Record, Slot, opaque};
use super::{Column, RecordSet, SetCore, Source, column_type};
use crate::Application;
use crate::errors::{self, keys};
use crate::events::{self, Event, Owner, Param};
use crate::meta::{arg_value, type_mismatch};
use crate::model::{FieldDef, FieldRole, InfoRegWriteMode, Periodicity, StdAttr};
use crate::read_budget as budget;
use crate::session::{call_ctx, internal};
use crate::txn::{CallTxn, Commit};
use crate::values::field::{from_field, to_field};
use bsl_prim::fold_name;
use bsl_vm::{Arg, CallResult, Category, CategorySet, Execution, HostError, ObjRef, Place, Value};
use ibschema::Row;
use ibschema::fields::FieldValue;
use ibschema::storage::{FieldId, IndexId, TableId, TableInfo};
use ibschema::store::{Direction, RowHandle, StoreError, StoreResult};
use platform_catalog::messages::Lang;
use std::ops::Bound;
use std::rc::Rc;

struct Plan<'a> {
    core: &'a SetCore,
    app: &'a Application,
    cols: Rc<Vec<Column>>,
    table: TableId,
    ti: &'a TableInfo,
    fields: Vec<FieldId>,
    index: IndexId,
    dims: Vec<usize>,
    area_prefix: bool,
    periodic: bool,
}

impl<'a> Plan<'a> {
    fn new(core: &'a SetCore, what: &str, write: bool) -> Result<Plan<'a>, HostError> {
        let app: &'a Application = &core.app;
        let def = core.def();
        let refuse = |why: &str| core.refusal(&format!("{what}: {why}"));
        if def.behavior.write_mode == Some(InfoRegWriteMode::RecorderSubordinate) {
            return Err(refuse("регистр, подчинённый регистратору"));
        }
        let periodicity = def.behavior.periodicity.unwrap_or_default();
        if !matches!(periodicity, Periodicity::Nonperiodical | Periodicity::Day) {
            return Err(refuse("периодичность регистра"));
        }
        let cols = core.cols()?;
        if cols
            .iter()
            .any(|c| matches!(c.source, Source::Separator(_)))
        {
            return Err(refuse("разделитель в записях набора"));
        }
        if write && app.options().read_only_scope.is_some() {
            return Err(refuse("приложение только для чтения (read_scope)"));
        }
        let st = app
            .model()
            .storage_of(core.obj)
            .ok_or_else(|| internal("the register has no storage"))?;
        let ti = app
            .store()
            .storage()
            .try_table(st.main)
            .ok_or_else(|| internal("no main table of the register"))?;
        if !ti.separators.is_empty() && app.options().area().is_none() {
            return Err(refuse("данные разделены, область данных не задана"));
        }
        let mut fields = Vec::with_capacity(cols.len());
        for c in cols.iter() {
            let Source::Field(i) = c.source else {
                return Err(internal("a separator column"));
            };
            fields.push(
                st.fields
                    .get(i)
                    .copied()
                    .flatten()
                    .ok_or_else(|| internal("a column without storage"))?,
            );
        }
        let periodic = periodicity != Periodicity::Nonperiodical;
        let dims: Vec<usize> = cols
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                core.field(c).is_some_and(|f| {
                    (f.std.is_none() && f.role == FieldRole::Dimension)
                        || (periodic && f.std == Some(StdAttr::Period))
                })
            })
            .map(|(i, _)| i)
            .collect();
        let mut dims = dims;
        dims.sort_by_key(|&i| {
            !cols
                .get(i)
                .and_then(|c| core.field(c))
                .is_some_and(|f| f.std == Some(StdAttr::Period))
        });
        let dim_fields: Vec<FieldId> = dims
            .iter()
            .filter_map(|i| fields.get(*i).copied())
            .collect();
        let found = ti.indexes.iter().enumerate().find(|(_, ix)| {
            ix.unique
                && !dim_fields.is_empty()
                && ix.fields.get(ix.prefix..) == Some(dim_fields.as_slice())
        });
        let Some((pos, ix)) = found else {
            return Err(refuse("нет уникального индекса измерений"));
        };
        Ok(Plan {
            core,
            app,
            cols,
            table: st.main,
            ti,
            fields,
            index: IndexId::Named(u16::try_from(pos).map_err(|_| internal("index number"))?),
            dims,
            area_prefix: ix.prefix != 0,
            periodic,
        })
    }

    fn field(&self, col: usize) -> Result<&'a FieldDef, HostError> {
        let core: &'a SetCore = self.core;
        self.cols
            .get(col)
            .and_then(|c| core.field(c))
            .ok_or_else(|| internal("no field of the column"))
    }

    fn area(&self) -> Result<Vec<FieldValue>, HostError> {
        if !self.area_prefix {
            return Ok(Vec::new());
        }
        let scope = self
            .app
            .options()
            .area()
            .ok_or_else(|| internal("separated table without an area"))?;
        scope
            .area(self.ti)
            .map_err(|_| internal("invalid bound data area"))
    }

    fn filter_key(&self, filter: &Filter, what: &str) -> Result<Vec<FieldValue>, HostError> {
        if filter.items.iter().any(|it| it.use_unknown()) {
            return Err(self
                .core
                .refusal(&format!("{what}: отбор: Значение без Использование")));
        }
        let mut key = self.area()?;
        let mut gap = false;
        for &col in &self.dims {
            let used = filter
                .items
                .iter()
                .find(|it| it.col == col)
                .and_then(|it| it.used_value());
            match used {
                Some(v) if !gap => key.push(to_field(self.app, self.field(col)?, &v)?),
                Some(_) => {
                    return Err(self
                        .core
                        .refusal(&format!("{what}: отбор не по первым измерениям")));
                }
                None => gap = true,
            }
        }
        Ok(key)
    }

    fn row_key(&self, row: &Row) -> Result<Vec<FieldValue>, HostError> {
        let mut key = self.area()?;
        for &col in &self.dims {
            let fid = self.fields.get(col).ok_or_else(|| internal("no field"))?;
            key.push(
                row.get(usize::from(fid.0))
                    .cloned()
                    .ok_or_else(|| internal("no cell"))?,
            );
        }
        Ok(key)
    }

    fn rows<'t>(
        &self,
        txn: &'t dyn ibschema::store::ReadTxn,
        key: &[FieldValue],
    ) -> StoreResult<ibschema::store::RowIter<'t>> {
        let bound = if key.is_empty() {
            Bound::Unbounded
        } else {
            Bound::Included(key)
        };
        txn.range(self.table, self.index, bound, bound, Direction::Forward)
    }

    fn slot(&self, col: usize, cell: &FieldValue) -> Result<Slot, HostError> {
        let c = self.cols.get(col).ok_or_else(|| internal("no column"))?;
        if opaque(self.core, c) {
            return Ok(Slot::Stored(Some(cell.clone())));
        }
        Ok(Slot::Value(from_field(self.app, self.field(col)?, cell)?))
    }
}

fn same(a: &FieldValue, b: &FieldValue) -> bool {
    match (a, b) {
        (FieldValue::Number(x), FieldValue::Number(y)) => x.eq_value(y),
        _ => a == b,
    }
}

fn other_error(ru: String, en: String) -> HostError {
    let mut e = HostError::text(&ru, &en);
    e.categories = Some(CategorySet::of(&[
        Category::AllErrors,
        Category::ScriptRuntimeError,
        Category::OtherError,
    ]));
    e
}

fn record_place(ex: &mut Execution, plan: &Plan<'_>, rec: &Record) -> CallResult<String> {
    let def = plan.core.def();
    let mut parts = Vec::with_capacity(plan.dims.len());
    for &col in &plan.dims {
        let v = rec.value(col)?;
        parts.push(ex.present(&v)?.to_string_lossy());
    }
    let synonym = crate::types::synonym(&def.synonym, &def.name, &plan.app.model().lang_code);
    Ok(format!("{synonym}: {}", parts.join(", ")))
}

fn record_source(plan: &Plan<'_>, line: Option<usize>, field: Option<&str>, lang: Lang) -> String {
    let mut s = format!(
        "({}: {}",
        errors::render(keys::INFOREG_CLASS, &[], lang),
        plan.core.def().name
    );
    if let Some(n) = line {
        s.push_str(&format!(
            "; {}: {n}",
            errors::render(keys::RECORD_LINE_NUMBER, &[], lang)
        ));
    }
    if let Some(f) = field {
        s.push_str(&format!(
            "; {}: {f}",
            errors::render(keys::FIELD_WORD, &[], lang)
        ));
    }
    s.push(')');
    s
}

fn duplicate(
    ex: &mut Execution,
    plan: &Plan<'_>,
    rec: &Record,
    line: Option<usize>,
) -> CallResult<HostError> {
    let place = record_place(ex, plan, rec)?;
    let text = |lang| {
        format!(
            "{} : {place} {}",
            errors::render(keys::RECORD_KEY_NOT_UNIQUE, &[], lang),
            record_source(plan, line, None, lang)
        )
    };
    Ok(other_error(text(Lang::Ru), text(Lang::En)))
}

fn outside_filter(
    ex: &mut Execution,
    plan: &Plan<'_>,
    rec: &Record,
    line: usize,
    field: &str,
) -> CallResult<HostError> {
    let place = record_place(ex, plan, rec)?;
    let text = |lang| {
        format!(
            "{} {}: {place} {}",
            errors::render(keys::RECORD_INVALID, &[], lang),
            errors::render(keys::RECORD_OUTSIDE_FILTER, &[], lang),
            record_source(plan, Some(line), Some(field), lang)
        )
    };
    Ok(other_error(text(Lang::Ru), text(Lang::En)))
}

fn cancelled(core: &SetCore) -> HostError {
    let def = core.def();
    let synonym = crate::types::synonym(&def.synonym, &def.name, &core.app.model().lang_code);
    let text = |lang| {
        format!(
            "{}: \"{synonym}\"!",
            errors::render(keys::INFOREG_CANT_SAVE, &[], lang)
        )
    };
    other_error(text(Lang::Ru), text(Lang::En))
}

pub(super) fn read(ex: &mut Execution, set: &RecordSet) -> CallResult<Value> {
    let core = &set.core;
    let plan = Plan::new(core, "Прочитать", false)?;
    crate::read::admit_data_read(ex)?;
    let filter = set.filter(ex)?;
    let key = plan.filter_key(&filter, "Прочитать")?;
    let txn = CallTxn::of_call(ex);
    let rows: Vec<Row> = {
        let mut view = crate::read::read_view(ex, txn.as_deref(), plan.app)?;
        let rd = view.txn();
        let iter = plan
            .rows(&*rd, &key)
            .map_err(|e| crate::read::store_call_error(ex, e))?;
        let mut rows = Vec::new();
        let mut bytes = 0u64;
        for item in iter {
            let (_, row) = item.map_err(|e| crate::read::store_call_error(ex, e))?;
            bytes = bytes.saturating_add(budget::stored_row(&row));
            budget::reserve(ex, bytes)?;
            rows.push(row);
        }
        rows
    };
    let mut records = Vec::with_capacity(rows.len());
    for row in &rows {
        ex.charge(1)?;
        let mut cells = Vec::with_capacity(plan.cols.len());
        for (col, fid) in plan.fields.iter().enumerate() {
            let cell = row
                .get(usize::from(fid.0))
                .ok_or_else(|| internal("no cell of the row"))?;
            cells.push(plan.slot(col, cell)?);
        }
        records.push(Record::new(ex, core, plan.cols.clone(), cells)?);
    }
    set.replace_records(records);
    core.modified.set(false);
    Ok(Value::Undefined)
}

pub(super) fn get_record(
    ex: &mut Execution,
    core: &Rc<SetCore>,
    values: &[(usize, Value)],
) -> CallResult<Option<Rc<Record>>> {
    let plan = Plan::new(core, "Получить", false)?;
    crate::read::admit_data_read(ex)?;
    let mut key = plan.area()?;
    for &col in &plan.dims {
        let v = values
            .iter()
            .find(|(c, _)| *c == col)
            .map(|(_, v)| v)
            .ok_or_else(|| internal("no value of a dimension"))?;
        key.push(to_field(plan.app, plan.field(col)?, v)?);
    }
    let txn = CallTxn::of_call(ex);
    let row: Option<Row> = {
        let mut view = crate::read::read_view(ex, txn.as_deref(), plan.app)?;
        let rd = view.txn();
        let mut iter = plan
            .rows(&*rd, &key)
            .map_err(|e| crate::read::store_call_error(ex, e))?;
        match iter.next() {
            None => None,
            Some(item) => {
                let (_, row) = item.map_err(|e| crate::read::store_call_error(ex, e))?;
                budget::reserve(ex, budget::stored_row(&row))?;
                Some(row)
            }
        }
    };
    let Some(row) = row else {
        return Ok(None);
    };
    let mut cells = Vec::with_capacity(plan.cols.len());
    for (col, fid) in plan.fields.iter().enumerate() {
        let cell = row
            .get(usize::from(fid.0))
            .ok_or_else(|| internal("no cell of the row"))?;
        cells.push(plan.slot(col, cell)?);
    }
    Ok(Some(Record::new(ex, core, plan.cols.clone(), cells)?))
}

fn replace_mode(ex: &mut Execution, core: &SetCore, args: &[Arg]) -> CallResult<bool> {
    let v = arg_value(ex, args, 0)?;
    match v {
        Value::Undefined => Ok(true),
        Value::Bool(b) => Ok(b),
        other => match bsl_stdlib::enums::as_enum_value(&other) {
            Some(e) if e.name_en() == "Replace" => Ok(true),
            Some(e) if e.name_en() == "Append" => Ok(false),
            Some(e)
                if platform_catalog::catalog().find_type("РежимЗамещения")
                    == Some(e.type_idx()) =>
            {
                Err(core
                    .refusal(&format!("Записать: режим замещения {}", e.name_ru()))
                    .into())
            }
            _ => Err(type_mismatch(1).into()),
        },
    }
}

pub(super) fn write(ex: &mut Execution, set: &Rc<RecordSet>, args: &[Arg]) -> CallResult<Value> {
    let core = set.core.clone();
    let app = core.app.clone();
    let replace = replace_mode(ex, &core, args)?;
    let plan = Plan::new(&core, "Записать", true)?;
    if !call_ctx(ex).is_some_and(|c| c.sess.user().no_users) {
        return Err(HostError::not_implemented("P6", "application write security provider").into());
    }
    let txn =
        CallTxn::of_call(ex).ok_or_else(|| internal("the write of a record set outside a call"))?;
    txn.check().map_err(|e| e.host())?;
    let modified = core.modified.get();
    txn.begin();
    let ended = match steps(ex, &plan, &txn, set, replace) {
        Ok(()) => txn.commit(&*app).map_err(|e| e.call_error(ex)),
        Err(e) => {
            let _ = txn.rollback(&*app);
            Err(e)
        }
    };
    if !matches!(ended, Ok(Commit::Committed | Commit::Inner)) {
        core.modified.set(modified);
    }
    ended.map(|_| Value::Undefined)
}

enum Stored {
    Done,
    Duplicate { record: usize, line: Option<usize> },
}

fn steps(
    ex: &mut Execution,
    plan: &Plan<'_>,
    txn: &Rc<CallTxn>,
    set: &Rc<RecordSet>,
    replace: bool,
) -> CallResult<()> {
    let core = &set.core;
    let app = plan.app;
    let exchange = set.exchange()?;
    let auto_before = exchange.auto_fill();
    let source: ObjRef = set.clone();
    let owner = Owner {
        app,
        def: core.def(),
        module: set.module(),
        source: Value::Object(source),
        category: "RecordSet",
    };
    let replacing = Place::temp(Value::Bool(replace));
    let cancel = Place::temp(Value::Bool(false));
    let params = [Param::Ref(cancel.clone()), Param::Ref(replacing.clone())];
    if events::run_for(ex, &owner, Event::BeforeWrite, &params, Some(&cancel))? {
        return Err(cancelled(core).into());
    }
    let filter = set.filter(ex)?;
    let records = set.records();
    let rows = rows_of(ex, plan, &filter, &records)?;
    let key = plan.filter_key(&filter, "Записать")?;
    let keys = rows
        .iter()
        .map(|r| plan.row_key(r))
        .collect::<Result<Vec<_>, _>>()?;
    let bytes = rows
        .iter()
        .fold(0u64, |n, r| n.saturating_add(budget::stored_row(r)));
    budget::reserve(ex, bytes.saturating_mul(2))?;
    if crate::data_lock::lock_for_write(app, txn, core.def()).is_err() {
        return Err(crate::data_lock::conflict().into());
    }
    let registration =
        crate::changes::Registration::for_write(app, core.obj, &exchange, auto_before)?;
    let mut removed: Vec<Row> = Vec::new();
    let stored = txn.with_write(app, |w| -> StoreResult<Stored> {
        if replace {
            let found = plan
                .rows(&*w, &key)?
                .collect::<StoreResult<Vec<(RowHandle, Row)>>>()?;
            for (h, row) in found {
                w.delete(plan.table, h)?;
                if registration.is_some() {
                    removed.push(row);
                }
            }
        }
        let mut inserted = Vec::with_capacity(rows.len());
        for (i, row) in rows.iter().enumerate() {
            match w.insert(plan.table, row) {
                Ok(h) => inserted.push(h),
                Err(StoreError::UniqueViolation { .. }) => {
                    let taken = plan
                        .rows(&*w, keys.get(i).map_or(&[][..], Vec::as_slice))?
                        .map(|item| item.map(|(h, _)| h))
                        .collect::<StoreResult<Vec<RowHandle>>>()?;
                    let line = taken
                        .iter()
                        .find_map(|h| inserted.iter().position(|x| x == h))
                        .map(|_| i + 1);
                    return Ok(Stored::Duplicate { record: i, line });
                }
                Err(e) => return Err(e),
            }
        }
        Ok(Stored::Done)
    });
    match stored {
        Ok(Stored::Done) => {}
        Ok(Stored::Duplicate { record, line }) => {
            let rec = records
                .get(record)
                .ok_or_else(|| internal("no such record"))?;
            return Err(duplicate(ex, plan, rec, line)?.into());
        }
        Err(e) => return Err(e.call_error(ex)),
    }
    if let Some(reg) = &registration {
        let keys = change_keys(plan, reg, &filter, &rows, &removed)?;
        let mut nodes = Vec::new();
        let done = txn.with_write(app, |w| {
            nodes = reg.nodes(&*w)?;
            for k in &keys {
                reg.apply(w, &nodes, k)?;
            }
            Ok(())
        });
        done.map_err(|e| e.call_error(ex))?;
        reg.done(&nodes);
    }
    crate::commit_guard::guard(app, txn, core.obj);
    core.modified.set(false);
    let cancel = Place::temp(Value::Bool(false));
    let params = [Param::Ref(cancel.clone()), Param::Ref(replacing)];
    if events::run_for(ex, &owner, Event::OnWrite, &params, Some(&cancel))? {
        return Err(cancelled(core).into());
    }
    Ok(())
}

fn change_keys(
    plan: &Plan<'_>,
    reg: &crate::changes::Registration<'_>,
    filter: &Filter,
    rows: &[Row],
    removed: &[Row],
) -> Result<Vec<Vec<(usize, FieldValue)>>, HostError> {
    let changes = &reg.changes;
    let sources = changes.key_sources(plan.ti)?;
    let mut probe: Row = vec![FieldValue::Null; plan.ti.fields.len()];
    let mut full = true;
    for src in &sources {
        let item = filter.items.iter().find_map(|it| {
            let fid = plan.fields.get(it.col)?;
            (fid == src)
                .then(|| it.used_value().map(|v| (it.col, v)))
                .flatten()
        });
        match item {
            Some((col, v)) => {
                if let Some(slot) = probe.get_mut(usize::from(src.0)) {
                    *slot = to_field(plan.app, plan.field(col)?, &v)?;
                }
            }
            None => full = false,
        }
    }
    if full {
        return Ok(vec![changes.key_of_row(plan.ti, &probe)?]);
    }
    let mut keys: Vec<Vec<(usize, FieldValue)>> = Vec::with_capacity(rows.len());
    for row in rows {
        let k = changes.key_of_row(plan.ti, row)?;
        if !keys.contains(&k) {
            keys.push(k);
        }
    }
    for row in removed {
        let k = changes.key_of_row(plan.ti, row)?;
        if !keys.contains(&k) {
            return Err(plan
                .core
                .refusal("Записать: регистрация удалённых записей при неполном отборе"));
        }
    }
    Ok(keys)
}

fn rows_of(
    ex: &mut Execution,
    plan: &Plan<'_>,
    filter: &Filter,
    records: &[Rc<Record>],
) -> CallResult<Vec<Row>> {
    let mut hints: Vec<Option<&FieldDef>> = vec![None; plan.ti.fields.len()];
    for (col, fid) in plan.fields.iter().enumerate() {
        if let Some(slot) = hints.get_mut(usize::from(fid.0)) {
            *slot = Some(plan.field(col)?);
        }
    }
    let mut rows = Vec::with_capacity(records.len());
    for (i, rec) in records.iter().enumerate() {
        ex.charge(1)?;
        if plan.periodic
            && let Some(&col) = plan.dims.first()
        {
            match rec.value(col)? {
                Value::Date(d) if d != bsl_prim::BslDate::EMPTY => {
                    let p = d.parts();
                    if (p.hour, p.minute, p.second) != (0, 0, 0) {
                        return Err(plan.core.refusal("Записать: период со временем").into());
                    }
                }
                _ => return Err(plan.core.refusal("Записать: пустой период").into()),
            }
        }
        for item in &filter.items {
            let Some(want) = item.used_value() else {
                continue;
            };
            let f = plan.field(item.col)?;
            let have = rec.value(item.col)?;
            if !same(
                &to_field(plan.app, f, &want)?,
                &to_field(plan.app, f, &have)?,
            ) {
                let name = plan.cols.get(item.col).map_or("", |c| c.name.as_str());
                return Err(outside_filter(ex, plan, rec, i + 1, name)?.into());
            }
        }
        let mut row = crate::predefined::blank_row(plan.app, plan.ti, &hints);
        for (col, fid) in plan.fields.iter().enumerate() {
            let value = match rec.stored(col) {
                Some(Some(stored)) => stored,
                Some(None) => continue,
                None => to_field(plan.app, plan.field(col)?, &rec.value(col)?)?,
            };
            if let Some(slot) = row.get_mut(usize::from(fid.0)) {
                *slot = value;
            }
        }
        rows.push(row);
    }
    Ok(rows)
}

fn table(
    ex: &mut Execution,
    core: &SetCore,
    cols: &[Column],
    which: &[usize],
    records: &[Rc<Record>],
) -> CallResult<Value> {
    let table = ex.construct_by_name("ТаблицаЗначений", &mut [])?;
    let list = ex.get_member(&table, "Колонки")?;
    for &c in which {
        ex.charge(1)?;
        let col = cols.get(c).ok_or_else(|| internal("no column"))?;
        let ty = column_type(&core.app, ex, core.obj, col)?;
        let mut add = [Arg::Val(Value::from(col.name.as_str())), Arg::Val(ty)];
        ex.call_method(&list, "Добавить", &mut add, true)?;
    }
    for rec in records {
        let row = ex.call_method(&table, "Добавить", &mut [], true)?;
        for &c in which {
            let col = cols.get(c).ok_or_else(|| internal("no column"))?;
            let v = rec.value(c)?;
            ex.set_member(&row, &col.name, v)?;
        }
    }
    Ok(table)
}

pub(super) fn unload(ex: &mut Execution, set: &RecordSet, args: &[Arg]) -> CallResult<Value> {
    let core = &set.core;
    let cols = core.cols()?;
    let records = match arg_value(ex, args, 0)? {
        Value::Undefined => set.records(),
        v => {
            let Some(items) = bsl_stdlib::array_items(&v) else {
                return Err(core.refusal("Выгрузить: строки не массив").into());
            };
            let own = set.records();
            let mut picked = Vec::with_capacity(items.len());
            for item in &items {
                let found = own.iter().find(|r| {
                    item.as_object()
                        .and_then(|o| o.as_any().downcast_ref::<Record>())
                        .is_some_and(|x| std::ptr::eq(x, &***r))
                });
                match found {
                    Some(r) => picked.push(r.clone()),
                    None => return Err(core.refusal("Выгрузить: строка не запись набора").into()),
                }
            }
            picked
        }
    };
    let which: Vec<usize> = match arg_value(ex, args, 1)? {
        Value::Undefined => (0..cols.len()).collect(),
        Value::String(list) => {
            let list = list.to_string_lossy();
            let mut which = Vec::new();
            for name in list.split(',').map(str::trim) {
                let wanted = fold_name(name);
                match cols.iter().position(|c| fold_name(&c.name) == wanted) {
                    Some(i) if !name.is_empty() => which.push(i),
                    _ => {
                        return Err(core.refusal(&format!("Выгрузить: колонка {name}")).into());
                    }
                }
            }
            which
        }
        _ => return Err(type_mismatch(2).into()),
    };
    table(ex, core, &cols, &which, &records)
}

pub(super) fn unload_columns(ex: &mut Execution, set: &RecordSet) -> CallResult<Value> {
    let core = &set.core;
    let cols = core.cols()?;
    let all: Vec<usize> = (0..cols.len()).collect();
    table(ex, core, &cols, &all, &[])
}

pub(super) fn load(ex: &mut Execution, set: &RecordSet, args: &[Arg]) -> CallResult<Value> {
    let core = &set.core;
    let cols = core.cols()?;
    let table = arg_value(ex, args, 0)?;
    let is_table = platform_catalog::catalog()
        .find_type("ТаблицаЗначений")
        .is_some_and(|t| table.type_id() == bsl_vm::TypeId::of_catalog(t));
    if !is_table {
        return Err(type_mismatch(1).into());
    }
    let list = ex.get_member(&table, "Колонки")?;
    let count = |ex: &mut Execution, v: &Value| -> CallResult<usize> {
        match ex.call_method(v, "Количество", &mut [], true)? {
            Value::Number(n) => Ok(usize::try_from(n.trunc_to_i64_saturating()).unwrap_or(0)),
            _ => Err(internal("a count that is not a number").into()),
        }
    };
    let mut matched: Vec<(String, usize)> = Vec::new();
    for i in 0..count(ex, &list)? {
        let mut at = [Arg::Val(Value::from(i64::try_from(i).unwrap_or(i64::MAX)))];
        let column = ex.call_method(&list, "Получить", &mut at, true)?;
        let Value::String(name) = ex.get_member(&column, "Имя")? else {
            continue;
        };
        let name = name.to_string_lossy();
        let wanted = fold_name(&name);
        if let Some(c) = cols
            .iter()
            .position(|c| fold_name(&c.name) == wanted && c.std != Some(StdAttr::LineNumber))
        {
            matched.push((name, c));
        }
    }
    let mut records = Vec::new();
    for r in 0..count(ex, &table)? {
        let mut at = [Arg::Val(Value::from(i64::try_from(r).unwrap_or(i64::MAX)))];
        let row = ex.call_method(&table, "Получить", &mut at, true)?;
        let rec = Record::blank(ex, core, cols.clone())?;
        for (name, c) in &matched {
            let v = ex.get_member(&row, name)?;
            rec.assign(ex, *c, v)?;
        }
        records.push(rec);
    }
    set.replace_records(records);
    core.modified.set(true);
    Ok(Value::Undefined)
}
