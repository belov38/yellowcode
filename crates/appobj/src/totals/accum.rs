use super::{Change, Edit, Movement, RecordKind, TotalsPlan, refusal};
use crate::Application;
use crate::model::{FieldRole, ObjDef};
use crate::read::store_error;
use bsl_prim::Number;
use bsl_vm::HostError;
use ibschema::fields::FieldValue;
use ibschema::logical::TypeCode;
use ibschema::names::{StdField, role};
use ibschema::storage::{FieldId, IndexId, StorageMap, TableId, TableInfo};
use ibschema::store::{Direction, ReadTxn, RowHandle};
use ibschema::totals::{
    CURRENT_TOTALS_PERIOD, MIN_PERIOD_INITIAL, SPLITTER_FILE_MODE, USE_TOTALS_ON,
};
use ibschema::values::Uuid16;
use ibschema::{DateTime, Numeric, Row};
use query_ir::convert::{number_from_q, number_to_q};
use std::ops::Bound;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Balance,
    Turnovers,
}

struct Layout {
    table: TableId,
    period: FieldId,
    dims: Vec<FieldId>,
    res: Vec<(FieldId, u32, u32)>,
    splitter: Option<FieldId>,
    index: IndexId,
    key: Vec<KeyPart>,
}

#[derive(Clone, Copy)]
enum KeyPart {
    Period,
    Dim(usize),
    Splitter,
}

struct Touched {
    period: DateTime,
    dims: Vec<FieldValue>,
    found: Option<(RowHandle, Row)>,
    delta: Vec<Number>,
}

pub(crate) fn service(app: &Application, def: &ObjDef, name: &str) -> Option<TableId> {
    app.model()
        .storage_of(def.id)?
        .service
        .iter()
        .find(|(r, _)| r == name)
        .map(|(_, t)| *t)
}

pub(crate) fn std_value<'r>(
    info: &TableInfo,
    row: &'r Row,
    std: StdField,
) -> Option<&'r FieldValue> {
    row.get(usize::from(info.field_by_std(std)?.0))
}

fn put(row: &mut Row, f: FieldId, v: FieldValue) {
    if let Some(slot) = row.get_mut(usize::from(f.0)) {
        *slot = v;
    }
}

fn is_zero(n: &Numeric) -> bool {
    n.digits().iter().all(|d| *d == 0)
}

fn settings_row(
    view: &dyn ReadTxn,
    map: &StorageMap,
    def: &ObjDef,
    t: TableId,
) -> Result<Option<Row>, HostError> {
    let info = map.table(t);
    let mine = |row: &Row| matches!(std_value(info, row, StdField::RegId), Some(FieldValue::Uuid(u)) if *u == def.md);
    let mut found = None;
    for r in view.scan(t).map_err(|e| store_error(&e))? {
        let (_, row) = r.map_err(|e| store_error(&e))?;
        if mine(&row) {
            if found.is_some() {
                return Err(refusal(&def.name, "несколько строк настроек итогов"));
            }
            found = Some(row);
        }
    }
    Ok(found)
}

fn settings(
    app: &Application,
    view: &dyn ReadTxn,
    def: &ObjDef,
    kind: Kind,
) -> Result<Option<DateTime>, HostError> {
    let map = app.store().storage();
    let name = &def.name;
    let t = service(app, def, role::ACCUMULATION_REGISTER_TOTALS_OPTIONS)
        .ok_or_else(|| refusal(name, "нет таблицы настроек итогов"))?;
    let info = map.table(t);
    let row = settings_row(view, map, def, t)?
        .ok_or_else(|| refusal(name, "нет строки настроек итогов"))?;
    let on = Numeric::from_i64(i64::from(USE_TOTALS_ON));
    if !matches!(std_value(info, &row, StdField::UseTotals), Some(FieldValue::Number(n)) if n.eq_value(&on))
    {
        return Err(refusal(name, "использование итогов выключено"));
    }
    if !matches!(
        std_value(info, &row, StdField::ActualPeriod),
        Some(FieldValue::Bool(true))
    ) {
        return Err(refusal(name, "нет текущих итогов"));
    }
    if !matches!(std_value(info, &row, StdField::MinPeriod), Some(FieldValue::Date(d)) if *d == MIN_PERIOD_INITIAL)
    {
        return Err(refusal(name, "минимальный период итогов не начальный"));
    }
    let calculated = match std_value(info, &row, StdField::Period) {
        Some(FieldValue::Date(d)) if *d != DateTime::EMPTY => Some(*d),
        Some(FieldValue::Date(_) | FieldValue::Null) | None => None,
        Some(_) => return Err(refusal(name, "период рассчитанных итогов не дата")),
    };
    match (kind, calculated) {
        (Kind::Balance, None) => return Err(refusal(name, "итоги не рассчитаны")),
        (Kind::Balance, Some(p)) if p != month_start(p) => {
            return Err(refusal(name, "период рассчитанных итогов не начало месяца"));
        }
        (Kind::Turnovers, Some(_)) => {
            return Err(refusal(name, "рассчитанные итоги оборотов"));
        }
        _ => {}
    }
    if let Some(t) = service(app, def, role::ACCUMULATION_REGISTERS_AGGREGATES_OPTIONS) {
        let info = map.table(t);
        let row = settings_row(view, map, def, t)?
            .ok_or_else(|| refusal(name, "нет строки настроек агрегатов"))?;
        if !matches!(
            std_value(info, &row, StdField::AggMode),
            Some(FieldValue::Bool(false))
        ) {
            return Err(refusal(name, "режим агрегатов"));
        }
    }
    Ok(calculated)
}

fn field_by_md(info: &TableInfo, md: Uuid16) -> Option<FieldId> {
    let i = info.fields.iter().position(|f| f.md == Some(md))?;
    Some(FieldId(u16::try_from(i).ok()?))
}

impl Layout {
    fn new(map: &StorageMap, def: &ObjDef, table: TableId) -> Result<Layout, HostError> {
        let name = &def.name;
        let info = map.table(table);
        if !info.separators.is_empty() {
            return Err(refusal(name, "разделённая таблица итогов"));
        }
        let period = info
            .field_by_std(StdField::Period)
            .ok_or_else(|| refusal(name, "таблица итогов без периода"))?;
        let splitter = info.field_by_std(StdField::Splitter);
        let mut dims = Vec::new();
        let mut res = Vec::new();
        for f in &def.fields {
            let Some(md) = f.md else { continue };
            match f.role {
                FieldRole::Dimension => {
                    dims.push(field_by_md(info, md).ok_or_else(|| {
                        refusal(name, &format!("измерение {} не в итогах", f.name))
                    })?)
                }
                FieldRole::Resource => {
                    let not_number = || refusal(name, &format!("ресурс {} не число", f.name));
                    let id = field_by_md(info, md)
                        .ok_or_else(|| refusal(name, &format!("ресурс {} не в итогах", f.name)))?;
                    let types = info.field(id).map(|f| f.types.as_slice());
                    let Some([t]) = types else {
                        return Err(not_number());
                    };
                    if t.code != TypeCode::N {
                        return Err(not_number());
                    }
                    res.push((id, t.length, t.precision));
                }
                _ => {}
            }
        }
        let keyed = 1 + dims.len() + usize::from(splitter.is_some());
        if info.fields.len() != keyed + res.len() {
            return Err(refusal(name, "в таблице итогов другие поля"));
        }
        let part = |f: FieldId| {
            if f == period {
                Some(KeyPart::Period)
            } else if Some(f) == splitter {
                Some(KeyPart::Splitter)
            } else {
                dims.iter().position(|d| *d == f).map(KeyPart::Dim)
            }
        };
        let index = info.indexes.iter().enumerate().find(|(_, ix)| {
            ix.unique
                && !ix.absent
                && ix.prefix == 0
                && ix.fields.len() == keyed
                && ix.fields.iter().all(|f| part(*f).is_some())
        });
        let Some((k, ix)) = index else {
            return Err(refusal(name, "нет уникального индекса итогов"));
        };
        let key: Vec<KeyPart> = ix.fields.iter().filter_map(|f| part(*f)).collect();
        let splitter_at = key.iter().position(|p| matches!(p, KeyPart::Splitter));
        if splitter_at.is_some_and(|i| i + 1 != key.len()) {
            return Err(refusal(name, "разделитель итогов не последний в индексе"));
        }
        let index = u16::try_from(k)
            .map(IndexId::Named)
            .map_err(|_| refusal(name, "нет уникального индекса итогов"))?;
        Ok(Layout {
            table,
            period,
            dims,
            res,
            splitter,
            index,
            key,
        })
    }

    fn find(
        &self,
        view: &dyn ReadTxn,
        name: &str,
        period: DateTime,
        dims: &[FieldValue],
    ) -> Result<Option<(RowHandle, Row)>, HostError> {
        let prefix: Vec<FieldValue> = self
            .key
            .iter()
            .filter_map(|p| match p {
                KeyPart::Period => Some(FieldValue::Date(period)),
                KeyPart::Dim(i) => dims.get(*i).cloned(),
                KeyPart::Splitter => None,
            })
            .collect();
        let rows = view
            .range(
                self.table,
                self.index,
                Bound::Included(&prefix),
                Bound::Included(&prefix),
                Direction::Forward,
            )
            .map_err(|e| store_error(&e))?;
        let mut found = None;
        for r in rows {
            let (h, row) = r.map_err(|e| store_error(&e))?;
            let same = row.get(usize::from(self.period.0)) == Some(&FieldValue::Date(period))
                && self
                    .dims
                    .iter()
                    .zip(dims)
                    .all(|(f, v)| row.get(usize::from(f.0)) == Some(v));
            if !same {
                return Err(refusal(name, "строка итогов с ключом, равным по индексу"));
            }
            if let Some(s) = self.splitter
                && !matches!(row.get(usize::from(s.0)), Some(FieldValue::Number(n)) if is_zero(n))
            {
                return Err(refusal(name, "строка итогов с разделителем"));
            }
            if found.is_some() {
                return Err(refusal(name, "две строки итогов одного ключа"));
            }
            found = Some((h, row));
        }
        Ok(found)
    }

    fn key_less(&self, a: &Touched, b: &Touched) -> Option<bool> {
        for part in &self.key {
            let ord = match part {
                KeyPart::Period => a.period.cmp(&b.period),
                KeyPart::Dim(i) => match (a.dims.get(*i)?, b.dims.get(*i)?) {
                    (
                        FieldValue::Ref { table: ta, id: ia },
                        FieldValue::Ref { table: tb, id: ib },
                    ) if ta == tb => ia.cmp(ib),
                    _ => return None,
                },
                KeyPart::Splitter => std::cmp::Ordering::Equal,
            };
            if ord != std::cmp::Ordering::Equal {
                return Some(ord == std::cmp::Ordering::Less);
            }
        }
        Some(false)
    }

    fn resources(&self, name: &str, row: &Row) -> Result<Vec<Number>, HostError> {
        self.res
            .iter()
            .map(|(f, _, _)| match row.get(usize::from(f.0)) {
                Some(FieldValue::Number(n)) => Ok(number_to_q(n)),
                _ => Err(refusal(name, "ресурс итогов не число")),
            })
            .collect()
    }

    fn stored(&self, name: &str, k: usize, v: &Number) -> Result<Numeric, HostError> {
        let overflow = || refusal(name, "переполнение ресурса итогов");
        let (_, length, precision) = self.res.get(k).copied().ok_or_else(overflow)?;
        let n = number_from_q(v).ok_or_else(overflow)?;
        let digits = n.digits();
        let (int, frac) = digits.split_at(digits.len().saturating_sub(n.scale() as usize));
        let int = int.iter().skip_while(|d| **d == 0).count();
        let frac = frac.iter().rposition(|d| *d != 0).map_or(0, |i| i + 1);
        if int > length.saturating_sub(precision) as usize || frac > precision as usize {
            return Err(overflow());
        }
        Ok(n)
    }

    fn new_row(&self, info: &TableInfo, t: &Touched, res: Vec<Numeric>) -> Row {
        let mut row = vec![FieldValue::Null; info.fields.len()];
        put(&mut row, self.period, FieldValue::Date(t.period));
        for (f, v) in self.dims.iter().zip(&t.dims) {
            put(&mut row, *f, v.clone());
        }
        for ((f, _, _), v) in self.res.iter().zip(res) {
            put(&mut row, *f, FieldValue::Number(v));
        }
        if let Some(s) = self.splitter {
            let zero = Numeric::from_i64(i64::from(SPLITTER_FILE_MODE));
            put(&mut row, s, FieldValue::Number(zero));
        }
        row
    }
}

fn month_start(d: DateTime) -> DateTime {
    DateTime {
        day: 1,
        hour: 0,
        minute: 0,
        second: 0,
        ..d
    }
}

fn next_month(d: DateTime) -> DateTime {
    if d.month >= 12 {
        DateTime {
            year: d.year.saturating_add(1),
            month: 1,
            ..d
        }
    } else {
        DateTime {
            month: d.month + 1,
            ..d
        }
    }
}

fn periods(kind: Kind, calc: Option<DateTime>, period: DateTime) -> Vec<DateTime> {
    let Some(calc) = calc.filter(|_| kind == Kind::Balance) else {
        return vec![match kind {
            Kind::Balance => CURRENT_TOTALS_PERIOD,
            Kind::Turnovers => month_start(period),
        }];
    };
    let mut out = vec![CURRENT_TOTALS_PERIOD];
    let mut p = next_month(month_start(period));
    while p <= calc {
        out.push(p);
        p = next_month(p);
    }
    out
}

fn check(name: &str, kind: Kind, layout: &Layout, m: &Movement) -> Result<(), HostError> {
    if !m.active {
        return Err(refusal(name, "неактивное движение"));
    }
    let kind_ok = match kind {
        Kind::Balance => matches!(m.kind, RecordKind::Receipt | RecordKind::Expense),
        Kind::Turnovers => m.kind == RecordKind::Turnover,
    };
    if !kind_ok {
        return Err(refusal(name, "вид движения не регистра"));
    }
    if m.dims.len() != layout.dims.len() || m.res.len() != layout.res.len() {
        return Err(refusal(name, "движение не регистра"));
    }
    Ok(())
}

pub(super) fn plan(
    app: &Application,
    view: &dyn ReadTxn,
    def: &ObjDef,
    ch: &Change<'_>,
) -> Result<TotalsPlan, HostError> {
    let name = &def.name;
    let (kind, table) = match (
        service(app, def, role::TOTALS),
        service(app, def, role::TURNOVERS),
    ) {
        (Some(t), None) => (Kind::Balance, t),
        (None, Some(t)) => (Kind::Turnovers, t),
        _ => return Err(refusal(name, "регистр без таблицы итогов")),
    };
    let calc = settings(app, view, def, kind)?;
    let map = app.store().storage();
    let layout = Layout::new(map, def, table)?;
    let mut touched: Vec<Touched> = Vec::new();
    let steps = ch
        .removed
        .iter()
        .map(|m| (m, true))
        .chain(ch.added.iter().map(|m| (m, false)));
    for (m, removed) in steps {
        check(name, kind, &layout, m)?;
        let negative = removed != (m.kind == RecordKind::Expense);
        let delta: Vec<Number> = m
            .res
            .iter()
            .map(|r| {
                let n = number_to_q(r);
                if negative { n.neg() } else { n }
            })
            .collect();
        for (k, p) in periods(kind, calc, m.period).into_iter().enumerate() {
            let i = match touched
                .iter()
                .position(|t| t.period == p && t.dims == m.dims)
            {
                Some(i) => i,
                None => {
                    let found = layout.find(view, name, p, &m.dims)?;
                    touched.push(Touched {
                        period: p,
                        dims: m.dims.clone(),
                        found,
                        delta: vec![Number::zero(); layout.res.len()],
                    });
                    touched.len() - 1
                }
            };
            if touched.get(i).is_some_and(|t| t.found.is_none()) {
                if removed {
                    return Err(refusal(name, "нет строки итогов снимаемого движения"));
                }
                if k > 0 {
                    let current = touched.iter().any(|t| {
                        t.period == CURRENT_TOTALS_PERIOD && t.dims == m.dims && t.found.is_some()
                    });
                    return Err(refusal(
                        name,
                        if current {
                            "нет месячной строки итогов"
                        } else {
                            "новое сочетание измерений задним числом"
                        },
                    ));
                }
            }
            if let Some(t) = touched.get_mut(i) {
                for (s, d) in t.delta.iter_mut().zip(&delta) {
                    *s = s.add(d);
                }
            }
        }
    }
    let info = map.table(table);
    let mut out = TotalsPlan::default();
    let mut inserts: Vec<&Touched> = Vec::new();
    for t in &touched {
        if t.delta.iter().all(Number::is_zero) {
            out.commit_refusal
                .get_or_insert_with(|| format!("итоги {name}: строка итогов без изменения"));
        }
        let base = match &t.found {
            Some((_, row)) => layout.resources(name, row)?,
            None => vec![Number::zero(); layout.res.len()],
        };
        let new: Vec<Numeric> = base
            .iter()
            .zip(&t.delta)
            .enumerate()
            .map(|(k, (b, d))| layout.stored(name, k, &b.add(d)))
            .collect::<Result<_, _>>()?;
        match &t.found {
            Some((h, row)) => {
                let mut values = row.clone();
                for ((f, _, _), v) in layout.res.iter().zip(new) {
                    put(&mut values, *f, FieldValue::Number(v));
                }
                out.edits.push(Edit::Update {
                    table,
                    row: *h,
                    values,
                });
            }
            None => {
                inserts.push(t);
                out.edits.push(Edit::Insert {
                    table,
                    values: layout.new_row(info, t, new),
                });
            }
        }
    }
    if inserts.len() > 1
        && !inserts
            .windows(2)
            .all(|w| matches!(w, [a, b] if layout.key_less(a, b) == Some(true)))
    {
        out.commit_refusal
            .get_or_insert_with(|| format!("итоги {name}: несколько новых строк итогов"));
    }
    Ok(out)
}
